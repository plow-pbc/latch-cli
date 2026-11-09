import Foundation
import Testing
@testable import PlowLatchUI

/// Install Update's `latch update` outlives the app: the installer quits
/// the app partway through. A stand-in for latch records how it was
/// started, quits the app that started it (a `StreamHolder`, which stops
/// what it ran on SIGTERM as the app does when it quits), and only then
/// prints its answer and finishes.
struct DetachedUpdateTests {
    @Test func theUpdateFinishesAfterTheAppQuits() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "latch-update-\(UUID().uuidString.prefix(8))")
        let state = folder.appending(path: "state")
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        try """
        #!/bin/sh
        set -e
        seen='\(folder.path)'
        if [ -p /dev/stdout ]; then echo pipe > "$seen/stdout"; else echo file > "$seen/stdout"; fi
        [ "$(ps -o pgid= -p $$ | tr -d ' ')" = "$$" ] && echo own > "$seen/group" || echo app > "$seen/group"
        app=$PPID
        kill -TERM "$app"
        while kill -0 "$app" 2>/dev/null; do sleep 0.1; done
        sleep 0.3
        echo '{"updated":true}'
        echo done > "$seen/finished"
        """.write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)

        let app = try HeldStreams(latch: latch, state: state.path, ["update"])
        defer { app.stop() }
        await waitUntil(within: .seconds(10)) { FileManager.default.fileExists(atPath: folder.appending(path: "finished").path) }
        let read = { (name: String) in (try? String(contentsOf: folder.appending(path: name), encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) }
        #expect(!app.holder.isRunning)
        #expect(read("finished") == "done")
        #expect(read("stdout") == "file")
        #expect(read("group") == "own")
        #expect(read("state/app-update/out.json") == #"{"updated":true}"#)
    }

    /// An update that never ends fails after the client's wait, and the
    /// next one stops it, and what it started, before running again.
    @Test func aHungUpdateFailsAndTheNextStopsIt() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "latch-update-\(UUID().uuidString.prefix(8))")
        let state = folder.appending(path: "state")
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        // The first run hangs, with a child of its own; the next answers.
        try """
        #!/bin/sh
        seen='\(folder.path)'
        if [ -e "$seen/hung" ]; then echo '{"updated":true}'; exit 0; fi
        echo $$ > "$seen/hung"
        sleep 600 &
        echo $! > "$seen/child"
        wait
        """.write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)
        let client = LiveLatchClient(executable: latch, stateDir: state.path, updateWithin: .seconds(2))

        await #expect(throws: LatchError.self) { _ = try await client.update() }
        let pid = { (name: String) in pid_t((try? String(contentsOf: folder.appending(path: name), encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "") }
        let hung = try #require(pid("hung")), child = try #require(pid("child"))
        #expect(kill(hung, 0) == 0 && kill(child, 0) == 0)

        #expect(try await client.update() == UpdateResult(updated: true))
        // Both killed: the first wait reaps the run, launchd its child.
        await waitUntil(within: .seconds(10)) { kill(hung, 0) != 0 && kill(child, 0) != 0 }
    }

    /// latch's error from an update whose end the app didn't see is told
    /// once, at the next launch; a run still going leaves nothing to tell.
    @Test func anUnseenFailureIsToldOnce() async throws {
        let state = FileManager.default.temporaryDirectory.appending(path: "latch-update-\(UUID().uuidString.prefix(8))")
        let folder = state.appending(path: "app-update")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: state) }
        let client = LiveLatchClient(executable: URL(filePath: "/usr/bin/false"), stateDir: state.path)
        try Data().write(to: folder.appending(path: "out.json"))
        try Data().write(to: folder.appending(path: "err.log"))
        #expect(await client.unseenUpdateFailure() == nil)

        try #"{"error":{"code":"install_failed","message":"v0.1.0-168's install-binary.sh failed"}}"#
            .write(to: folder.appending(path: "err.log"), atomically: true, encoding: .utf8)
        #expect(await client.unseenUpdateFailure()?.code == "install_failed")
        #expect(await client.unseenUpdateFailure() == nil)
    }

    /// latch starts blocking no signal, whatever the thread that starts it
    /// blocks (Swift's and Dispatch's block most): with SIGCHLD blocked,
    /// latch never hears its installer end and waits for it forever, and
    /// with SIGTERM blocked nothing short of SIGKILL stops it. Perl, not
    /// sh, stands in for latch: a shell unblocks them as it starts.
    @Test func theUpdateBlocksNoSignals() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "latch-update-\(UUID().uuidString.prefix(8))")
        let state = folder.appending(path: "state")
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        try """
        #!/usr/bin/perl
        use POSIX;
        my $blocked = POSIX::SigSet->new;
        sigprocmask(SIG_BLOCK, undef, $blocked);
        open(my $seen, ">", "\(folder.path)/blocked");
        print $seen join(" ", grep { $blocked->ismember($_) } (SIGCHLD, SIGTERM));
        print '{"updated":true}';
        """.write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)

        _ = try await LiveLatchClient(executable: latch, stateDir: state.path).update()
        #expect(try String(contentsOf: folder.appending(path: "blocked"), encoding: .utf8) == "")
    }
}

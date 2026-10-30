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
}

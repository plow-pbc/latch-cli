import Foundation
import Testing
@testable import PlowLatchUI

/// Against a real latch and a host it serves, named by the environment:
/// `LATCH_LIVE_LATCH`, `LATCH_LIVE_STATE` (the host's state folder), and
/// `LATCH_LIVE_CALLER_STATE` for another of the owner's machines that
/// calls it; the remote case also `LATCH_LIVE_REMOTE`, a host that machine
/// reads with `--host`. Built only with `LATCH_LIVE_TESTS=1`; a selected
/// test without its inputs fails.
@MainActor
struct LiveTests {
    nonisolated static let env = ProcessInfo.processInfo.environment

    private static func input(_ name: String, sourceLocation: SourceLocation = #_sourceLocation) throws -> String {
        try #require(env[name], "\(name) is required", sourceLocation: sourceLocation)
    }

    @Test func aNewRecordArrivesWhileWatchedAndLatchStopsAfter() async throws {
        let latch = URL(filePath: try Self.input("LATCH_LIVE_LATCH"))
        let state = try Self.input("LATCH_LIVE_STATE")
        let host = try await LiveLatchClient(executable: latch, stateDir: state).localStatus().loggedInAs
        try await recordArrives(latch: latch, state: state, feedHost: nil, host: try #require(host))
    }

    /// The app killed outright while it watches its host: its streams'
    /// latches end, and the host it started keeps serving.
    @Test func killingTheAppEndsItsStreamsAndLeavesTheHost() async throws {
        let latch = URL(filePath: try Self.input("LATCH_LIVE_LATCH"))
        let state = try Self.input("LATCH_LIVE_STATE")
        let client = LiveLatchClient(executable: latch, stateDir: state)
        let before = try await client.localStatus().thisMachine.hosting
        try #require(before.ready, "LATCH_LIVE_STATE's host must be serving")
        let app = try HeldStreams(latch: latch, state: state, ["status", "audit"])
        defer { app.stop() }
        func streams() -> Int { latchRuns(on: state, doing: "status --local --follow") + latchRuns(on: state, doing: "audit --follow") }
        await waitUntil { streams() == 2 }
        app.killApp()
        await waitUntil { streams() == 0 }
        #expect(streams() == 0)
        let after = try await client.localStatus().thisMachine.hosting
        #expect(after.ready && after.pid == before.pid)
    }

    /// The caller's machine reads the remote host's audit.
    @Test func aRemoteRecordArrivesWhileWatchedAndLatchStopsAfter() async throws {
        let latch = URL(filePath: try Self.input("LATCH_LIVE_LATCH"))
        let remote = try Self.input("LATCH_LIVE_REMOTE")
        try await recordArrives(latch: latch, state: try Self.input("LATCH_LIVE_CALLER_STATE"), feedHost: remote, host: remote)
    }

    private func recordArrives(latch: URL, state: String, feedHost: String?, host: String) async throws {
        let caller = try Self.input("LATCH_LIVE_CALLER_STATE")
        let feed = ActivityFeed(client: LiveLatchClient(executable: latch, stateDir: state), host: feedHost)
        // A short grace, so latch stops soon after the last watcher goes.
        feed.grace = .milliseconds(40)
        feed.start()
        await waitUntil { feed.caughtUp }
        let before = feed.records.count
        let marker = "live-\(UUID().uuidString.prefix(8))"
        _ = try await LatchProcess(executable: latch, arguments: ["--state-dir", caller, "exec", "--host", host,
                                                                   "--", "echo", marker]).output()
        await waitUntil { feed.records.contains { $0.args?.argv?.last == marker } }
        #expect(feed.records.count > before)
        #expect(feed.records.contains { $0.command?.hasSuffix(" echo \(marker)") == true && $0.status == .allowed(nil) })
        feed.stop()
        await waitUntil { latchRuns(on: state, doing: "audit --follow") == 0 }
    }
}

import Foundation
import Testing
@testable import PlowLatchKit

@Test func eventLinesDecode() throws {
    let events = try Fixtures.activityLines.split(separator: "\n").map {
        try Fixtures.decode(LatchEvent.self, String($0))
    }
    #expect(events.count == 4)
    guard case let .snapshot(cursor, host, activity) = events[0] else { Issue.record("not a snapshot"); return }
    #expect(cursor == "c3" && host == .serving && activity.map(\.id) == ["h1", "h2", "h3"])
    #expect(events[1] == .host(cursor: "c4", .stopped))
    #expect(events[3] == .reset)
}

@Test func nanosecondTimestampsParse() throws {
    let date = try #require(LatchJSON.parseTimestamp("2026-10-20T09:12:03.120394857Z"))
    #expect(abs(date.timeIntervalSince1970 - 1792487523.120) < 0.001)
    #expect(LatchJSON.parseTimestamp("2026-10-20T09:20:00Z") != nil)
    #expect(LatchJSON.parseTimestamp("2026-10-20T09:20:00.5+02:00") != nil)
}

@Test func statusAndRulesDecode() throws {
    let status = try Fixtures.decode(LocalStatus.self, Fixtures.statusServing)
    #expect(status.peer?.name == "mac-host" && status.serve.ready && status.shares.count == 2)
    let rules = try Fixtures.decode(RulesShow.self, Fixtures.rules)
    #expect(rules.everyAgent?.revision == "a1b2c3" && rules.agents.first?.agentName == "owner-laptop agent")
}

private func entry(_ id: String) -> ActivityEntry {
    ActivityEntry(id: id, time: .now, caller: "k", op: "exec", decision: "allow", outcome: "ok")
}

@Test func feedShowsNewestFirstAndResumesWithoutDuplicates() {
    var feed = ActivityFeed()
    feed.connecting(resumed: false)
    feed.apply(.snapshot(cursor: "2", host: .serving, activity: [entry("a"), entry("b")]))
    feed.apply(.activity(cursor: "3", entry("c")))
    #expect(feed.entries.map(\.id) == ["c", "b", "a"] && feed.cursor == "3")

    // A resumed connection replays an overlap: nothing repeats or is lost.
    feed.connecting(resumed: true)
    feed.apply(.snapshot(cursor: "4", host: .serving, activity: [entry("c"), entry("d")]))
    feed.apply(.activity(cursor: "4", entry("d")))
    #expect(feed.entries.map(\.id) == ["d", "c", "b", "a"] && feed.cursor == "4")
}

@Test func resetReplacesWhatTheFeedHeld() {
    var feed = ActivityFeed()
    feed.connecting(resumed: false)
    feed.apply(.snapshot(cursor: "2", host: .serving, activity: [entry("a"), entry("b")]))
    feed.connecting(resumed: true)
    feed.apply(.reset)
    feed.apply(.snapshot(cursor: "1", host: .serving, activity: [entry("x")]))
    #expect(feed.entries.map(\.id) == ["x"] && feed.cursor == "1")
}

@Test func feedKeepsOnlyTheNewestEntries() {
    var feed = ActivityFeed()
    feed.apply(.snapshot(cursor: "0", host: .serving, activity: (0..<(ActivityFeed.limit + 10)).map { entry("\($0)") }))
    #expect(feed.entries.count == ActivityFeed.limit)
    #expect(feed.entries.first?.id == "\(ActivityFeed.limit + 9)")
}

// The live client against scripts/stub-latch: real processes, real pipes.

private let stub = URL(filePath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .appending(path: "scripts/stub-latch")

private func stubState() throws -> String {
    let dir = FileManager.default.temporaryDirectory.appending(path: "plow-latch-tests-\(UUID())")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.path
}

private func emit(_ state: String, _ args: String...) throws {
    let process = try Process.run(stub, arguments: ["--state-dir", state, "emit"] + args)
    process.waitUntilExit()
}

@Test func liveClientReadsStatusAndFollowsEvents() async throws {
    let state = try stubState()
    try emit(state, "host", "serving")
    try emit(state, "activity", "file.read", "allow")
    let client = LiveLatchClient(executable: stub, stateDir: state)

    let status = try await client.localStatus()
    #expect(status.serve.ready && status.stateDir == state)

    var events = client.events(after: nil, lifecycleOnly: false).makeAsyncIterator()
    guard case let .snapshot(_, host, activity) = try await events.next() else { Issue.record("no snapshot"); return }
    #expect(host == .serving && activity.map(\.op) == ["file.read"])
    try emit(state, "host", "stopped")
    #expect(try await events.next() == .host(cursor: "3", .stopped))
}

@Test func liveClientSurfacesLatchsError() async throws {
    let state = try stubState()
    let refusing = URL(filePath: state).appending(path: "latch")
    try """
    #!/bin/sh
    echo 'starting' >&2
    echo '{"error":{"code":"not_logged_in","message":"not logged in: run `latch login`"}}' >&2
    exit 3
    """.write(to: refusing, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: refusing.path)
    let client = LiveLatchClient(executable: refusing, stateDir: nil)
    await #expect(throws: LatchError.failed(LatchFailure(code: "not_logged_in", message: "not logged in: run `latch login`"))) {
        _ = try await client.rules()
    }
    await #expect(throws: LatchError.self) {
        for try await _ in client.events(after: nil, lifecycleOnly: true) {}
    }
}

@Test func missingLatchIsUnreachable() async {
    let client = LiveLatchClient(executable: URL(filePath: "/nonexistent/latch"), stateDir: nil)
    do {
        _ = try await client.localStatus()
        Issue.record("expected a failure")
    } catch let error as LatchError {
        guard case .unreachable = error else { Issue.record("\(error)"); return }
    } catch {
        Issue.record("\(error)")
    }
}

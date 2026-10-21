import Foundation
import Synchronization
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

private let stub = URL(filePath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .appending(path: "scripts/stub-latch")

private func stubState() throws -> String {
    let dir = FileManager.default.temporaryDirectory.appending(path: "plow-latch-tests-\(UUID())")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir.path
}

/// Runs the stub to its end without blocking a thread, as `waitUntilExit` would.
private func runStub(_ arguments: [String]) async throws {
    let process = Process()
    process.executableURL = stub
    process.arguments = arguments
    await withCheckedContinuation { continuation in
        process.terminationHandler = { _ in continuation.resume() }
        do { try process.run() } catch { continuation.resume() }
    }
}

private func emit(_ state: String, _ args: String...) async throws {
    try await runStub(["--state-dir", state, "emit"] + args)
}

/// Ends a stub serve and waits until it has gone: the test runner's
/// children ignore SIGTERM, which a shell script can't undo, so the stub's
/// own stop ends it, and a serve left behind would hold the runner open.
private func stopServe(_ serve: Process, state: String) async throws {
    try await runStub(["--state-dir", state, "stop"])
    while serve.isRunning { try await Task.sleep(for: .milliseconds(100)) }
}

// The live client against scripts/stub-latch: real processes, real pipes,
// one test at a time. Run side by side, two of them hung now and then; the
// cause isn't pinned down.
@Suite(.serialized) struct LiveClientAgainstTheStub {
    @Test func liveClientReadsStatusAndFollowsEvents() async throws {
        let state = try stubState()
        try await emit(state, "host", "serving")
        try await emit(state, "activity", "file.read", "allow")
        let client = LiveLatchClient(executable: stub, stateDir: state)

        let status = try await client.localStatus()
        // A lifecycle event alone isn't a running host: no serve holds the lock.
        #expect(!status.serve.running && status.stateDir == state)

        var events = client.events(after: nil, lifecycleOnly: false).makeAsyncIterator()
        guard case let .snapshot(_, host, activity) = try await events.next() else { Issue.record("no snapshot"); return }
        #expect(host == .serving && activity.map(\.op) == ["file.read"])
        try await emit(state, "host", "stopped")
        #expect(try await events.next() == .host(cursor: "3", .stopped))
    }

    @Test func liveClientSurfacesLatchsError() async throws {
        let state = try stubState()
        let refusing = URL(filePath: state).appending(path: "latch")
        try """
        #!/bin/sh
        echo 'starting' >&2
        echo '{"error":{"code":"not_logged_in","message":"not logged in: run `latch login`"}}' >&2
        exit 2
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

    @Test func liveClientLogsInAndSavesShares() async throws {
        let state = try stubState()
        try await emit(state, "logout")
        let client = LiveLatchClient(executable: stub, stateDir: state)
        #expect(try await client.localStatus().peer == nil)
        setenv("STUB_LOGIN_SECONDS", "0", 1)
        var seen: [LoginEvent] = []
        for try await event in client.login() { seen.append(event) }
        #expect(seen.count == 3 && seen[1] == .waiting)
        guard case .verified = seen.last else { Issue.record("not verified"); return }
        #expect(try await client.setShares(["/tmp/a b", "/tmp/c"]).shares == ["/tmp/a b", "/tmp/c"])
        let status = try await client.localStatus()
        #expect(status.peer?.name == "stub-host" && status.shares == ["/tmp/a b", "/tmp/c"])
    }

    @Test func aSecondServeIsRefusedAsLatchRefusesIt() async throws {
        let state = try stubState()
        let first = try Process.run(stub, arguments: ["--state-dir", state, "serve"])
        // Until the first holds the lock.
        let lock = URL(filePath: state).appending(path: "serve.lock")
        while ((try? String(contentsOf: lock, encoding: .utf8)) ?? "").isEmpty {
            try await Task.sleep(for: .milliseconds(50))
        }
        let errPath = URL(filePath: state).appending(path: "second.err")
        FileManager.default.createFile(atPath: errPath.path, contents: nil)
        let second = Process()
        second.executableURL = stub
        second.arguments = ["--state-dir", state, "serve"]
        second.standardError = try FileHandle(forWritingTo: errPath)
        let status = await withCheckedContinuation { continuation in
            second.terminationHandler = { continuation.resume(returning: $0.terminationStatus) }
            do { try second.run() } catch { continuation.resume(returning: -1) }
        }
        #expect(status == 1)
        let line = try String(contentsOf: errPath, encoding: .utf8)
        #expect(line.contains(#""code":"already_serving""#))
        #expect(line.contains("is already served by latch serve (pid \(first.processIdentifier),"))
        try await stopServe(first, state: state)
    }

    @Test func lifecycleStreamSeesAServeStartAndStop() async throws {
        let state = try stubState()
        let client = LiveLatchClient(executable: stub, stateDir: state)
        var events = client.events(after: nil, lifecycleOnly: true).makeAsyncIterator()
        guard case .snapshot(_, .stopped, _) = try await events.next() else { Issue.record("not stopped"); return }
        let serve = try Process.run(stub, arguments: ["--state-dir", state, "serve"])
        #expect(try await events.next() == .host(cursor: "1", .starting))
        #expect(try await events.next() == .host(cursor: "2", .serving))
        let stopping = Task { try await stopServe(serve, state: state) }
        #expect(try await events.next() == .host(cursor: "3", .stopped))
        try await stopping.value
    }

    @Test func aQuietStreamDoesNotHoldUpAnother() async throws {
        let quiet = LiveLatchClient(executable: stub, stateDir: try stubState())
        let busy = LiveLatchClient(executable: stub, stateDir: try stubState())
        var idle = quiet.events(after: nil, lifecycleOnly: true).makeAsyncIterator()
        _ = try await idle.next()  // its snapshot; after this it waits, reading nothing
        let events = busy.events(after: nil, lifecycleOnly: true)
        // Whichever comes first: the busy stream's snapshot, or five seconds. A
        // stalled read can't be cancelled, so the loser is left to run out.
        let first = Mutex<CheckedContinuation<Bool, Never>?>(nil)
        let arrived = await withCheckedContinuation { continuation in
            first.withLock { $0 = continuation }
            Task {
                for try await _ in events { break }
                first.withLock { $0?.resume(returning: true); $0 = nil }
            }
            Task {
                try await Task.sleep(for: .seconds(5))
                first.withLock { $0?.resume(returning: false); $0 = nil }
            }
        }
        #expect(arrived, "the second stream stalled behind the first")
        _ = idle
    }
}

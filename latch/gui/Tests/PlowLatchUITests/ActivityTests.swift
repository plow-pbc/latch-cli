import Foundation
import Testing
@testable import PlowLatchUI

@MainActor
struct ActivityTests {
    private func records(_ fixture: String) throws -> [AuditRecord] {
        try AuditPage.parse(Data(fixture.utf8)).records
    }

    @Test func readsEveryRecordOfARealAudit() throws {
        let page = try AuditPage.parse(Data(Fixtures.studioAudit.utf8))
        #expect(page.records.count == 27)
        #expect(!page.hasOlder)
        let gated = try #require(page.records.first { $0.gate?.p != nil })
        #expect(gated.gate?.p?["NOT_COVERED"] != nil)
    }

    @Test func eachOutcomeHasItsWords() throws {
        let all = try records(Fixtures.studioAudit) + records(Fixtures.kitchenAudit)
        let titles = Set(all.map(\.status.title))
        #expect(titles.isSuperset(of: [
            "Connected", "Allowed", "Refused · not covered", "Refused · you said no",
            "Refused · exec is off for this agent", "Refused · Google is off", "Refused · no instructions from you",
        ]))
    }

    @Test func requestsReadAsTyped() throws {
        let all = try records(Fixtures.studioAudit)
        let commands = all.map(\.command)
        #expect(commands.contains("cat Projects/latch-demo/README.md"))
        #expect(commands.contains("rm -rf Finance"))
        #expect(commands.contains("rules set · rules for every agent"))
    }

    @Test func callersAreNamed() throws {
        let all = try records(Fixtures.studioAudit)
        let rules = try #require(all.first { $0.op == "rules" })
        #expect(rules.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("You", "on this Mac"))
        let laptop = try #require(all.first { $0.op == "exec" && $0.agent == nil })
        #expect(laptop.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("You", "laptop"))
        let scout = try #require(all.first { $0.callerName == "scout" })
        #expect(scout.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("scout", "agent"))
        // An agent's first call after a host starts can be unnamed.
        let unnamed = try #require(all.first { $0.agent != nil && $0.callerName == nil })
        #expect(unnamed.callerLabel(hostKey: nil, hostName: "studio", isThisMac: true).who.hasPrefix("Agent "))
    }

    @Test func aFeedHoldsEachRecordOnceAndAtMostItsLimit() throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: nil)
        feed.stage(Fixtures.studioAudit)
        feed.stage(Fixtures.studioAudit)
        #expect(feed.records.count == 27)
        #expect(feed.caughtUp)
        let record = try #require(feed.records.last)
        for n in 0..<ActivityFeed.limit {
            var copy = record
            copy.id = "r\(n)"
            feed.apply(.record(cursor: copy.id, copy))
        }
        #expect(feed.records.count == ActivityFeed.limit)
        #expect(feed.hasOlder)
        #expect(feed.records.last?.id == "r\(ActivityFeed.limit - 1)")
    }

    @Test func aFeedStreamsOnlyWhileWatched() async throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: "pi-kitchen")
        feed.start()
        for _ in 0..<200 where !feed.caughtUp { try await Task.sleep(for: .milliseconds(5)) }
        #expect(feed.caughtUp)
        #expect(feed.records.contains { $0.outcome == "no_intent" })
        feed.stop()
    }
}

/// Every stream the app reads opens with `hello` (and a newer latch may add
/// line types): each is skipped, never an error.
struct StreamLineTests {
    @Test func unknownLinesAreSkipped() throws {
        for line in [#"{"type":"hello","schema":1}"#, #"{"type":"something_new","x":1}"#] {
            let data = Data(line.utf8)
            #expect(try LatchJSON.decoder.decode(LoginEvent.self, from: data) == .other)
            #expect(try LatchJSON.decoder.decode(AuditLine.self, from: data) == (line.contains("hello") ? .hello : .other))
            guard case .other = try LatchJSON.decoder.decode(StatusLine.self, from: data) else {
                Issue.record("status line \(line) wasn't skipped")
                continue
            }
        }
    }
}

/// Against a real latch and a host it serves, named by the environment
/// (`LATCH_LIVE_LATCH`, `LATCH_LIVE_STATE`, and `LATCH_LIVE_CALLER_STATE`
/// for another of the owner's machines that calls it; with
/// `LATCH_LIVE_REMOTE`, also a host that machine reads with `--host`);
/// skipped without.
@MainActor
struct LiveActivityTests {
    nonisolated static let env = ProcessInfo.processInfo.environment

    @Test(.enabled(if: env["LATCH_LIVE_LATCH"] != nil), arguments: [false, true])
    func aNewRecordArrivesWhileWatchedAndLatchStopsAfter(remote: Bool) async throws {
        guard !remote || Self.env["LATCH_LIVE_REMOTE"] != nil else { return }
        let latch = URL(filePath: try #require(Self.env["LATCH_LIVE_LATCH"]))
        let caller = try #require(Self.env["LATCH_LIVE_CALLER_STATE"])
        // Remote: the caller's machine reads the remote host's audit.
        let state = remote ? caller : try #require(Self.env["LATCH_LIVE_STATE"])
        let host = remote ? Self.env["LATCH_LIVE_REMOTE"]
            : try await LiveLatchClient(executable: latch, stateDir: state).localStatus().peer?.name
        let feed = ActivityFeed(client: LiveLatchClient(executable: latch, stateDir: state), host: remote ? host : nil)
        feed.start()
        for _ in 0..<500 where !feed.caughtUp { try await Task.sleep(for: .milliseconds(10)) }
        #expect(feed.caughtUp)
        let before = feed.records.count
        let marker = "live-\(UUID().uuidString.prefix(8))"
        _ = try await LatchProcess(executable: latch, arguments: ["--state-dir", caller, "exec", "--host", try #require(host),
                                                                   "--", "echo", marker]).output()
        for _ in 0..<1000 where !feed.records.contains(where: { $0.args?.argv?.last == marker }) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(feed.records.count > before)
        #expect(feed.records.contains { $0.command == "echo \(marker)" && $0.status == .allowed(nil) })
        feed.stop()
        try await Task.sleep(for: .milliseconds(500))
        #expect(Self.followers(state) == 0)
    }

    /// `latch audit --follow` runs still started on `state`.
    static func followers(_ state: String) -> Int {
        let ps = Process()
        ps.executableURL = URL(filePath: "/bin/ps")
        ps.arguments = ["-axo", "command"]
        let out = Pipe()
        ps.standardOutput = out
        try? ps.run()
        let listing = out.fileHandleForReading.readDataToEndOfFile()
        ps.waitUntilExit()
        return String(decoding: listing, as: UTF8.self)
            .split(separator: "\n").filter { $0.contains(state) && $0.contains("audit --follow") }.count
    }
}

/// The inspector shows every field a record holds, whatever its op.
struct RecordFieldsTests {
    private func record(_ json: String) throws -> AuditRecord { try LatchJSON.decoder.decode(AuditRecord.self, from: Data(json.utf8)) }

    // A real `file get` record (path shortened).
    @Test func aFileGetShowsItsPathSizeAndHash() throws {
        let file = try record(#"{"agent":null,"caller":"e4144a3a","caller_name":"laptop","decision":"allow","details":{"args":{"path":"/Users/me/Projects/latch-demo/README.md","sha256":"eb9c26baee47f19e4993a77bca936d0ff09e355a82d3db79bf154ebff1a80604","size":5},"gate":null,"started":"2026-10-23T05:27:23.226971Z"},"id":"e5d0","op":"file_get","outcome":"ok","time":"2026-10-23T05:27:23.45253Z"}"#)
        let shown = RecordInspector.otherFields(of: file).map { (RecordInspector.label($0.key), RecordInspector.value($0.key, $0.value)) }
        #expect(shown.map(\.0) == ["Path", "SHA-256", "Size"])
        #expect(shown[0].1 == "/Users/me/Projects/latch-demo/README.md")
        #expect(shown[1].1.hasPrefix("eb9c26ba"))
        #expect(shown[2].1 == "5 bytes")
    }

    // Google's audit fields as `google/host.rs` writes them: command, flags,
    // recipients, accounts, and each account's outcome.
    @Test func aGoogleCallShowsRecipientsAndAccounts() throws {
        let google = try record(#"{"agent":"a1","caller":"k","caller_name":"scout","decision":"allow","details":{"args":{"command":"gmail.send","flags":["--to"],"recipients":["ada@example.com","bob@example.com"],"accounts":["me@example.com"],"outcomes":[{"account":"me@example.com","status":"ok"}]},"gate":null,"started":null},"id":"g1","op":"google","outcome":"ok","time":"2026-10-23T05:27:23Z"}"#)
        let shown = Dictionary(uniqueKeysWithValues: RecordInspector.otherFields(of: google)
            .map { (RecordInspector.label($0.key), RecordInspector.value($0.key, $0.value)) })
        #expect(shown["Recipients"] == "ada@example.com\nbob@example.com")
        #expect(shown["Accounts"] == "me@example.com")
        #expect(shown["Command"] == "gmail.send")
        #expect(shown["Outcomes"] == "account: me@example.com, status: ok")
        #expect(google.command == "google gmail send")
    }

    // Exec's own fields have their own sections; the rest still shows.
    @Test func execShowsWhatItsSectionsDont() throws {
        let exec = try #require(try AuditPage.parse(Data(Fixtures.studioAudit.utf8)).records.first { $0.op == "exec" && $0.args?.exitCode != nil })
        #expect(RecordInspector.otherFields(of: exec).map(\.key) == ["timeout"])
    }
}

/// A latch on a Mac that has never served: `audit --follow` refuses with
/// `no_host_here` until its host starts, then streams.
final class NeverServedClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var hosting = false
    private(set) var follows = 0

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(hosting ? Fixtures.serving : Fixtures.stopped) }
    func accountStatus() async throws -> AccountStatus { throw LatchError.unreachable("no Plow") }
    func login() -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func pluginsList(host: String?) async throws -> PluginsList { try Fixtures.decode(#"{"plugins":[]}"#) }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> {
        follows += 1
        let hosting = hosting
        return AsyncThrowingStream { continuation in
            guard hosting else {
                return continuation.finish(throwing: LatchError.failed(LatchFailure(code: "no_host_here", message: "this machine has never served")))
            }
            continuation.yield(.caughtUp(cursor: String(repeating: "0", count: 64), hasOlder: false))
        }
    }
    func serve(again: Bool) async throws { hosting = true }
    func stop() async throws {}
}

@MainActor
struct NeverServedTests {
    private func until(_ done: () -> Bool) async {
        for _ in 0..<300 where !done() { try? await Task.sleep(for: .milliseconds(5)) }
    }

    @Test func activityFollowsOnceThisMacStartsHosting() async throws {
        let client = NeverServedClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        let feed = app.feed(for: try #require(app.host))
        feed.start()
        await until { feed.error != nil }
        #expect(feed.error?.code == "no_host_here")
        // Waiting alone doesn't ask again.
        try await Task.sleep(for: .milliseconds(100))
        #expect(client.follows == 1)
        client.hosting = true
        app.statusChanged(try Fixtures.decode(Fixtures.serving))
        await until { feed.caughtUp }
        #expect(feed.caughtUp)
        #expect(feed.error == nil)
        feed.stop()
    }

    // "Start Latch" reads the new status itself, so the status stream may
    // never show the change: whichever path learns this Mac serves, the
    // activity follows.
    @Test func activityFollowsWhenStartingHostingReadsTheStatusFirst() async throws {
        let client = NeverServedClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        let feed = app.feed(for: try #require(app.host))
        feed.start()
        await until { feed.error != nil }
        #expect(feed.error?.code == "no_host_here")
        await app.startHosting()
        // The stream's copy of the change arrives after: nothing more to do.
        app.statusChanged(try Fixtures.decode(Fixtures.serving))
        await until { feed.caughtUp }
        #expect(feed.caughtUp)
        #expect(client.follows == 2)
        feed.stop()
    }
}

/// The owner's reads, as latch records them (real records' shapes).
struct OwnerReadTests {
    private func record(op: String, args: String, agent: String? = nil) throws -> AuditRecord {
        try LatchJSON.decoder.decode(AuditRecord.self, from: Data(
            #"{"agent":\#(agent.map { "\"\($0)\"" } ?? "null"),"caller":"k","caller_name":"studio","decision":"allow","details":{"args":\#(args),"gate":null,"started":null},"id":"\#(op)","op":"\#(op)","outcome":"ok","time":"2026-10-23T05:27:23Z"}"#.utf8))
    }

    @Test func readsAreBackgroundAndActionsAreNot() throws {
        for (op, args) in [("rules", #"{"command":"show"}"#), ("vault", #"{"command":"list"}"#), ("vault", #"{"command":"show","item":"v1"}"#),
                           ("plugins", #"{"command":"list"}"#), ("status", "{}"), ("audit", "{}")] {
            #expect(try record(op: op, args: args).isBackground, "\(op) \(args)")
        }
        for (op, args) in [("rules", #"{"command":"set","scope":"every agent"}"#), ("vault", #"{"command":"add"}"#),
                           ("plugins", #"{"command":"enable","name":"google"}"#), ("exec", #"{"argv":["ls"]}"#)] {
            #expect(try !record(op: op, args: args).isBackground, "\(op) \(args)")
        }
        // An agent asking is never the owner looking.
        #expect(try !record(op: "vault", args: #"{"command":"list"}"#, agent: "a1").isBackground)
    }
}

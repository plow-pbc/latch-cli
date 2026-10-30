import AppKit
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
            "Connected", "Allowed", "Refused by gatekeeper · needed your OK", "Refused by gatekeeper · you said no",
            "Refused · exec is off for this agent", "Refused · Google is off",
        ]))
    }

    /// latch gives each record its command; a connection has none.
    @Test func requestsAreLatchsOwnCommands() throws {
        let all = try records(Fixtures.studioAudit)
        let commands = all.map(\.command)
        #expect(commands.contains("latch exec cat Projects/latch-demo/README.md"))
        #expect(commands.contains("latch rules set"))
        #expect(all.filter { $0.op == "connect" }.allSatisfy { $0.command == nil })
        let quoted = try LatchJSON.decoder.decode(AuditRecord.self, from: Data(#"{"agent":null,"caller":"k","caller_name":"vac2","command":"latch exec cat 'Projects/my file.txt'","decision":"allow","details":null,"id":"e1","op":"exec","outcome":"ok","time":"2026-10-23T08:40:02Z"}"#.utf8))
        #expect(quoted.command == "latch exec cat 'Projects/my file.txt'")
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

    @Test func aFeedHoldsEachRecordOnce() throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: nil)
        feed.stage(Fixtures.studioAudit)
        feed.stage(Fixtures.studioAudit)
        #expect(feed.records.count == 27)
        #expect(feed.caughtUp)
    }

    @Test func aFeedStreamsOnlyWhileWatched() async throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: "pi-kitchen")
        feed.start()
        await waitUntil { feed.caughtUp }
        #expect(feed.caughtUp)
        #expect(feed.records.contains { $0.gate?.source == "defaults" })
        feed.stop()
    }

    /// The lines are gathered off the main actor: a busy main thread takes
    /// the opening page in a turn or two, not one turn a line.
    @Test func aBusyMainThreadTakesTheOpeningPageAtOnce() async throws {
        let feed = ActivityFeed(client: HostLog(records: 2000), host: nil)
        feed.start()
        defer { feed.end() }
        var turns = 0
        while !feed.caughtUp, turns < 1000 {
            // Busy for a moment each turn, the way other work on the main
            // thread is.
            usleep(1000)
            turns += 1
            await Task.yield()
        }
        #expect(feed.records.count == ActivityFeed.openingPage)
        #expect(turns < 50, "\(turns) turns")
    }

    /// Lines that came while the feed was busy are taken as one batch when
    /// it comes back, in order, and the next batch still waits its interval.
    @Test func aFeedThatFellBehindTakesWhatCameMeanwhileAsOneBatch() async throws {
        let (lines, latch) = AsyncThrowingStream<AuditLine, Error>.makeStream()
        let batches = LineBatches(lines, every: .milliseconds(100))
        // Busy elsewhere while five lines come, each more than an interval
        // after the last.
        for n in 0..<5 {
            latch.yield(HostLog.line(n))
            try await Task.sleep(for: .milliseconds(120))
        }
        #expect(try await batches.due())
        let took = ContinuousClock.now
        #expect(batches.take() == (0..<5).map(HostLog.line))
        latch.yield(HostLog.line(5))
        #expect(try await batches.due())
        #expect(ContinuousClock.now - took >= .milliseconds(100))
        #expect(batches.take() == [HostLog.line(5)])
        latch.finish()
        #expect(try await batches.due() == false)
    }

    /// A record that came after catching up but before the take doesn't
    /// make the next batch due sooner than an interval after the take.
    @Test func theNextBatchWaitsAnIntervalFromTheTake() async throws {
        let (lines, latch) = AsyncThrowingStream<AuditLine, Error>.makeStream()
        let batches = LineBatches(lines, every: .milliseconds(100))
        latch.yield(HostLog.line(0))
        latch.yield(.caughtUp(cursor: HostLog.id(0), hasOlder: false))
        #expect(try await batches.due())
        latch.yield(HostLog.line(1))
        try await Task.sleep(for: .milliseconds(30))
        let took = ContinuousClock.now
        #expect(batches.take().first == HostLog.line(0))
        latch.yield(HostLog.line(2))
        #expect(try await batches.due())
        #expect(ContinuousClock.now - took >= .milliseconds(100))
        #expect(batches.take() == [HostLog.line(2)])
    }

    /// The feed already waiting when the main thread stalls: the line that
    /// woke it and those that came during the stall go in as one batch.
    @Test func aFeedWaitingWhenTheMainThreadStallsTakesWhatCameMeanwhileAsOneBatch() async throws {
        let (lines, latch) = AsyncThrowingStream<AuditLine, Error>.makeStream()
        let batches = LineBatches(lines, every: .milliseconds(100))
        let applied = Task { @MainActor in
            var applied: [[AuditLine]] = []
            while try await batches.due() { applied.append(batches.take()) }
            return applied
        }
        // Waiting, and an interval has passed: the next line is due at once.
        try await Task.sleep(for: .milliseconds(150))
        latch.yield(HostLog.line(0))
        // The main thread stalls; four more lines come meanwhile, more than
        // an interval after the first.
        usleep(50_000)
        for n in 1..<5 { latch.yield(HostLog.line(n)) }
        usleep(200_000)
        latch.finish()
        #expect(try await applied.value == [(0..<5).map(HostLog.line)])
    }
}

/// Every stream the app reads opens with `hello` (and a newer latch may add
/// line types): each is skipped, never an error.
struct StreamLineTests {
    @Test func unknownLinesAreSkipped() throws {
        for line in [#"{"type":"hello","schema":1}"#, #"{"type":"something_new","x":1}"#] {
            let data = Data(line.utf8)
            #expect(try LatchJSON.decoder.decode(LoginLine.self, from: data).event == nil)
            #expect(try LatchJSON.decoder.decode(AuditLine.self, from: data) == (line.contains("hello") ? .hello : .other))
            guard case .other = try LatchJSON.decoder.decode(StatusLine.self, from: data) else {
                Issue.record("status line \(line) wasn't skipped")
                continue
            }
        }
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
    }

    // Exec's own fields have their own sections; the rest still shows.
    @Test func execShowsWhatItsSectionsDont() throws {
        let exec = try #require(try AuditPage.parse(Data(Fixtures.studioAudit.utf8)).records.first { $0.op == "exec" && $0.args?.exitCode != nil })
        #expect(RecordInspector.otherFields(of: exec).map(\.key) == ["timeout"])
    }
}

/// A latch on a Mac that has never served: `audit --follow` refuses with
/// `no_host_here` (or `refusal`) until its host starts, then streams.
final class NeverServedClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var hosting = false
    var refusal = LatchFailure(code: "no_host_here", message: "this machine has never served")
    private(set) var follows = 0
    /// Where each follow picked up.
    private(set) var afters: [String?] = []

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(hosting ? Fixtures.serving : Fixtures.stopped) }
    func accountStatus() async throws -> AccountStatus { throw LatchError.unreachable("no Plow") }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
        follows += 1
        let hosting = hosting
        let refusal = refusal
        return AsyncThrowingStream { continuation in
            guard hosting else {
                return continuation.finish(throwing: LatchError.failed(refusal))
            }
            continuation.yield(.caughtUp(cursor: String(repeating: "0", count: 64), hasOlder: false))
        }
    }
    /// Start Latch runs the host step's args (`serve -d --again`).
    func perform(_ args: [String]) async throws { hosting = true }
    func stop() async throws {}
}

@MainActor
struct NeverServedTests {
    // This Mac learns it serves either from the status stream or, after
    // Start Latch, from Start Latch's own read (the stream may then never
    // show the change): either way, the activity follows.
    @Test(arguments: [false, true])
    func activityFollowsOnceThisMacStartsHosting(startViaApp: Bool) async throws {
        let client = NeverServedClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        let feed = app.feed(for: try #require(app.host))
        feed.start()
        await waitUntil { feed.error != nil }
        #expect(feed.error?.code == "no_host_here")
        // An error waiting won't fix ends the stream as it is recorded:
        // nothing is asked again until this Mac hosts.
        #expect(client.follows == 1)
        if startViaApp {
            await app.startHosting()
        } else {
            client.hosting = true
            app.statusChanged(try Fixtures.decode(Fixtures.serving))
        }
        await waitUntil { feed.caughtUp }
        #expect(feed.caughtUp && feed.error == nil)
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

    /// Every `latch status`, an agent's too, asks each host for its plugins:
    /// answered, that is background; refused, it stays in view.
    @Test func anyCallersAnsweredPluginsListIsBackground() throws {
        #expect(try record(op: "plugins", args: #"{"command":"list"}"#, agent: "a1").isBackground)
        let refused = try LatchJSON.decoder.decode(AuditRecord.self, from: Data(
            #"{"agent":"a1","caller":"k","caller_name":null,"decision":"deny","details":{"args":{"command":"list"},"gate":null,"started":null},"id":"p","op":"plugins","outcome":"host_upgraded","time":"2026-10-23T05:27:23Z"}"#.utf8))
        #expect(!refused.isBackground)
    }
}

/// Records whose shape differs from exec's, as latch writes them.
struct RecordShapeTests {
    private func record(_ json: String) throws -> AuditRecord { try LatchJSON.decoder.decode(AuditRecord.self, from: Data(json.utf8)) }

    // `browser/judged.rs` keeps a browser judgment in the args, as `gate`,
    // beside what was judged.
    @Test func aBrowserJudgmentIsRead() throws {
        let refused = try record(#"{"agent":"a1","caller":"k","caller_name":"scout","decision":"deny","details":{"args":{"command":"open","origin":"https://shop.example","judged":{"kind":"open"},"gate":{"verdict":"owner_said_no","source":"chat_and_rules","p":{"COVERED":0.1,"FORBIDDEN":0.9,"NOT_COVERED":0.0}}},"gate":null,"started":null},"id":"b1","op":"browser","outcome":"gatekeeper_denied","time":"2026-10-23T05:27:23Z"}"#)
        #expect(refused.status == .gatekeeper(saidNo: true))
        #expect(refused.gate?.source == "chat_and_rules")
        #expect(!RecordInspector.otherFields(of: refused).map(\.key).contains("gate"))
    }

    // A browser step on the host's own Mac: `serve.rs` marks the details'
    // gate `{"caller":"owner_on_host"}`; the judgment is still the args'.
    @Test func aLocalBrowserJudgmentIsReadPastTheHostMarker() throws {
        let refused = try record(#"{"agent":null,"caller":"k","caller_name":"studio","decision":"deny","details":{"args":{"command":"open","judged":{"kind":"open"},"gate":{"verdict":"owner_said_no","source":"owner_request"}},"gate":{"caller":"owner_on_host"},"started":null},"id":"b2","op":"browser","outcome":"gatekeeper_denied","time":"2026-10-23T05:27:23Z"}"#)
        #expect(refused.status == .gatekeeper(saidNo: true))
        #expect(refused.gate?.source == "owner_request")
    }

    // latch denies more than the gatekeeper does: a stop of its own says
    // what happened, never "Refused by gatekeeper".
    @Test(arguments: [
        ("confirm_needed", "Needs confirmation"), ("no_such_field", "Field not found"),
        ("no_such_item", "Item not found"), ("secret_on_page", "Secret on page"),
    ])
    func latchsOwnStopsSayWhatHappened(_ outcome: String, _ title: String) throws {
        let stopped = try record(#"{"agent":"a1","caller":"k","caller_name":"scout","decision":"deny","details":{"args":{"command":"do","gate":{"verdict":"allow"}},"gate":null,"started":null},"id":"s1","op":"browser","outcome":"\#(outcome)","time":"2026-10-23T05:27:23Z"}"#)
        #expect(stopped.status == .stopped(title))
        #expect(stopped.status.title == title)
    }

    @Test func onlyTheGatekeepersDenialsAreItsRefusals() throws {
        let notCovered = try record(#"{"agent":"a1","caller":"k","caller_name":"scout","decision":"deny","details":{"args":{"argv":["rm","x"]},"gate":{"verdict":"not_covered"},"started":null},"id":"g1","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T05:27:23Z"}"#)
        #expect(notCovered.status.title == "Refused by gatekeeper · needed your OK")
        let execOff = try record(#"{"agent":"a1","caller":"k","caller_name":"scout","decision":"deny","details":{"args":{"argv":["ls"]},"gate":null,"started":null},"id":"g2","op":"exec","outcome":"exec_denied","time":"2026-10-23T05:27:23Z"}"#)
        #expect(execOff.status.title == "Refused · exec is off for this agent")
    }

    // `serve.rs` records a refused connection with decision deny.
    @Test func aRefusedConnectionIsARefusal() throws {
        let refused = try record(#"{"agent":null,"caller":"k","caller_name":null,"decision":"deny","details":null,"id":"c1","op":"connect","outcome":"caller_denied","time":"2026-10-23T05:27:23Z"}"#)
        #expect(refused.status == .refused("caller not allowed"))
        #expect(refused.command == nil)
        #expect(!refused.isBackground)
        let allowed = try record(#"{"agent":null,"caller":"k","caller_name":"laptop","decision":"allow","details":null,"id":"c2","op":"connect","outcome":"connected","time":"2026-10-23T05:27:23Z"}"#)
        #expect(allowed.status == .connected && allowed.isBackground)
    }
}

/// Home's Recent reaches back past connections and the owner's reads.
@MainActor
struct RecentTests {
    @Test func homeLoadsOlderPagesUntilItHasFiveRequests() async throws {
        let client = PagedClient()
        let feed = ActivityFeed(client: client, host: nil)
        feed.stage(client.newest)
        #expect(feed.records.filter { !$0.isBackground }.isEmpty && feed.hasOlder)
        await feed.loadOlder(until: 5, pages: ActivityFeed.homePages)
        #expect(feed.records.filter { !$0.isBackground }.count == 6)
        #expect(client.pages == 1)
        // The earlier page (94…99) went before the newest (100…149); a
        // record of it streamed again isn't held twice.
        feed.apply(try LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line(95, op: "exec").utf8)))
        #expect(feed.records.map(\.id) == (94..<150).map { String(format: "%064d", $0) })
    }
}

/// A host whose newest 50 records are all connections, with six exec
/// requests on the page before.
final class PagedClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    private(set) var pages = 0
    private(set) var follows = 0
    /// Where each follow picked up.
    private(set) var afters: [String?] = []
    /// What `audit --follow` streams, given how many follows came before;
    /// nothing by default.
    var following: (Int) -> AsyncThrowingStream<AuditLine, Error> = { _ in AsyncThrowingStream { _ in } }
    static func line(_ n: Int, op: String) -> String {
        let id = String(format: "%064d", n)
        let args = op == "exec" ? #"{"argv":["ls"]}"# : "null"
        return #"{"type":"record","cursor":"\#(id)","record":{"agent":null,"caller":"k","caller_name":"laptop","decision":"allow","details":{"args":\#(args)},"id":"\#(id)","op":"\#(op)","outcome":"ok","time":"2026-10-23T05:27:23Z"}}"#
    }
    var newest: String {
        ([#"{"type":"snapshot","cursor":"x","has_older":true}"#] + (100..<150).map { Self.line($0, op: "connect") }
            + [#"{"type":"caught_up","cursor":"y","has_older":true}"#]).joined(separator: "\n")
    }
    /// Each earlier page: six records of `olderOp`, more before them when
    /// `olderHasMore`; a page throws `olderFails` instead when it's set.
    var olderOp = "exec"
    var olderHasMore = false
    var olderFails: (any Error)?
    func audit(host: String?, before: String?) async throws -> AuditPage {
        pages += 1
        if let olderFails { throw olderFails }
        let end = 100 - (pages - 1) * 6
        return try AuditPage.parse(Data((((end - 6)..<end).map { Self.line($0, op: olderOp) }
            + [#"{"type":"caught_up","cursor":"z","has_older":\#(olderHasMore)}"#]).joined(separator: "\n").utf8))
    }
    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
        follows += 1
        afters.append(after)
        return following(follows - 1)
    }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct TooLargeRecordTests {
    @Test func activityStopsAtARecordTooLargeToSendInsteadOfAskingForeverAgain() async throws {
        let client = NeverServedClient()
        client.refusal = LatchFailure(code: "record_too_large", message: "the record is too large")
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        let feed = app.feed(for: try #require(app.host))
        feed.start()
        for _ in 0..<200 where feed.running { try await Task.sleep(for: .milliseconds(5)) }
        #expect(!feed.running)
        #expect(feed.error?.code == "record_too_large")
        #expect(client.follows == 1)
        feed.stop()
    }
}

/// Activity holds each record once, oldest first, however it arrives: from
/// the stream, an earlier page, or again after the host reset its cursor.
@MainActor
struct ActivityOrderTests {
    private static func line(_ n: Int) throws -> AuditLine {
        try LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line(n, op: "exec").utf8))
    }

    @Test func aRecordStreamedTwiceIsHeldOnce() throws {
        let feed = ActivityFeed(client: PagedClient(), host: nil)
        for n in [1, 2, 1, 3, 2] { feed.apply(try Self.line(n)) }
        #expect(feed.records.map(\.id) == [1, 2, 3].map { String(format: "%064d", $0) })
    }

    @Test func afterTheHostResetsItsCursorTheRecordsComeBackOnce() async throws {
        let client = PagedClient()
        let record = try Self.line(1)
        client.following = { before in
            AsyncThrowingStream { continuation in
                continuation.yield(record)
                if before == 0 {
                    continuation.finish(throwing: LatchError.failed(LatchFailure(code: "cursor_expired", message: "that record isn't in this audit")))
                } else {
                    continuation.yield(.caughtUp(cursor: String(format: "%064d", 1), hasOlder: false))
                }
            }
        }
        let feed = ActivityFeed(client: client, host: nil)
        feed.start()
        await waitUntil { feed.caughtUp }
        #expect(client.follows == 2)
        #expect(feed.records.map(\.id) == [String(format: "%064d", 1)])
        feed.stop()
    }
}

/// Hands out each screenshot it is asked for, and remembers each ask.
final class ShotClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    /// The shots the host no longer keeps.
    var gone: Set<String> = []
    private(set) var asked: [String] = []

    func auditShot(_ record: String, shot: String, host: String?) async throws -> Data {
        asked.append("\(record) \(shot) \(host ?? "-")")
        guard !gone.contains(shot) else {
            throw LatchError.failed(LatchFailure(code: "no_such_shot", message: "record \(record)'s screenshot \(shot) is no longer kept"))
        }
        return Data("png \(shot)".utf8)
    }
}

@MainActor
struct RecordShotsTests {
    private func browser() throws -> [AuditRecord] {
        try AuditPage.parse(Data(Fixtures.browserAudit.utf8)).records
    }

    @Test func aPlanNamesItsShotsInTheOrderTheyWereTaken() throws {
        let browser = try browser()
        let plan = try #require(browser.first { $0.args?.command == "do" })
        #expect(plan.shots == ["1", "final"])
        #expect(!plan.isScreenshot)
        let screenshot = try #require(browser.first { $0.args?.command == "screenshot" })
        #expect(screenshot.shots.isEmpty && screenshot.isScreenshot)
        #expect(try records(Fixtures.studioAudit).allSatisfy { $0.shots.isEmpty && !$0.isScreenshot })
        // The app's own reads of them are the owner's reads, as `audit` is.
        let read = try LatchJSON.decoder.decode(AuditRecord.self, from: Data(#"{"agent":null,"caller":"k","command":"latch audit shot","decision":"allow","details":null,"id":"e2","op":"audit_shot","outcome":"ok","time":"2026-10-23T22:50:02Z"}"#.utf8))
        #expect(read.isOwnerRead && read.isBackground)
        let judged = try LatchJSON.decoder.decode(AuditRecord.self, from: Data(#"{"agent":null,"caller":"k","command":"latch audit judge","decision":"allow","details":{"args":{"request":"g1"}},"id":"e3","op":"audit_judge","outcome":"ok","time":"2026-10-29T01:00:00Z"}"#.utf8))
        #expect(judged.isOwnerRead && judged.isBackground)
    }

    /// Nothing is read until the record is opened; then each shot once,
    /// from the record's own host, into memory. A shot the host no longer
    /// keeps says how long it keeps them.
    @Test(arguments: [("pi-kitchen", []), (nil, ["1"])] as [(String?, Set<String>)])
    func shotsLoadOnceFromTheirHost(host: String?, gone: Set<String>) async throws {
        let plan = try #require(try browser().first { $0.args?.command == "do" })
        let client = ShotClient()
        client.gone = gone
        let shots = RecordShots(client: client, host: host, record: plan)
        #expect(client.asked.isEmpty)
        #expect(shots.shots.map(\.state) == [.loading, .loading])
        await shots.load()
        #expect(client.asked == ["1", "final"].map { "\(plan.id) \($0) \(host ?? "-")" })
        let first: RecordShots.State = gone.isEmpty ? .loaded(Data("png 1".utf8)) : .gone
        #expect(shots.shots.map(\.state) == [first, .loaded(Data("png final".utf8))])
        #expect(shots.shots.map(\.title) == ["After action 1", "Last page"])
        #expect(RecordShots.gone.contains("Kept on the host for 7 days"))
        #expect(RecordShots.standalone.contains("Saved on the caller's machine"))
        await shots.load()
        #expect(client.asked.count == 2)
    }

    private func records(_ fixture: String) throws -> [AuditRecord] {
        try AuditPage.parse(Data(fixture.utf8)).records
    }

    /// A tile is drawn from a thumbnail made once, not the whole page.
    @Test func eachShotGetsASmallThumbnailOnce() async throws {
        let plan = try #require(try browser().first { $0.args?.command == "do" })
        let shots = RecordShots(client: FakeLatchClient(), host: nil, record: plan)
        await shots.load()
        let thumbnail = try #require(shots.shots.first?.thumbnail)
        #expect(max(thumbnail.size.width, thumbnail.size.height) == 256)
        guard case let .loaded(png) = shots.shots.first?.state else { Issue.record("not loaded"); return }
        #expect(NSImage(data: png)?.size.width == 1280)
    }

    /// The bundle's latch saves the shot into a folder of its own, which is
    /// gone once the bytes are read: nothing stays on disk.
    @Test func theAppKeepsNoCopyOnDisk() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        try """
            #!/bin/sh
            printf '%s\\n' "$@" > "\(folder.path)/argv"
            while [ "$1" != "--output" ]; do shift; done
            printf 'png bytes' > "$2"
            dirname "$2" > "\(folder.path)/saved-in"
            stat -f %Lp "$(dirname "$2")" > "\(folder.path)/mode"
            echo '{"record":"r","shot":"1","path":"'"$2"'","size":9,"sha256":"x"}'
            """.write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)
        let client = LiveLatchClient(executable: latch, stateDir: folder.path)
        let png = try await client.auditShot("e3b0c442", shot: "1", host: "pi-kitchen")
        #expect(png == Data("png bytes".utf8))
        let argv = try String(contentsOf: folder.appending(path: "argv"), encoding: .utf8).split(separator: "\n")
        #expect(Array(argv.prefix(6)) == ["--state-dir", Substring(folder.path), "audit", "shot", "e3b0c442", "1"])
        #expect(argv.suffix(2) == ["--host", "pi-kitchen"])
        let savedIn = try String(contentsOf: folder.appending(path: "saved-in"), encoding: .utf8).trimmingCharacters(in: .newlines)
        #expect(savedIn.contains("latch-shot-"))
        #expect(!FileManager.default.fileExists(atPath: savedIn))
        #expect(try String(contentsOf: folder.appending(path: "mode"), encoding: .utf8) == "700\n")
    }
}

/// Answers `latch audit judge` with each call's id, and remembers each ask.
final class JudgeClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    private(set) var asked: [String] = []

    func auditJudge(_ id: String, host: String?) async throws -> String {
        asked.append("\(id) \(host ?? "-")")
        return "{\n  \"id\": \"\(id)\"\n}\n"
    }
}

/// Answers `latch audit judge` once released, saying when it was asked.
nonisolated final class HeldJudgeClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    let (asked, asking) = AsyncStream.makeStream(of: Void.self)
    let (released, release) = AsyncStream.makeStream(of: Void.self)

    func auditJudge(_ id: String, host: String?) async throws -> String {
        asking.yield()
        for await _ in released { break }
        return "{}"
    }
}

@MainActor
struct GatekeeperRequestTests {
    /// The owner's call is kept as one; an agent's as its rules layer, then
    /// its whole-chat and owner-only calls. Records from before have none.
    @Test func aJudgedRecordNamesTheCallsItsHostKept() throws {
        let decode = { (gate: String) in try LatchJSON.decoder.decode(AuditRecord.Gate.self, from: Data(gate.utf8)) }
        #expect(try decode(#"{"verdict":"allow","kept_request":"g1"}"#).keptRequests == ["g1"])
        let agent = #"{"layers":{"chat":{"kept_request":"g3"},"rules":{"kept_request":"g2"},"will":{"kept_request":"g4"}}}"#
        #expect(try decode(agent).keptRequests == ["g2", "g3", "g4"])
        #expect(try decode(#"{"verdict":"allow"}"#).keptRequests.isEmpty)
    }

    /// Each is read from the record's own host; an agent's three as one JSON list.
    @Test func theRequestsReadAsOneJSONDocument() async throws {
        let client = JudgeClient()
        let one = try await RecordInspector.judged(["g1"], client: client, host: nil)
        #expect(try JSONSerialization.jsonObject(with: Data(one.utf8)) as? [String: String] == ["id": "g1"])
        let both = try await RecordInspector.judged(["g2", "g3", "g4"], client: client, host: "pi-kitchen")
        let list = try #require(try JSONSerialization.jsonObject(with: Data(both.utf8)) as? [[String: String]])
        #expect(list.map { $0["id"] } == ["g2", "g3", "g4"])
        #expect(client.asked == ["g1 -", "g2 pi-kitchen", "g3 pi-kitchen", "g4 pi-kitchen"])
    }

    /// A read still going when its record is left gives nothing to show:
    /// neither its file nor its failure.
    @Test func aReadCancelledWithItsRecordPublishesNothing() async throws {
        let client = HeldJudgeClient()
        let read = Task { await RecordInspector.read(["g1"], client: client, host: nil) }
        for await _ in client.asked { break }
        read.cancel()
        client.release.yield()
        #expect(await read.value == nil)
        #expect(await RecordInspector.read(["g1"], client: JudgeClient(), host: nil) == .success("{\n  \"id\": \"g1\"\n}"))
    }

    /// A screenshot's bytes go where every preview goes: the owner's own
    /// file in the folder the app clears at launch, so a crash leaves none.
    @Test func aPreviewedScreenshotIsTheOwnersAndInTheClearedFolder() throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "GatekeeperRequestTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        let previews = QuickLookFiles(dir: dir)
        let file = try #require(previews.write(Data("png".utf8), name: "After action 1.png"))
        #expect(file.deletingLastPathComponent().standardizedFileURL == dir.standardizedFileURL)
        #expect(try Data(contentsOf: file) == Data("png".utf8))
        #expect((try FileManager.default.attributesOfItem(atPath: file.path))[.posixPermissions] as? Int == 0o600)
        previews.removeAll()
        #expect(!FileManager.default.fileExists(atPath: dir.path))
    }
}

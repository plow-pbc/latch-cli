import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// A latch that counts every process the app would start, by command, and
/// the host each asked (nil: this Mac); a held command waits for its gate.
final class CountingClient: LatchClient, @unchecked Sendable {
    private let counted = Mutex<[(command: String, host: String?)]>([])
    var held: [String: Gate] = [:]
    var calls: [String] { counted.withLock { $0.map(\.command) } }
    var hostsAsked: [String?] { counted.withLock { $0.map(\.host) } }

    private func ran(_ command: String, host: String? = nil) async {
        counted.withLock { $0.append((command, host)) }
        await held[command]?.wait()
    }

    func localStatus() async throws -> LocalStatus {
        await ran("status --local")
        return try Fixtures.decode(Fixtures.serving)
    }
    func hostStatus(_ host: String) async throws -> LocalStatus {
        await ran("status --host", host: host)
        return try Fixtures.decode(Fixtures.serving)
    }
    func setupStatus() async throws -> SetupStatus {
        await ran("setup --status")
        return try Fixtures.decode(Fixtures.setup(on: []))
    }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow {
        await ran("rules show", host: host)
        return try Fixtures.decode(Fixtures.studioRules)
    }
    func agents(host: String?) async throws -> AgentsList {
        await ran(host == nil ? "agents" : "agents --host", host: host)
        return try Fixtures.decode(Fixtures.agents)
    }
    func vaultList(host: String?) async throws -> VaultList {
        await ran("vault list", host: host)
        return try Fixtures.decode(Fixtures.studioVault)
    }
    func audit(host: String?, before: String?) async throws -> AuditPage {
        await ran("audit --before", host: host)
        return AuditPage(records: [], hasOlder: true)
    }
    nonisolated func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
        counted.withLock { $0.append(("audit --follow", host)) }
        // One connection, with more before it.
        return AsyncThrowingStream { continuation in
            for line in [#"{"type":"snapshot","cursor":"x","has_older":true}"#, PagedClient.line(1, op: "connect"),
                         #"{"type":"caught_up","cursor":"y","has_older":true}"#] {
                continuation.yield(try! LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)))
            }
        }
    }
    func devices() async throws -> DeviceList {
        await ran("devices")
        return try Fixtures.decode(Fixtures.devices)
    }
    /// latch's refusal of an enable, when a test gives one.
    var enableRefusal: LatchFailure?
    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        await ran("plugins enable \(plugin)")
        if let enableRefusal { throw LatchError.failed(enableRefusal) }
        return nil
    }
}

/// Each screen's reads are the model's: only what the screen uses, once.
@MainActor
struct EntryTests {
    private func app(_ client: CountingClient) throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return app
    }

    @Test(arguments: [Place.host(.home), .host(.settings), .host(.rules), .host(.vault), .account(.agents), .account(.connections),
                      .host(.activity)])
    func comingBackToALoadedScreenStartsNoLatch(_ place: Place) async throws {
        let client = CountingClient()
        let app = try app(client)
        app.place = place
        for host in [app.hosts[0], app.hosts[1]] {
            app.chosenHost = host.name
            await app.enter(app.route)
            let first = client.calls.count
            await app.enter(app.route)
            #expect(client.calls.count == first, "\(place) on \(host.name): \(client.calls.dropFirst(first))")
        }
    }

    @Test func eachScreenReadsOnlyWhatItShows() async throws {
        let client = CountingClient()
        let app = try app(client)
        let mac = app.hosts[0], kitchen = app.hosts[1]
        // This Mac's plugins are in its own status, which the app holds.
        await app.enter(.host(.settings, mac))
        #expect(client.calls == ["rules show"])
        await app.enter(.host(.settings, kitchen))
        #expect(client.calls.dropFirst(1).sorted() == ["rules show", "status --host"])
        await app.enter(.host(.activity, mac))
        #expect(client.calls.count == 3)
        await app.enter(.host(.home, mac))
        #expect(client.calls.dropFirst(3) == ["vault list"])
        // Another host's are in its status, which Settings already read.
        await app.enter(.host(.home, kitchen))
        #expect(client.calls.dropFirst(4) == ["vault list"])
    }

    // Home on a host asks only that host, never the others it could: its
    // summary reads, its activity's stream and the pages before it.
    @Test func homeAsksOnlyItsHost() async throws {
        let client = CountingClient()
        let app = try app(client)
        let mac = app.hosts[0], kitchen = app.hosts[1]
        var first = 0
        for (host, asked) in [(mac, nil), (kitchen, kitchen.name)] as [(HostEntry, String?)] {
            await app.enter(.host(.home, host))
            let feed = app.feed(for: host)
            feed.start()
            await waitUntil { feed.caughtUp }
            await feed.loadOlder(until: 5, pages: ActivityFeed.homePages)
            feed.end()
            let calls = client.calls.dropFirst(first)
            #expect(calls.contains("audit --follow") && calls.contains("audit --before"), "\(calls)")
            #expect(client.hostsAsked.dropFirst(first).allSatisfy { $0 == asked }, "\(host.name): \(client.hostsAsked)")
            first = client.calls.count
        }
    }

    @Test func refreshReadsAgainWhatIsHeld() async throws {
        let client = CountingClient()
        let app = try app(client)
        await app.enter(.host(.home, app.host))
        await app.enter(.host(.home, app.host), refresh: true)
        #expect(client.calls.sorted() == ["rules show", "rules show", "vault list", "vault list"])
    }

    // Leaving a screen while its reads run (side by side): what was held
    // stays, and nothing they answer late is kept or called a failure.
    @Test func leavingCancelsTheReadsUnderWay() async throws {
        let client = CountingClient()
        let gate = Gate()
        client.held["rules show"] = gate
        client.held["vault list"] = gate
        let app = try app(client)
        let entering = Task { await app.enter(.host(.home, app.host)) }
        await waitUntil { client.calls.sorted() == ["rules show", "vault list"] }
        entering.cancel()
        gate.open()
        await entering.value
        #expect(client.calls.sorted() == ["rules show", "vault list"])
        #expect(app.summaries[app.hosts[0].name]?.rules == nil)
        #expect(app.summaries[app.hosts[0].name]?.vault == nil)
    }

    @Test func aCancelledReadIsNoFailure() async {
        let cancelled: Result<Int, LatchError>? = await Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return await AppModel.result { throw CancellationError() }
        }.value
        #expect(cancelled == nil)
        let failed = await AppModel.result { () throws -> Int in throw LatchError.unreachable("no") }
        #expect(failed == .failure(.unreachable("no")))
    }

    // Leaving Agents while it reads each host's standing, then coming back:
    // the hosts it didn't get to are read then, not left "Checking…".
    @Test func comingBackToAgentsReadsTheStandingItDidntGet() async throws {
        let client = CountingClient()
        let gate = Gate()
        client.held["agents --host"] = gate
        let app = try app(client)
        let entering = Task { await app.enter(.account(.agents)) }
        await waitUntil { client.calls.contains("agents --host") }
        entering.cancel()
        gate.open()
        await entering.value
        await app.enter(.account(.agents))
        let reachable = app.hosts.filter { $0.isThisMac || $0.online != false }
        #expect(!reachable.isEmpty)
        #expect(reachable.allSatisfy { AppModel.held(app.standings[$0.name]) })
    }

    @Test func homeAndActivityShareTheirHostsFeedAndOtherScreensHaveNone() throws {
        let app = try app(CountingClient())
        app.place = .host(.home)
        let home = try #require(app.activityFeed)
        app.place = .host(.activity)
        #expect(app.activityFeed === home)
        app.place = .host(.rules)
        #expect(app.activityFeed == nil)
        app.place = .host(.activity)
        app.chosenHost = "pi-kitchen"
        #expect(app.activityFeed !== home)
    }
}

/// A feed keeps running for its grace after the last screen showing it
/// goes; the window closing ends it at once; it picks up where it was.
@MainActor
struct FeedGraceTests {
    /// Streams one record and waits; counts each start and where it began.
    final class Following: LatchClient, @unchecked Sendable {
        private let started = Mutex<[String?]>([])
        var afters: [String?] { started.withLock { $0 } }

        func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
            started.withLock { $0.append(after) }
            return AsyncThrowingStream { continuation in
                continuation.yield(try! LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line(after == nil ? 1 : 2, op: "exec").utf8)))
                continuation.yield(.caughtUp(cursor: String(format: "%064d", after == nil ? 1 : 2), hasOlder: false))
            }
        }
    }

    @Test func comingBackWithinTheGraceKeepsTheStreamAndAfterItPicksUpWhereItWas() async {
        let client = Following()
        let feed = ActivityFeed(client: client, host: nil)
        feed.grace = .milliseconds(40)
        feed.start()
        await waitUntil { feed.caughtUp }
        feed.stop()
        #expect(feed.running)
        feed.start()
        #expect(client.afters.count == 1)
        feed.stop()
        await waitUntil { !feed.running }
        #expect(feed.records.count == 1)
        feed.start()
        await waitUntil { feed.records.count == 2 }
        #expect(client.afters == [nil, String(format: "%064d", 1)])
        feed.end()
    }

    @Test func theWindowClosingEndsEveryFeedAtOnce() async throws {
        let app = AppModel(client: Following(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        let feed = try #require(app.activityFeed)
        feed.start()
        await waitUntil { feed.caughtUp }
        app.endFeeds()
        #expect(!feed.running)
    }

    // Leaving Home while it reaches back: the cancelled page is no failure
    // to show on coming back.
    @Test func aCancelledEarlierPageIsNoFailure() async {
        let client = PagedClient()
        client.olderFails = CancellationError()
        let feed = ActivityFeed(client: client, host: nil)
        feed.stage(client.newest)
        await feed.loadOlder()
        #expect(feed.error == nil)
    }

    @Test func homeReachesBackOnlyAFewPagesOnItsOwn() async {
        let client = PagedClient()
        client.olderOp = "connect"
        client.olderHasMore = true
        let feed = ActivityFeed(client: client, host: nil)
        feed.stage(PagedClient().newest)
        await feed.loadOlder(until: 5, pages: ActivityFeed.homePages)
        #expect(client.pages == ActivityFeed.homePages)
        #expect(feed.hasOlder)
    }
}

/// Back in front, the app reads again the screen showing, whichever it is.
@MainActor
struct ActivationTests {
    @Test(arguments: [HostScreen.rules, .vault, .home])
    func comingToTheFrontReadsTheVisibleScreenAgain(_ section: HostScreen) async throws {
        let client = CountingClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(section)
        await app.enter(app.route)
        let first = client.calls
        await app.activated()
        let again = Array(client.calls.dropFirst(first.count))
        #expect(again.contains(section == .vault ? "vault list" : "rules show"), "\(section): \(again)")
    }

    // Rules names the agents but doesn't show their standing on each host:
    // neither showing it nor coming back to the front asks each host.
    @Test func rulesReadsTheAgentListAndNoHostsStanding() async throws {
        let client = CountingClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(.rules)
        await app.enter(.host(.rules, app.host))
        await app.activated()
        #expect(client.calls.sorted() == ["agents", "agents", "rules show", "rules show"])
        #expect(!client.calls.contains("agents --host"))
    }
}

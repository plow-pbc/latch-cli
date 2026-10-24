import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// A latch that counts every process the app would start, by command; a
/// held command waits for its gate.
final class CountingClient: LatchClient, @unchecked Sendable {
    private let counted = Mutex<[String]>([])
    var held: [String: Gate] = [:]
    var calls: [String] { counted.withLock { $0 } }

    private func ran(_ command: String) async {
        counted.withLock { $0.append(command) }
        await held[command]?.wait()
    }

    func hostStatus(_ host: String) async throws -> LocalStatus {
        await ran("status --host")
        return try Fixtures.decode(Fixtures.serving)
    }
    func setupStatus() async throws -> SetupStatus {
        await ran("setup --status")
        return try Fixtures.decode(Fixtures.setup(on: []))
    }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow {
        await ran("rules show")
        return try Fixtures.decode(Fixtures.studioRules)
    }
    func agents(host: String?) async throws -> AgentsList {
        await ran(host == nil ? "agents" : "agents --host")
        return try Fixtures.decode(Fixtures.agents)
    }
    func pluginsList(host: String?) async throws -> PluginsList {
        await ran("plugins list")
        return try Fixtures.decode(Fixtures.studioPlugins)
    }
    func vaultList(host: String?) async throws -> VaultList {
        await ran("vault list")
        return try Fixtures.decode(Fixtures.studioVault)
    }
    func devices() async throws -> DeviceList {
        await ran("devices")
        return try Fixtures.decode(Fixtures.devices)
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
        await app.enter(.host(.settings, mac))
        #expect(client.calls == ["rules show", "plugins list"])
        await app.enter(.host(.settings, kitchen))
        #expect(Array(client.calls.dropFirst(2)) == ["rules show", "plugins list", "status --host"])
        await app.enter(.host(.activity, mac))
        #expect(client.calls.count == 5)
        await app.enter(.host(.home, mac))
        #expect(client.calls.last == "vault list")
        #expect(client.calls.count == 6)
    }

    @Test func refreshReadsAgainWhatIsHeld() async throws {
        let client = CountingClient()
        let app = try app(client)
        await app.enter(.host(.home, app.host))
        await app.enter(.host(.home, app.host), refresh: true)
        #expect(client.calls == ["rules show", "vault list", "plugins list", "rules show", "vault list", "plugins list"])
    }

    // Leaving a screen while its read runs: what was held stays, nothing
    // is called a failure, and the reads after it never start.
    @Test func leavingCancelsTheReadAndTheOnesAfterIt() async throws {
        let client = CountingClient()
        let gate = Gate()
        client.held["rules show"] = gate
        let app = try app(client)
        let entering = Task { await app.enter(.host(.home, app.host)) }
        await waitUntil { client.calls == ["rules show"] }
        entering.cancel()
        gate.open()
        await entering.value
        #expect(client.calls == ["rules show"])
        #expect(app.summaries[app.hosts[0].name]?.rules == nil)
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

        func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> {
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
        #expect(client.calls == ["agents", "rules show", "agents", "rules show"])
        #expect(!client.calls.contains("agents --host"))
    }
}

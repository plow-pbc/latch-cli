import Foundation
import Testing
@testable import PlowLatchUI

/// Where the main window goes: an agent's activity on exactly the host
/// asked for, account pages on no host, and reads only for the route that
/// asked.
@MainActor
struct TargetTests {
    private let scout = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd", helper = "3971582e59baad83250141c8dcc95855"

    private func app(_ client: any LatchClient = FakeLatchClient()) throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.stage(agents: try Fixtures.decode(Fixtures.agents), standings: [:])
        return app
    }

    @Test func activityOpensOnTheHostAndAgentAskedForWithEveryOutcomeAndNoSearch() throws {
        let app = try app()
        app.place = .account(.agents)
        app.activityFilter = .refused
        app.activitySearch = "Finance"
        let kitchen = try #require(app.hosts.first { $0.name == "pi-kitchen" })
        app.openActivity(kitchen, agent: scout)
        #expect(app.route == .host(.activity, kitchen))
        #expect(app.activityAgent(on: kitchen) == scout)
        #expect(app.activityFilter == .all && app.activitySearch.isEmpty)
        #expect(app.activityAgent(on: try #require(app.hosts.first { $0.name == "studio" })) == nil)
    }

    @Test(arguments: AccountPage.allCases)
    func anAccountPageHasNoHostWhicheverIsPicked(_ page: AccountPage) throws {
        let app = try app()
        app.chosenHost = "pi-kitchen"
        app.place = .account(page)
        #expect(app.route == .account(page))
        #expect(app.activityFeed == nil)
    }

    // Agents asks each host it can for its standing: hosts Plow lists
    // after Agents showed are asked then; picking a host asks nothing.
    @Test func agentsReadsAgainForHostsListedLaterButNotForAPickedHost() async throws {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.place = .account(.agents)
        let before = app.entry
        await app.enter(app.route)
        #expect(app.standings.keys.sorted() == ["studio"])
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        #expect(app.entry != before)
        await app.enter(app.route)
        #expect(app.standings.keys.sorted() == ["pi-kitchen", "sidecar", "studio"])
        let listed = app.entry
        app.chosenHost = "pi-kitchen"
        #expect(app.entry == listed)
        app.place = .account(.connections)
        let connections = app.entry
        app.chosenHost = "studio"
        #expect(app.entry == connections)
    }

    @Test func connectionsReadsOnlyTheAccount() async throws {
        let client = CountingClient()
        let app = try app(client)
        app.chosenHost = "pi-kitchen"
        app.place = .account(.connections)
        await app.enter(app.route)
        #expect(client.calls == ["setup --status", "devices"])
    }

    // One host's read is cancelled for another's: whatever it answers late
    // lands nowhere, and the other host's read is what's held.
    @Test func aCancelledReadCantOverwriteAnotherTarget() async throws {
        let client = CountingClient()
        let gate = Gate()
        for command in ["vault list", "status --host"] { client.held[command] = gate }
        let app = try app(client)
        app.stage(summary: HostSummary(), for: "studio")
        app.stage(summary: HostSummary(), for: "pi-kitchen")
        let mac = app.hosts[0], kitchen = app.hosts[1]
        let first = Task { await app.enter(.host(.home, kitchen)) }
        await waitUntil { client.calls.count == 2 }
        first.cancel()
        let second = Task { await app.enter(.host(.home, mac)) }
        await waitUntil { client.calls.count == 3 }
        gate.open()
        await first.value
        await second.value
        #expect(app.summaries["pi-kitchen"] == HostSummary())
        #expect(AppModel.held(app.summaries["studio"]?.vault))
    }
}

/// Activity's scope bar narrows the records loaded so far; it never
/// starts the stream again, and never reads history of its own.
@MainActor
struct ActivityScopeTests {
    /// scout's uid on studio's records, some of which carry no name.
    private let scout = "a74f38668570a45ddc4b762ba0465d74"

    private func app(_ client: any LatchClient = FakeLatchClient()) throws -> (AppModel, HostEntry) {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        let studio = try #require(app.host)
        app.feed(for: studio).stage(Fixtures.studioAudit)
        return (app, studio)
    }

    @Test func theAgentOutcomeAndSearchAllNarrowAndTheAgentIsItsUid() throws {
        let (app, studio) = try app()
        let records = app.feed(for: studio).records
        app.openActivity(studio, agent: scout)
        let agents = app.activityRows(records, on: studio)
        #expect(!agents.isEmpty && agents.allSatisfy { $0.agent == scout })
        #expect(agents.contains { $0.callerName == nil })
        #expect(agents.count == records.filter { $0.agent == scout && !$0.isBackground }.count)
        app.activityFilter = .refused
        let refused = app.activityRows(records, on: studio)
        #expect(!refused.isEmpty && refused.allSatisfy { $0.agent == scout && $0.isRefused })
        app.activitySearch = "finance"
        let found = app.activityRows(records, on: studio)
        #expect(!found.isEmpty && found.count < refused.count && found.allSatisfy { $0.command?.contains("Finance") == true })
        #expect(found.map(\.time) == found.map(\.time).sorted(by: >))
    }

    @Test func nothingMatchingIsNoMatchNotNoHistory() throws {
        let (app, studio) = try app()
        let records = app.feed(for: studio).records
        app.openActivity(studio, agent: scout)
        app.activitySearch = "Taxes"
        #expect(app.activityRows(records, on: studio).isEmpty)
        #expect(!records.isEmpty)
        app.activityAgent = nil
        app.activitySearch = ""
        #expect(app.activityRows(records, on: studio).count == records.filter { !$0.isBackground }.count)
    }

    @Test func aSelectionFilteredOutIsNotShownAndComesBackWithIt() throws {
        let (app, studio) = try app()
        let records = app.feed(for: studio).records
        let allowed = try #require(records.last { $0.agent == scout && $0.decision == "allow" && !$0.isBackground })
        app.selectedRecord = OnHost(host: "studio", id: allowed.id)
        #expect(app.inspectedRecord(on: studio) == allowed)
        app.activityFilter = .refused
        #expect(app.inspectedRecord(on: studio) == nil)
        app.activityFilter = .all
        #expect(app.inspectedRecord(on: studio) == allowed)
    }

    // Home's recent request opens in Activity even where a filter left
    // there would hide it, in the inspector even if it was closed; filters
    // it passes are kept.
    @Test func aRecentRequestFromHomeIsShownWhateverWasFiltered() throws {
        let (app, studio) = try app()
        let records = app.feed(for: studio).records
        let other = try #require(records.last { $0.agent == nil && !$0.isBackground && !$0.isRefused })
        app.openActivity(studio, agent: scout)
        app.activityFilter = .refused
        app.activitySearch = "Finance"
        app.inspecting.remove(.activity)
        app.place = .host(.home)
        app.openRecord(other, on: studio)
        #expect(app.route == .host(.activity, studio))
        #expect(app.inspectedRecord(on: studio) == other)
        #expect(app.inspecting.contains(.activity))

        let refused = try #require(records.last { $0.agent == scout && $0.isRefused && $0.command?.contains("Finance") == true })
        app.openActivity(studio, agent: scout)
        app.activityFilter = .refused
        app.openRecord(refused, on: studio)
        #expect(app.activityFilter == .refused && app.activityAgent(on: studio) == scout)
        #expect(app.inspectedRecord(on: studio) == refused)
    }

    // Home's Requests and Refused: all of the host's Activity, or what was
    // refused, whatever filters were left there: exactly what Home counted.
    @Test func homesCountsOpenAllOfActivityOrWhatWasRefused() throws {
        let (app, studio) = try app()
        let records = app.feed(for: studio).records
        #expect(records.contains { $0.isBackground })
        app.openActivity(studio, agent: scout)
        app.activityFilter = .allowed
        app.activitySearch = "Finance"
        app.showConnections = true
        app.place = .host(.home)
        app.openActivity(studio)
        #expect(app.activityRows(records, on: studio).count == records.filter { !$0.isBackground }.count)

        app.showConnections = true
        app.place = .host(.home)
        app.openActivity(studio, showing: .refused)
        #expect(app.route == .host(.activity, studio))
        let refused = records.filter { !$0.isBackground && $0.isRefused }
        #expect(!refused.isEmpty)
        #expect(app.activityRows(records, on: studio).count == refused.count)
    }

    @Test func filteringNeverStartsTheStreamAgain() async throws {
        let client = PagedClient()
        let newest = client.newest
        client.following = { _ in
            AsyncThrowingStream { continuation in
                for line in newest.split(separator: "\n") {
                    continuation.yield(try! LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)))
                }
            }
        }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.place = .host(.activity)
        let feed = try #require(app.activityFeed)
        feed.start()
        await waitUntil { feed.caughtUp }
        let studio = try #require(app.host)
        app.openActivity(studio, agent: scout)
        app.activityFilter = .refused
        app.activitySearch = "ls"
        app.showConnections = true
        app.activityAgent = nil
        #expect(app.activityFeed === feed)
        #expect(client.follows == 1)
        feed.end()
    }

    // Earlier records join what's loaded under the same filters, and the
    // stream picks up where it was, not where the earlier page began.
    @Test func loadingEarlierKeepsTheFiltersAndWhereTheStreamWas() async throws {
        let client = PagedClient()
        let feed = ActivityFeed(client: client, host: nil)
        feed.stage(client.newest)
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        let studio = try #require(app.host)
        app.activityFilter = .allowed
        app.activitySearch = "laptop"
        #expect(app.activityRows(feed.records, on: studio).isEmpty)
        await feed.loadOlder()
        #expect(app.activityFilter == .allowed && app.activitySearch == "laptop")
        #expect(app.activityRows(feed.records, on: studio).count == 6)
        feed.start()
        await waitUntil { client.follows == 1 }
        #expect(client.afters == ["y"])
        feed.end()
    }
}

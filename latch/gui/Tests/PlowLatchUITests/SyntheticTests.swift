import Foundation
import Testing
@testable import PlowLatchUI

/// The made-up latch a measured test app runs on: the sizes it is asked
/// for, through the app's own decoding.
@MainActor
struct SyntheticTests {
    @Test func aSpecIsReadOrRefused() {
        #expect(SyntheticLatchClient.Spec("") == SyntheticLatchClient.Spec())
        let spec = SyntheticLatchClient.Spec("hosts=8,records=10000,vault=1000,agents=5,rate=10,delay=40,jitter=20,connectors=1500")
        #expect(spec?.hosts == 8 && spec?.records == 10000 && spec?.vault == 1000 && spec?.agents == 5 && spec?.rate == 10 && spec?.delay == 40)
        #expect(spec?.jitter == 20)
        #expect(spec?.connectors == 1500)
        #expect(SyntheticLatchClient.Spec("hosts=9") == nil)
        #expect(SyntheticLatchClient.Spec("hosts=0") == nil)
        #expect(SyntheticLatchClient.Spec("colour=blue") == nil)
        #expect(SyntheticLatchClient.Spec("records=-1") == nil)
    }

    @Test func eightHostsInEveryStateWithTheirRecordsVaultAndAgents() async throws {
        let client = SyntheticLatchClient(try #require(.init("hosts=8,records=10000,vault=1000,agents=5")), stateDir: "/s")
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        await app.refreshStatus()
        await app.refreshAccount()
        #expect(app.hosts.count == 8)
        #expect(app.hosts.first?.isThisMac == true && !app.viewerOnly)
        #expect(Set(app.hosts.map { HostState($0, local: app.status) }) == [.serving, .online, .offline, .unknown])
        #expect(app.hosts.contains { $0.name.count > 30 })

        let feed = app.feed(for: app.hosts[1])
        feed.start()
        await waitUntil(within: .seconds(30)) { feed.caughtUp }
        // The newest 500, with older ones on the host.
        #expect(feed.records.count == ActivityFeed.openingPage)
        #expect(Set(feed.records.map(\.id)).count == ActivityFeed.openingPage)
        #expect(feed.hasOlder)
        // Every agent has requests allowed and refused, not one kind each.
        for uid in Set(feed.records.compactMap(\.agent)) {
            let own = feed.records.filter { $0.agent == uid }
            #expect(own.contains { $0.isRefused } && own.contains { !$0.isRefused && !$0.isBackground } && own.contains(where: \.isBackground))
        }
        #expect(Set(feed.records.compactMap(\.agent)).count == 5)
        feed.end()

        #expect(try await client.vaultList(host: "nas").items.count == 1000)
        #expect(try await client.vaultShow("v998", host: nil).item.title == "Note 998 (synthetic)")
        let agents = try await client.agents(host: "mini").agents
        #expect(agents.count == 5 && agents.allSatisfy { $0.standing != nil })
        #expect(try await client.rulesShow(host: "mini").agents?.count == 3)
        #expect(app.plugins(of: app.hosts[0])?.map(\.count) == .success(3))
        #expect(try await client.hostStatus("mini").plugins?.count == 3)
    }

    // With connectors, a plugin turned on stays on, busy while it turns;
    // Google has an account that works, and a sign-in adds another.
    @Test func connectorChangesHoldAndGoogleSignsIn() async throws {
        let client = SyntheticLatchClient(try #require(.init("hosts=3,connectors=200")), stateDir: "/s")
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        await app.refreshStatus()
        await app.refreshAccount()
        let mac = try #require(app.hosts.first)
        let change = Task { await app.setConnector("messages", on: true) }
        await waitUntil { app.connectorChange != nil }
        #expect(app.connectorChange == AppModel.ConnectorChange(plugin: "messages", on: true))
        await change.value
        #expect(app.connectorChange == nil && app.connectorError == nil)
        #expect(try app.plugins(of: mac)?.get().first { $0.name == "messages" }?.state == "enabled")
        await app.setConnector("messages", on: false)
        #expect(try app.plugins(of: mac)?.get().first { $0.name == "messages" }?.state == "disabled")

        await app.refreshConnections()
        #expect(app.connections == .google(GoogleAccounts(accounts: ["morgan@example.com"])))
        var events: [GoogleSignInEvent] = []
        for try await event in client.signInGoogle(addAccount: true) { events.append(event) }
        #expect(events == [.link(Fixtures.signInLink), .enabled(GoogleEnabled(accounts: ["morgan@example.com", "morgan2@example.com"]))])
        await app.refreshConnections()
        #expect(app.connections == .google(GoogleAccounts(accounts: ["morgan@example.com", "morgan2@example.com"])))
        #expect(try app.plugins(of: mac)?.get().first { $0.name == "google" }?.state == "disabled")
        await app.refreshStatus()
        #expect(try app.plugins(of: mac)?.get().first { $0.name == "google" }?.state == "enabled")
    }

    // Coming back after the grace, the stream picks up after the last
    // record held: new records at once, none held already.
    @Test func aFollowPicksUpAfterItsCursor() async throws {
        let client = SyntheticLatchClient(try #require(.init("records=10")), stateDir: "/s")
        // Up to caught up: a follow then waits for more.
        func lines(after: String?) async throws -> [AuditLine] {
            var lines: [AuditLine] = []
            for try await line in client.auditFollow(host: nil, after: after, last: nil) {
                lines.append(line)
                if case .caughtUp = line { break }
            }
            return lines
        }
        let first = try await lines(after: nil)
        guard case let .caughtUp(cursor, _)? = first.last, case let .record(_, newest)? = first.dropLast().last else {
            Issue.record("no records then caught up: \(first)")
            return
        }
        #expect(cursor == newest.id)
        func records(_ lines: [AuditLine]) -> [AuditRecord] { lines.compactMap { if case let .record(_, record) = $0 { record } else { nil } } }
        let rest = records(try await lines(after: records(first)[5].id))
        #expect(rest.count == 4 && rest.last == newest)
        #expect(records(try await lines(after: cursor)).isEmpty)
    }

    @Test func newRecordsArriveAtTheRateAsked() async throws {
        let client = SyntheticLatchClient(try #require(.init("records=0,rate=100")), stateDir: "/s")
        let feed = ActivityFeed(client: client, host: nil)
        feed.start()
        await waitUntil { feed.records.count >= 10 }
        feed.end()
    }
}

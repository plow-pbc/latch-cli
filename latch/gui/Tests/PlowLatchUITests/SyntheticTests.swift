import Foundation
import Testing
@testable import PlowLatchUI

/// The made-up latch a measured test app runs on: the sizes it is asked
/// for, through the app's own decoding.
@MainActor
struct SyntheticTests {
    @Test func aSpecIsReadOrRefused() {
        #expect(SyntheticLatchClient.Spec("") == SyntheticLatchClient.Spec())
        let spec = SyntheticLatchClient.Spec("hosts=8,records=10000,vault=1000,agents=5,rate=10,delay=40")
        #expect(spec?.hosts == 8 && spec?.records == 10000 && spec?.vault == 1000 && spec?.agents == 5 && spec?.rate == 10 && spec?.delay == 40)
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
        #expect(feed.records.count == 10000)
        #expect(Set(feed.records.map(\.id)).count == 10000)
        #expect(feed.records.contains { $0.isRefused } && feed.records.contains { $0.isBackground })
        feed.end()

        #expect(try await client.vaultList(host: "nas").items.count == 1000)
        #expect(try await client.vaultShow("v998", host: nil).item.title == "Note 998 (synthetic)")
        let agents = try await client.agents(host: "mini").agents
        #expect(agents.count == 5 && agents.allSatisfy { $0.standing != nil })
        #expect(try await client.rulesShow(host: "mini").agents?.count == 3)
    }

    @Test func newRecordsArriveAtTheRateAsked() async throws {
        let client = SyntheticLatchClient(try #require(.init("records=0,rate=100")), stateDir: "/s")
        let feed = ActivityFeed(client: client, host: nil)
        feed.start()
        await waitUntil { feed.records.count >= 10 }
        feed.end()
    }
}

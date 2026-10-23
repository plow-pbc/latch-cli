import Testing
@testable import PlowLatchUI

@MainActor
struct HostsTests {
    private func app(status: String, account: String) throws -> AppModel {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(status))
        app.stage(account: try Fixtures.decode(account))
        return app
    }

    @Test func thisMacComesFirstAndIsTheDefaultHost() throws {
        let app = try app(status: Fixtures.serving, account: Fixtures.studioHosts)
        #expect(app.hosts.map(\.name) == ["studio", "pi-kitchen", "sidecar"])
        #expect(app.host?.isThisMac == true)
        #expect(!app.viewerOnly)
    }

    @Test func pickingAHostChangesTheHostTierOnly() throws {
        let app = try app(status: Fixtures.serving, account: Fixtures.studioHosts)
        app.section = .rules
        app.chosenHost = "pi-kitchen"
        #expect(app.host?.name == "pi-kitchen")
        #expect(app.section == .rules)
    }

    // Without Plow's list, a stopped Mac is still this Mac's host: its
    // own state, rules and vault are read locally, and it can be started.
    @Test func aStoppedMacStaysInThePickerWhilePlowIsDown() async throws {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped),
                           store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        app.stage(account: nil, error: "Plow couldn't be reached")
        #expect(app.hosts.map(\.name) == ["studio"])
        #expect(app.host?.isThisMac == true)
        #expect(!app.viewerOnly)
        await app.refreshSummary()
        #expect(app.summaries["studio"]?.rules != nil)
        #expect(app.summaries["studio"]?.vault != nil)
    }

    @Test func aMacThatNeverServedOnlyViews() throws {
        let app = try app(status: Fixtures.laptop, account: Fixtures.laptopHosts)
        #expect(app.viewerOnly)
        #expect(app.hosts.allSatisfy { !$0.isThisMac })
    }

    // Plow gives no last-seen time for a host it counts offline; that
    // doesn't mean it was never seen.
    @Test func anOfflineHostClaimsNoLastSeen() throws {
        let app = try app(status: Fixtures.laptop, account: Fixtures.laptopHostsPiOffline)
        let pi = try #require(app.hosts.first { $0.name == "pi-kitchen" })
        #expect(pi.subtitle(local: app.status) == "Offline")
    }
}

@MainActor
struct HostSummaryTests {
    // A stopped Mac is offline to Plow, but its rules and vault are still
    // read from its own state folder.
    @Test func thisMacIsReadWhilePlowCountsItOffline() async throws {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped),
                           store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts.replacingOccurrences(
            of: #""online":true,"self":true"#, with: #""online":false,"self":true"#)))
        #expect(app.host?.isThisMac == true && app.host?.online == false)
        await app.refreshSummary()
        #expect(app.summaries["studio"]?.rules != nil)
    }
}

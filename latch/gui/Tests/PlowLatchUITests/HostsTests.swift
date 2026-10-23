import Testing
@testable import PlowLatchUI

private let unreachable = LatchError.failed(LatchFailure(
    code: "plow_unreachable", message: "could not reach Plow at http://127.0.0.1:9: error sending request"))

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

    // A stopped Mac is this Mac's host whether Plow is unreachable (only
    // its list could say the Mac never served) or counts it offline: its
    // rules and vault are read locally, and it can be started.
    @Test(arguments: [false, true])
    func aStoppedMacRemainsLocallyReadable(plowAnswered: Bool) async throws {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped),
                           store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.stopped))
        let offline: AccountStatus = try Fixtures.decode(Fixtures.studioHosts.replacingOccurrences(
            of: #""online":true,"self":true"#, with: #""online":false,"self":true"#))
        app.stage(account: plowAnswered ? offline : nil, error: plowAnswered ? nil : unreachable)
        #expect(app.hosts.map(\.name) == (plowAnswered ? ["studio", "pi-kitchen", "sidecar"] : ["studio"]))
        #expect(app.host?.isThisMac == true && !app.viewerOnly)
        await app.refreshSummary()
        #expect(app.summaries["studio"]?.rules != nil)
        #expect(app.summaries["studio"]?.vault != nil)
    }

    @Test func homeSaysPlainlyThatPlowIsOutOfReach() throws {
        let app = try app(status: Fixtures.serving, account: Fixtures.studioHosts)
        app.stage(account: nil, error: unreachable)
        #expect(app.plowNotice?.title == "Plow can't be reached right now.")
        #expect(app.plowNotice?.detail.contains("can't be listed") == true)
        #expect(app.plowNotice?.detail.contains("127.0.0.1") == false)
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts), error: unreachable)
        #expect(app.plowNotice?.detail.contains("as Plow last listed them") == true)
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        #expect(app.plowNotice == nil)
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

/// The app's latch is told its target in arguments; a `LATCH_HOST` or
/// `LATCH_PLOW_CHAT` the app inherited (a terminal launch, `launchctl
/// setenv`) must not send its reads elsewhere.
struct ChildEnvironmentTests {
    @Test func targetSelectingVariablesAreNotPassedOn() {
        let child = LiveLatchClient.childEnvironment(["LATCH_HOST": "pi-kitchen", "LATCH_PLOW_CHAT": "c1", "HOME": "/Users/me", "PATH": "/bin"])
        #expect(child == ["HOME": "/Users/me", "PATH": "/bin"])
    }
}

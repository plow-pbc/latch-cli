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

@MainActor
struct HostSettingsTests {
    @Test func loggedInDevicesAreTheAccountsMachinesThatDontHost() throws {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.stage(devices: try Fixtures.decode(Fixtures.devices), update: nil)
        #expect(app.loggedInDevices?.map(\.name) == ["laptop"])
        let check: UpdateCheck = try Fixtures.decode(Fixtures.updateCheck)
        #expect(!check.available && check.current == nil && check.latest?.tag != nil)
    }

    /// A released latch's `update --check` gives its build as a number.
    @Test func aReleasedBuildsUpdateCheckDecodes() throws {
        let check: UpdateCheck = try Fixtures.decode(#"{"available":true,"current":137,"latest":{"build":138,"tag":"v0.1.0-138"}}"#)
        #expect(check.available && check.current == 137 && check.latest?.build == 138)
    }
}

/// A Mac that logs out while Plow can't be told, and an agent Plow starts.
final class AccountClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    private(set) var loggedOut = false
    /// Whether Plow takes the Mac out of the account at logout.
    var plowRemoves = false

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(loggedOut ? Fixtures.loggedOut : Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(Fixtures.rulesStudio) }
    func rulesSet(_ text: String) async throws {}
    func pluginsList(host: String?) async throws -> PluginsList { try Fixtures.decode(#"{"plugins":[]}"#) }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
    func agents(host: String?) async throws -> AgentsList { try Fixtures.decode(Fixtures.agents) }
    func agentShow(_ uid: String) async throws -> AgentsList.Agent {
        var agent = try #require((try Fixtures.decode(Fixtures.agents) as AgentsList).agents.first { $0.uid == uid })
        agent.status = "running"
        return agent
    }
    func logout() async throws -> LogoutResult {
        loggedOut = true
        if plowRemoves { return try Fixtures.decode(#"{"device":{"name":"studio","removed":true},"purged":false,"stopped":true}"#) }
        return try Fixtures.decode(#"{"device":{"name":"studio","removed":false,"error":{"code":"plow_unreachable","message":"could not reach Plow"},"next":"`latch devices remove studio` on another of your machines"},"purged":false,"stopped":true}"#)
    }
}

@MainActor
struct AccountTests {
    @Test func aLogoutPlowDidntHearOfIsSaidBeforeOnboarding() async throws {
        let app = AppModel(client: AccountClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        try await app.logOut()
        #expect(app.logOutWarning?.contains("`latch devices remove studio` on another of your machines") == true)
        #expect(app.onboarding == nil)
        app.acknowledgeLogOutWarning()
        #expect(app.onboarding != nil)
    }

    /// The status stream said "logged out" while the logout still ran.
    @Test func aLogoutTheStreamAnnouncedFirstStillOpensOnboarding() async throws {
        let client = AccountClient()
        client.plowRemoves = true
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        try await app.logOut()
        #expect(app.logOutWarning == nil)
        #expect(app.onboarding != nil)
    }

    @Test func aStartingAgentsNewStatusReachesTheAgentsList() async throws {
        let app = AppModel(client: AccountClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        await app.refreshAgents()
        let scout = try #require(app.agents?.first)
        _ = try await app.agentStatus(scout.uid)
        #expect(app.agents?.first?.status == "running")
    }
}

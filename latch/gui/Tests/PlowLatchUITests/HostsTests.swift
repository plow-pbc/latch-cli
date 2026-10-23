import Foundation
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
            of: #""self":true,"online":true"#, with: #""self":true,"online":false"#))
        app.stage(account: plowAnswered ? offline : nil, error: plowAnswered ? nil : unreachable)
        #expect(app.hosts.map(\.name) == (plowAnswered ? ["studio", "pi-kitchen", "sidecar"] : ["studio"]))
        #expect(app.host?.isThisMac == true && !app.viewerOnly)
        await app.enter(.home, host: app.host)
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

    /// A released host's `status --local` gives the build it found as a
    /// number, once its daily lookup has run.
    @Test func aReleasedHostsStatusDecodes() throws {
        let status: LocalStatus = try Fixtures.decode(Fixtures.serving.replacingOccurrences(
            of: #""update":{"available":false,"build":null,"checked_at":null}"#,
            with: #""update":{"available":false,"build":138,"checked_at":"2026-10-23T08:00:00Z"}"#))
        #expect(status.update?.build == 138)
    }

    /// Connections lists Google's accounts in Plow, and never takes "Plow
    /// couldn't say" for none.
    @Test func connectionsAreWhatPlowSaysOfGoogle() throws {
        func connections(_ google: String) throws -> Connections {
            Connections(try Fixtures.decode(Fixtures.setup(on: [], google: google)) as SetupStatus)
        }
        #expect(try connections(Fixtures.googleNotConnected) == .google(GoogleAccounts(accounts: [])))
        #expect(try connections(Fixtures.googleConnected(["ada@example.com", "ada.work@example.com"]))
            == .google(GoogleAccounts(accounts: ["ada@example.com", "ada.work@example.com"])))
        #expect(try connections(Fixtures.googleUnreachable) == .unknown)
        #expect(Connections(nil) == .unknown)
    }

    /// Another host's version is its own `status --local`, asked with
    /// `--host`; one Plow counts offline isn't asked.
    @Test func anotherHostsVersionIsWhatItSays() async throws {
        let app = AppModel(client: FakeLatchClient(account: Fixtures.studioHosts.replacingOccurrences(
            of: #""self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z""#,
            with: #""self":false,"online":false,"last_seen_at":null"#)),
                           store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        await app.refreshStatus()
        await app.refreshAccount()
        app.chosenHost = "pi-kitchen"
        await app.enter(.settings, host: app.host)
        #expect(try app.summaries["pi-kitchen"]?.status?.get().version?.hasPrefix("0.1.0 (138)") == true)
        app.chosenHost = "sidecar"
        await app.enter(.settings, host: app.host)
        #expect(app.summaries["sidecar"]?.status == nil)
        app.chosenHost = "studio"
        await app.enter(.settings, host: app.host)
        #expect(app.summaries["studio"]?.status == nil)
    }

    @Test func manageAccountsOpensConnections() throws {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.section = .settings
        app.manageAccounts()
        #expect(app.section == .connections)
    }

    /// latch's reasons and refusals name commands and pages; the Plugins
    /// area says them in its own words.
    @Test func pluginsAreSaidWithoutCommandsOrAddresses() throws {
        let plugins = try (Fixtures.decode(Fixtures.studioPluginsSetUp) as PluginsList).plugins
        #expect(plugins.map(PluginCopy.state) == ["On", "Needs Full Disk Access for Plow Latch, in System Settings", "Off"])
        #expect(plugins.contains { $0.reason?.contains("`") == true })
        // Google saves needs_setup for any failed setup, not only a missing account.
        let google = PluginsList.Plugin(name: "google", state: "needs_setup", reason: "gog 0.45.0 is not installed at /s/plugins/gog/0.45.0/gog; run install.sh")
        #expect(PluginCopy.state(google) == "Needs setting up")
        let refusals = [
            ("google", LatchFailure(code: "google_not_connected", message: "Plow has no Google account this host can use; connect one at http://localhost:3000/connections, then run `latch plugins enable google` again")),
            ("messages", LatchFailure(code: "needs_setup", message: "this host has no Full Disk Access; turn it on for /Applications/Plow Latch.app in System Settings (x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles), then run `latch plugins enable messages` again")),
            ("contacts", LatchFailure(code: "serve_required", message: "this machine needs a live receiver; start `latch serve` on this state folder, then retry")),
            ("google", LatchFailure(code: "gog_not_configured", message: "gog 0.45.0 is not installed at /s/plugins/gog/0.45.0/gog; run install.sh")),
        ]
        #expect(refusals.map { PluginCopy.refusal(.failed($1), plugin: $0, on: true) } == [
            "Connect a Google account first, in Connections.",
            "Needs Full Disk Access for Plow Latch, in System Settings.",
            "Couldn't turn Contacts on.",
            "Couldn't turn Google on.",
        ])
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
        #expect(app.logOutWarning == "This Mac is logged out, but Plow couldn't take studio out of your account. It may still be listed until Plow can be reached.")
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

import Observation
import SwiftUI

/// The app's state: this Mac's latch as `status --local` last said, and the
/// onboarding flow while it runs. The only owner of the client.
@Observable
final class AppModel {
    let client: any LatchClient
    /// Whichever read sets it (the status stream, a one-off read, Start
    /// Latch's own), a serving status lets this Mac's activity follow if it
    /// stopped because nothing was hosted here.
    private(set) var status: LocalStatus? {
        didSet {
            if status?.serve.state == "serving", let name = status?.peer?.name { feeds[name]?.retry() }
        }
    }
    private(set) var statusError: String?
    /// The onboarding flow, while the onboarding window is open.
    private(set) var onboarding: Onboarding?
    /// How the app hands a link to the system (Messages, System Settings);
    /// only ever on a person's click. A test build records them instead, so
    /// a run never opens anything.
    var open: (URL) -> Void = { NSWorkspace.shared.open($0) }
    /// Floats the Full Disk Access helper under System Settings, with this
    /// app to drag into the list.
    var guideGrant: (URL) -> any GrantGuide = { GrantHelper.show(dragging: $0) }
    /// Turns "Start when you log in" on or off; a test build records it.
    var setLoginItem: (Bool) throws -> Void = { try LoginItem.set($0) }
    var loginItemIsOn: () -> Bool = { LoginItem.isOn }
    /// The login item starts latch's default state folder, so it's offered
    /// only when the app runs on that folder, never on a `--state-dir` one.
    let loginItemOffered: Bool
    /// The windows SwiftUI opens and closes, set by the app's scenes.
    var openWindow: ((String) -> Void)?
    var dismissWindow: ((String) -> Void)?

    static let onboardingWindow = "onboarding"
    static let mainWindow = "main"

    /// A still, for snapshots: screens start nothing of their own.
    let still: Bool

    /// Onboarding progress for this state folder, in the app's preferences.
    let store: ProgressStore

    init(client: any LatchClient, store: ProgressStore? = nil, still: Bool = false, loginItemOffered: Bool = true) {
        self.client = client
        self.still = still
        self.loginItemOffered = loginItemOffered
        self.store = store ?? ProgressStore(defaults: .standard, stateDir: client.stateDir)
    }

    var progress: Progress { onboarding?.progress ?? store.load() }

    /// What's set up, as latch says: whether shared rules are saved, and
    /// each setup step (`setup --status`); nil where latch couldn't say.
    private(set) var rulesSaved: Bool?
    private(set) var setup: SetupStatus?

    func refreshSetup() async {
        rulesSaved = (try? await client.rulesShow()).map { $0.everyAgent != nil }
        setup = try? await client.setupStatus()
    }

    /// Whether latch says a connector is set up: on, and for Google still
    /// connected in Plow; nil before it has said.
    func connectorOn(_ name: String) -> Bool? {
        setup.map { $0.step(name)?.state == "done" }
    }

    /// For snapshots: what latch said is set up.
    func stage(rulesSaved: Bool?, setup: SetupStatus?) {
        self.rulesSaved = rulesSaved
        self.setup = setup
    }

    /// The account's hosts as `latch status` last said; it asks Plow, so it
    /// can fail where `status --local` doesn't.
    private(set) var account: AccountStatus?
    private(set) var accountError: LatchError?

    func refreshAccount() async {
        do {
            account = try await client.accountStatus()
            accountError = nil
        } catch {
            accountError = error as? LatchError ?? .unreachable(error.localizedDescription)
        }
    }

    /// What Home and the picker say while `latch status` fails.
    var plowNotice: PlowNotice? { accountError.map { PlowNotice(error: $0, listed: account != nil) } }

    /// The main window's place: a sidebar item, and the host the host tier
    /// shows (the picker's choice; nil until the person picks one).
    var section: SidebarItem = .home
    var chosenHost: String?

    /// The picker's hosts, this Mac first.
    var hosts: [HostEntry] { HostEntry.list(local: status, account: account) }

    /// The host the host tier shows: the one picked, else this Mac when it
    /// hosts, else the first other host.
    var host: HostEntry? { hosts.first { $0.name == chosenHost } ?? hosts.first }

    /// Logged in, with nothing served here: this Mac only views its hosts.
    /// Known once Plow has listed them.
    var viewerOnly: Bool { status?.peer != nil && account != nil && !hosts.contains(where: \.isThisMac) }

    /// What each host last said of its rules, vault and connectors, by host
    /// name; Home shows the selected host's.
    private(set) var summaries: [String: HostSummary] = [:]

    /// Reads the selected host's summary: this Mac's from its own state
    /// folder, another host's with `--host`. One read at a time (a remote
    /// read in parallel is no faster), each shown as it answers. Another
    /// host Plow counts offline isn't asked; this Mac always is.
    func refreshSummary() async {
        guard let host, host.isThisMac || host.online != false else { return }
        let target = host.isThisMac ? nil : host.name
        let rules = await Self.result { try await self.client.rulesShow(host: target) }
        summaries[host.name, default: HostSummary()].rules = rules
        let vault = await Self.result { try await self.client.vaultList(host: target) }
        summaries[host.name, default: HostSummary()].vault = vault
        let plugins = await Self.result { try await self.client.pluginsList(host: target) }
        summaries[host.name, default: HostSummary()].plugins = plugins
    }

    static func result<T>(_ read: () async throws -> T) async -> Result<T, LatchError> {
        do {
            return .success(try await read())
        } catch {
            return .failure(error as? LatchError ?? .unreachable(error.localizedDescription))
        }
    }

    /// For snapshots: what a host said.
    func stage(summary: HostSummary, for host: String) {
        summaries[host] = summary
    }

    /// The Vault screen's selected item, and what `vault show` said of
    /// items, by host name then item.
    var selectedVaultItem: VaultList.Item.ID?
    /// The New Item or Edit sheet over the Vault screen.
    var vaultSheet: VaultEditor?
    private(set) var vaultShows: [String: [String: Result<VaultShow, LatchError>]] = [:]

    /// Reads a host's vault again (the Vault screen, after a change).
    func refreshVault(on host: HostEntry) async {
        let vault = await Self.result { try await self.client.vaultList(host: host.isThisMac ? nil : host.name) }
        summaries[host.name, default: HostSummary()].vault = vault
    }

    /// Reads an item's non-secret values.
    func showVaultItem(_ item: String, on host: HostEntry) async {
        let show = await Self.result { try await self.client.vaultShow(item, host: host.isThisMac ? nil : host.name) }
        vaultShows[host.name, default: [:]][item] = show
    }

    /// Opens an item's Edit sheet, from the item as it is now.
    func beginVaultEdit(_ item: VaultList.Item, on host: HostEntry) async {
        await showVaultItem(item.item, on: host)
        switch vaultShows[host.name]?[item.item] {
        case let .success(show)?: vaultSheet = VaultEditor(editing: show, on: host)
        case let .failure(error)?: vaultSheet = VaultEditor(failed: item, error.message, on: host)
        case nil: break
        }
    }

    /// Adds an item to a host's vault and selects it.
    func addVaultItem(_ draft: VaultDraft, on host: HostEntry) async throws {
        let item = try await vaultChange(on: host) { try await self.client.vaultAdd(draft, host: $0) }
        selectedVaultItem = item.item
    }

    /// Changes an item at the revision the person saw: an item changed
    /// elsewhere meanwhile is refused, never overwritten.
    func editVaultItem(_ item: VaultList.Item, _ draft: VaultDraft, on host: HostEntry) async throws {
        _ = try await vaultChange(on: host) { try await self.client.vaultEdit(item.item, revision: item.revision, draft, host: $0) }
        await showVaultItem(item.item, on: host)
    }

    /// Why the last removal from a vault failed, until it's dismissed.
    var vaultRemoveError: String?

    /// Removes an item at the revision the person saw; a refusal is kept
    /// for the Vault screen to say.
    func removeVaultItem(_ item: VaultList.Item, on host: HostEntry) async {
        do {
            try await vaultChange(on: host) { try await self.client.vaultRemove(item.item, revision: item.revision, host: $0) }
            vaultRemoveError = nil
            if selectedVaultItem == item.item { selectedVaultItem = nil }
            vaultShows[host.name]?[item.item] = nil
        } catch {
            vaultRemoveError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    /// A change to a host's vault, then the vault read again, whether it
    /// worked or not. An item changed elsewhere is said in the app's words.
    private func vaultChange<T>(on host: HostEntry, _ change: (String?) async throws -> T) async throws -> T {
        do {
            let result = try await change(host.isThisMac ? nil : host.name)
            await refreshVault(on: host)
            return result
        } catch {
            await refreshVault(on: host)
            if (error as? LatchError)?.code == "stale_revision" { throw LatchError.unreachable(Self.itemChangedElsewhere) }
            throw error
        }
    }

    static let itemChangedElsewhere = "This item changed since you opened it. Nothing was saved; look at it again, then make your change."

    /// For snapshots: what `vault show` said.
    func stage(vaultShow: VaultShow, on host: String) {
        vaultShows[host, default: [:]][vaultShow.item.item] = .success(vaultShow)
    }

    /// The account's agents (`latch agents`), and each one's standing on
    /// each host (`latch agents --host`), by host name then agent uid; a
    /// host latch couldn't ask has its error instead.
    private(set) var agents: [AgentsList.Agent]?
    private(set) var agentsError: LatchError?
    private(set) var standings: [String: Result<[String: AgentsList.Agent.Standing], LatchError>] = [:]

    func refreshAgents() async {
        do {
            agents = try await client.agents(host: nil).agents
            agentsError = nil
        } catch {
            agentsError = error as? LatchError ?? .unreachable(error.localizedDescription)
        }
        // One host at a time: each is a read over latch's connection.
        for host in hosts where host.isThisMac || host.online != false {
            await refreshStanding(on: host)
        }
    }

    func refreshStanding(on host: HostEntry) async {
        let result = await Self.result { try await self.client.agents(host: host.name) }
        standings[host.name] = result.map { Dictionary($0.agents.compactMap { agent in agent.standing.map { (agent.uid, $0) } },
                                                        uniquingKeysWith: { first, _ in first }) }
    }

    /// The Agents page's path: an agent's uid opens its page.
    var agentsPath: [String] = []
    /// The agent whose rules the Rules screen brings into view.
    var rulesFocus: String?

    /// The agent's rules to scroll to on a host's Rules screen: the one
    /// asked for, once its editor exists (the host's rules read and the
    /// agent listed); nil until then.
    func rulesScrollTarget(on host: HostEntry) -> String? {
        guard let uid = rulesFocus, case .success? = summaries[host.name]?.rules,
              agents?.contains(where: { $0.uid == uid }) == true else { return nil }
        return uid
    }
    /// Rules' Try a request.
    let trial = Trial()

    /// Reads a host's rules again (the Rules screen, after a save).
    func refreshRules(on host: HostEntry) async {
        let rules = await Self.result { try await self.client.rulesShow(host: host.isThisMac ? nil : host.name) }
        summaries[host.name, default: HostSummary()].rules = rules
    }

    /// Saves rules on a host, for every agent or one agent's own, only if
    /// they're still what the person started editing (`revision`, nil when
    /// there were none): latch refuses a save over rules changed elsewhere
    /// meanwhile. Empty text clears them.
    func saveRules(_ text: String, on host: HostEntry, agent: String?, revision: String?) async throws {
        do {
            try await client.rulesSet(text, host: host.isThisMac ? nil : host.name, agent: agent, revision: revision)
        } catch let error as LatchError where error.code == "stale_revision" {
            throw LatchError.unreachable(Self.changedElsewhere)
        }
        await refreshRules(on: host)
        await refreshStanding(on: host)
    }

    static let changedElsewhere = "These rules changed since you started editing. Revert to see the saved rules, then make your change again."

    /// `latch rules try` on a host.
    func tryRequest(_ request: [String], on host: HostEntry, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        try await client.rulesTry(request, host: host.isThisMac ? nil : host.name, agent: agent, newAgent: newAgent, draft: draft)
    }

    /// For snapshots: as `latch agents` said.
    func stage(agents: AgentsList, standings: [String: Result<AgentsList, LatchError>]) {
        self.agents = agents.agents
        self.standings = standings.mapValues { result in
            result.map { Dictionary($0.agents.compactMap { agent in agent.standing.map { (agent.uid, $0) } }, uniquingKeysWith: { a, _ in a }) }
        }
    }

    /// Each host's activity, by host name, made when a screen first shows it.
    private var feeds: [String: ActivityFeed] = [:]

    func feed(for host: HostEntry) -> ActivityFeed {
        if let feed = feeds[host.name] { return feed }
        let feed = ActivityFeed(client: client, host: host.isThisMac ? nil : host.name)
        feeds[host.name] = feed
        return feed
    }

    /// The key on records of a host's requests of itself: this Mac's own,
    /// else Plow's for that host.
    func key(of host: HostEntry) -> String? {
        host.isThisMac ? status?.peer?.key ?? host.key : host.key
    }

    /// The record Activity's inspector shows; Home's recent rows set it.
    var selectedRecord: AuditRecord.ID?
    /// Activity's filters, kept as hosts change; `showConnections` also
    /// shows the owner's reads.
    var activityFilter = ActivityView.Filter.all
    var showConnections = false

    /// Follows `status --local --follow` until the main window closes, so
    /// this Mac's state is always latch's latest without asking again.
    func followStatus() async {
        do {
            for try await status in client.statusFollow() {
                statusChanged(status)
            }
        } catch is CancellationError {
        } catch {
            statusError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    /// A new `status --local` from the stream.
    func statusChanged(_ status: LocalStatus) {
        self.status = status
        statusError = nil
        leaveMainIfLoggedOut()
    }

    /// "Start hosting on this Mac…" asked, awaiting the person's yes; and a
    /// start from the main window, while it runs and how it failed.
    var confirmingStartHosting = false
    private(set) var startingHost = false
    private(set) var startHostError: String?

    /// Starts this Mac's host from the main window, then reads back what
    /// latch and Plow say.
    func startHosting() async {
        startingHost = true
        startHostError = nil
        defer { startingHost = false }
        do {
            try await startHost()
        } catch {
            startHostError = (error as? LatchError)?.message ?? error.localizedDescription
        }
        await refreshStatus()
        await refreshAccount()
    }

    /// Starts this Mac's host the way `setup --status` says (with the last
    /// serve's flags when there were any). Nothing to do when it isn't
    /// needed. A host latch says can't start now throws latch's reason.
    func startHost() async throws {
        guard let host = try await client.setupStatus().step("host") else { return }
        switch host.state {
        case "needed": if let args = host.args { try await client.perform(args) }
        case "blocked": throw HostBlocked(reason: host.reason)
        default: break
        }
    }

    func refreshStatus() async {
        do {
            status = try await client.localStatus()
            statusError = nil
        } catch {
            statusError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    /// The onboarding window opened with no flow (at launch): asks latch
    /// whether this Mac is logged in and resumes where the person left off.
    func resumeOnboarding() async {
        guard onboarding == nil else { return }
        await refreshStatus()
        let progress = store.load()
        switch progress.destination(loggedIn: status?.peer != nil) {
        case .main: finishOnboarding()
        case let .onboarding(step): onboarding = Onboarding(app: self, step: step, progress: progress)
        }
    }

    func beginOnboarding(at step: Step, progress: Progress) {
        onboarding = Onboarding(app: self, step: step, progress: progress)
        openWindow?(Self.onboardingWindow)
        dismissWindow?(Self.mainWindow)
    }

    /// "Set up again…" from the main window: every screen, from Welcome.
    func setUpAgain() {
        let progress = store.load().settingUpAgain()
        store.save(progress)
        guard case let .onboarding(step) = progress.destination(loggedIn: status?.peer != nil) else { return }
        beginOnboarding(at: step, progress: progress)
    }

    /// The main window learned this Mac is logged out (a `latch logout`):
    /// onboarding takes over, at Log in.
    func leaveMainIfLoggedOut() {
        guard let status, status.peer == nil, onboarding == nil else { return }
        let progress = store.load()
        guard case let .onboarding(step) = progress.destination(loggedIn: false) else { return }
        beginOnboarding(at: step, progress: progress)
    }

    func finishOnboarding() {
        openWindow?(Self.mainWindow)
        dismissWindow?(Self.onboardingWindow)
        onboarding = nil
    }

    /// For snapshots: a flow staged at `step`.
    func stage(_ step: Step, progress: Progress = Progress()) -> Onboarding {
        let flow = Onboarding(app: self, step: step, progress: progress)
        onboarding = flow
        return flow
    }

    /// For snapshots: as `status --local` said.
    func stage(status: LocalStatus?, error: String? = nil) {
        self.status = status
        statusError = error
    }

    /// For snapshots: as `latch status` said.
    func stage(account: AccountStatus?, error: LatchError? = nil) {
        self.account = account
        accountError = error
    }
}

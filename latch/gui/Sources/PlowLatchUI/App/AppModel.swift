import Observation
import os
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
            if status?.thisMachine.hosting.state == "serving", let name = status?.thisMachine.name { feeds[name]?.retry() }
        }
    }
    private(set) var statusError: LatchError?
    /// The onboarding flow, while the onboarding window is open.
    private(set) var onboarding: Onboarding?
    /// How the app hands a link to the system (Messages, System Settings);
    /// only ever on a person's click. A test build records them instead, so
    /// a run never opens anything.
    var open: (URL) -> Void = { NSWorkspace.shared.open($0) }
    /// Puts text on the clipboard (a number, a handle, a message to send);
    /// only on a click. A test build records it.
    var copy: (String) -> Void = { text in
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
    /// Floats the Full Disk Access helper under System Settings, with this
    /// app to drag into the list.
    var guideGrant: (URL) -> any GrantGuide = { GrantHelper.show(dragging: $0) }
    /// Turns "Start when you log in" on or off; a test build records it.
    var setLoginItem: (Bool) throws -> Void = { try LoginItem.set($0) }
    var loginItemIsOn: () -> Bool = { LoginItem.isOn }
    /// System Settings at Login Items, only ever on a click; a test build
    /// records it.
    var openLoginItems: () -> Void = { LoginItem.openSettings() }
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

    /// Where the app's own choices are kept; none for a still.
    private let preferences: UserDefaults?

    init(client: any LatchClient, store: ProgressStore? = nil, still: Bool = false, loginItemOffered: Bool = true,
         preferences: UserDefaults? = .standard) {
        self.client = client
        self.still = still
        self.loginItemOffered = loginItemOffered
        self.store = store ?? ProgressStore(defaults: .standard, stateDir: client.stateDir)
        self.preferences = still ? nil : preferences
        checksForUpdates = self.preferences?.object(forKey: Self.checksForUpdatesKey) as? Bool ?? true
    }

    var progress: Progress { onboarding?.progress ?? store.load() }

    /// What's set up, as latch says: whether rules apply on this Mac, and
    /// each setup step (`setup --status`); nil where latch couldn't say.
    private(set) var rulesSaved: Bool?
    private(set) var setup: SetupStatus?

    func refreshSetup() async {
        setup = try? await client.setupStatus()
        rulesSaved = setup?.step("rules").map { $0.state == "done" }
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
    /// When `latch status` last answered.
    private(set) var accountReadAt: Date?

    func refreshAccount() async {
        do {
            account = try await client.accountStatus()
            accountError = nil
            accountReadAt = .now
        } catch {
            accountError = .from(error)
        }
    }

    /// Settings opening: the account and its agents, each unless read in
    /// the last minute.
    func refreshAccountIfStale(now: Date = .now) async {
        let fresh = { (read: Date?) in read.map { now.timeIntervalSince($0) < 60 } == true }
        if !fresh(accountReadAt) { await refreshAccount() }
        if !fresh(agentsReadAt) { await refreshAgentList() }
    }

    /// What Home and the sidebar say while `latch status` fails.
    var plowNotice: PlowNotice? { accountError.map { PlowNotice(error: $0, listed: account != nil) } }

    /// The main window's place: an account page, or a screen of the host
    /// workspace, which keeps its screen while an account page shows and
    /// as hosts change.
    var place: Place {
        get { page.map(Place.account) ?? .host(screen) }
        set {
            switch newValue {
            case let .account(page): self.page = page
            case let .host(screen): (page, self.screen) = (nil, screen)
            }
        }
    }
    private var page: AccountPage?
    private(set) var screen: HostScreen = .home
    /// The host the workspace shows, as the sidebar picked it; nil until
    /// one is picked.
    var chosenHost: String?

    /// What the main window shows over itself, until it's answered or
    /// closed; nil for nothing.
    var sheet: MainSheet?

    /// Closes `sheet`, if it's still the one showing: a flow that ends
    /// after the person left it never closes what came next.
    func close(_ sheet: MainSheet) {
        if self.sheet == sheet { self.sheet = nil }
    }

    /// Asks the person to confirm an action, over the main window.
    func ask(_ confirmation: Confirmation) {
        sheet = .confirm(confirmation)
    }

    /// The person's yes: the action, on what the question named.
    func confirmed(_ confirmation: Confirmation) async {
        switch confirmation {
        case .startHosting: await startHosting()
        case .stopHost: await stopHost()
        case let .removeAgent(agent): await removeAgent(agent)
        case let .removeVaultItem(item, host): await removeVaultItem(item, on: host)
        case let .removeDevice(device): await removeDevice(device.name)
        case let .deleteRule(rule): await deleteRule(rule)
        }
    }

    /// A sidebar host row's click: that host's workspace, on the screen it
    /// was last on.
    /// Settings' way into the account: the main window, at `place`.
    /// Opened first: a window it makes restores the place it last saved.
    func showInMainWindow(_ place: Place) {
        openWindow?(Self.mainWindow)
        self.place = place
    }

    func showHost(_ name: String) {
        chosenHost = name
        place = .host(screen)
    }

    /// The window's place and host as the window last saved them (its
    /// scene state): a place no longer known is Home, and a host no longer
    /// listed falls back as `host` does.
    func restore(place saved: String, host: String?) {
        place = Place(rawValue: saved) ?? .host(.home)
        chosenHost = host
    }

    /// What the main window shows: an account page carries no host.
    var route: Route {
        switch place {
        case let .account(page): .account(page)
        case let .host(screen): .host(screen, host)
        }
    }

    /// What a screen's reads depend on, which the window keys them by: a
    /// host's screen, its host and whether Plow counts it online; Agents,
    /// every host it can ask; Connections, no host.
    struct Entry: Hashable {
        var place: Place
        var hosts: [String] = []
        var online: Bool?
    }

    var entry: Entry {
        switch route {
        case .account(.agents): Entry(place: place, hosts: hosts.filter { $0.isThisMac || $0.online != false }.map(\.name))
        case .account: Entry(place: place)
        case let .host(_, host): Entry(place: place, hosts: host.map { [$0.name] } ?? [], online: host?.online)
        }
    }

    /// The sidebar's hosts, this Mac first.
    var hosts: [HostEntry] { HostEntry.list(local: status, account: account) }

    /// The host the workspace shows: the one picked while it's listed,
    /// else this Mac when it hosts, else the first other host.
    var host: HostEntry? { hosts.first { $0.name == chosenHost } ?? hosts.first }

    /// Logged in, with nothing served here: this Mac only views its hosts.
    /// Known once Plow has listed them.
    var viewerOnly: Bool { status?.login != nil && account != nil && !hosts.contains(where: \.isThisMac) }

    /// What each host last said of itself and its vault, by host name;
    /// Home shows the selected host's.
    private(set) var summaries: [String: HostSummary] = [:]

    /// Reads a host's vault: this Mac's from its own state folder, another
    /// host's with `--host`; only when it isn't held yet unless `refresh`.
    /// Another host Plow counts offline isn't asked; this Mac always is.
    private func readVault(of host: HostEntry, refresh: Bool) async {
        guard host.isThisMac || host.online != false, refresh || !Self.held(summaries[host.name]?.vault) else { return }
        await refreshVault(on: host)
    }

    /// A host's plugins, each with whether it's on: this Mac's from its own
    /// status, another host's from its `status --host`; nil until known.
    func plugins(of host: HostEntry) -> Result<[Plugin], LatchError>? {
        if host.isThisMac { return status.map { .success($0.plugins ?? []) } }
        return summaries[host.name]?.status?.map { $0.plugins ?? [] }
    }

    /// Another host's own status (its plugins, and Host Settings' version),
    /// asked only of a host Plow doesn't count offline.
    private func readHostStatus(of host: HostEntry, refresh: Bool) async {
        guard !host.isThisMac, host.online != false, refresh || !Self.held(summaries[host.name]?.status) else { return }
        guard let status = await Self.result({ try await self.client.hostStatus(host.name) }) else { return }
        summaries[host.name, default: HostSummary()].status = status
    }

    /// What a screen reads when it shows, decided here, not by the screen:
    /// only what it uses, and only what isn't held yet, so coming back to
    /// a screen reads nothing again (`refresh` reads it all again). Run in
    /// a task that's cancelled when the route changes: a cancelled read
    /// keeps what was held, and the reads after it don't start.
    func enter(_ route: Route, refresh: Bool = false) async {
        let interval = Self.signposter.beginInterval("enter", id: Self.signposter.makeSignpostID(), "\(route.title, privacy: .public)")
        defer { Self.signposter.endInterval("enter", interval) }
        switch route {
        case let .host(.home, host):
            guard let host else { return }
            async let vault: Void = readVault(of: host, refresh: refresh)
            async let status: Void = readHostStatus(of: host, refresh: refresh)
            _ = await (vault, status)
        case let .host(.settings, host):
            guard let host else { return }
            await readHostStatus(of: host, refresh: refresh)
        case .account(.rules):
            // Read on every visit: latch brings each host it reaches up to
            // date as it reads. Its rows name the agents.
            let names = refresh || agents == nil
            async let list: Void = refreshRules()
            async let agentList = names ? refreshAgentList() : true
            _ = await (list, agentList)
        case let .host(.vault, host):
            guard let host, host.isThisMac || host.online != false else { return }
            if refresh || !Self.held(summaries[host.name]?.vault) { await refreshVault(on: host) }
        case .account(.agents):
            // The catalog only names each cloud agent's kind; without it
            // the page says less, so a failed read isn't shown.
            let readKinds = refresh || agentKinds == nil
            async let kinds: Void = readKinds ? refreshAgentKinds() : ()
            if refresh { await refreshAgents() } else {
                if agents == nil, await !refreshAgentList() { return }
                // Each host's standing it doesn't have yet, as when a visit
                // was left before reading them all.
                await readStandings(missing: true)
            }
            await kinds
        case .account(.connections):
            if refresh || connections == nil { await refreshConnections() }
            if refresh || !Self.held(devices) { await refreshDevices() }
        case .host(.activity, _):
            // Its feed is the window's (`activityFeed`).
            break
        }
    }

    /// The app came to the front: what can change while it's elsewhere
    /// (Plow's hosts, rules or a vault changed from another device, a
    /// Google account connected in the browser) is read again, once, here:
    /// Plow's hosts and the screen showing.
    func activated() async {
        await refreshAccount()
        await enter(route, refresh: true)
    }

    /// Marks screen entries (and a test build's timed hooks) for
    /// Instruments' Points of Interest.
    static let signposter = OSSignposter(subsystem: "co.plow.latch", category: .pointsOfInterest)

    /// A read's answer, or nil when the task asking was cancelled: a
    /// cancelled read is no failure, and it leaves what was held as it was.
    static func result<T>(_ read: () async throws -> T) async -> Result<T, LatchError>? {
        do {
            let answer = try await read()
            return Task.isCancelled ? nil : .success(answer)
        } catch {
            return error is CancellationError || Task.isCancelled ? nil : .failure(.from(error))
        }
    }

    /// An answer is held: read, and not a failure.
    static func held<T>(_ answer: Result<T, LatchError>?) -> Bool {
        if case .success? = answer { true } else { false }
    }

    /// For snapshots: what a host said.
    func stage(summary: HostSummary, for host: String) {
        summaries[host] = summary
    }

    /// The Vault screen's selected item, on the host it was selected on,
    /// its search, and what `vault show` said of items, by host name then
    /// item.
    var selectedVaultItem: OnHost?
    var vaultSearch = ""
    private(set) var vaultShows: [String: [String: Result<VaultShow, LatchError>]] = [:]

    /// Reads a host's vault again (the Vault screen, after a change).
    func refreshVault(on host: HostEntry) async {
        guard let vault = await Self.result({ try await self.client.vaultList(host: host.isThisMac ? nil : host.name) }) else { return }
        summaries[host.name, default: HostSummary()].vault = vault
    }

    /// Reads an item's non-secret values; if it changed since the vault was
    /// listed, the vault is read again, so the item listed is the one shown.
    func showVaultItem(_ item: String, on host: HostEntry) async {
        guard let show = await Self.result({ try await self.client.vaultShow(item, host: host.isThisMac ? nil : host.name) }) else { return }
        vaultShows[host.name, default: [:]][item] = show
        guard case let .success(shown) = show, case let .success(vault)? = summaries[host.name]?.vault,
              let listed = vault.items.first(where: { $0.item == item }), listed.revision < shown.item.revision else { return }
        await refreshVault(on: host)
    }

    /// What `vault show` said of an item at the revision listed; nil while
    /// that revision is read (a show of an older one is not its values).
    func vaultShow(of item: VaultList.Item, on host: HostEntry) -> Result<VaultShow, LatchError>? {
        let show = vaultShows[host.name]?[item.item]
        if case let .success(shown)? = show, shown.item.revision != item.revision { return nil }
        return show
    }

    /// Opens an item's Edit sheet, from the item as it is now.
    func beginVaultEdit(_ item: VaultList.Item, on host: HostEntry) async {
        await showVaultItem(item.item, on: host)
        switch vaultShows[host.name]?[item.item] {
        case let .success(show)?: sheet = .vaultItem(VaultEditor(editing: show, on: host))
        case let .failure(error)?: sheet = .vaultItem(VaultEditor(failed: item, error, on: host))
        case nil: break
        }
    }

    /// Adds an item to a host's vault and selects it.
    func addVaultItem(_ draft: VaultDraft, on host: HostEntry) async throws {
        let item = try await vaultChange(on: host) { try await self.client.vaultAdd(draft, host: $0) }
        selectedVaultItem = OnHost(host: host.name, id: item.item)
    }

    /// Changes an item at the revision the person saw: an item changed
    /// elsewhere meanwhile is refused, never overwritten.
    func editVaultItem(_ item: VaultList.Item, _ draft: VaultDraft, on host: HostEntry) async throws {
        _ = try await vaultChange(on: host) { try await self.client.vaultEdit(item.item, revision: item.revision, draft, host: $0) }
        await showVaultItem(item.item, on: host)
    }

    /// The items a host's Vault lists: those its search matches.
    func vaultRows(_ vault: VaultList) -> [VaultList.Item] {
        let query = vaultSearch.trimmingCharacters(in: .whitespaces).lowercased()
        guard !query.isEmpty else { return vault.items }
        return vault.items.filter { item in
            ([item.title, item.kind, item.username ?? ""] + item.sites).contains { $0.lowercased().contains(query) }
        }
    }

    /// The item the inspector shows on a host: the one selected there,
    /// while the Vault lists it.
    func inspectedVaultItem(on host: HostEntry) -> VaultList.Item? {
        guard let selected = selectedVaultItem, selected.host == host.name,
              case let .success(vault)? = summaries[host.name]?.vault,
              let item = vault.items.first(where: { $0.item == selected.id }) else { return nil }
        return vaultRows(VaultList(items: [item])).isEmpty ? nil : item
    }

    /// Why the last removal from a vault failed, until it's dismissed.
    var vaultRemoveError: LatchError?

    /// Removes an item at the revision the person saw; a refusal is kept
    /// for the Vault screen to say.
    func removeVaultItem(_ item: VaultList.Item, on host: HostEntry) async {
        do {
            try await vaultChange(on: host) { try await self.client.vaultRemove(item.item, revision: item.revision, host: $0) }
            vaultRemoveError = nil
            if selectedVaultItem == OnHost(host: host.name, id: item.item) { selectedVaultItem = nil }
            vaultShows[host.name]?[item.item] = nil
        } catch {
            vaultRemoveError = .from(error)
        }
    }

    /// Import…'s export, chosen: what it holds, in the import sheet.
    func beginImport(_ file: URL, on host: HostEntry) {
        let importer = VaultImport(file: file, on: host)
        sheet = .vaultImport(importer)
        Task { await importer.load(app: self) }
    }

    /// `latch vault import FILE --preview` on a host.
    func previewImport(_ file: URL, on host: HostEntry) async throws -> ImportPreview {
        try await client.vaultImportPreview(file, host: host.isThisMac ? nil : host.name)
    }

    /// Imports the rows picked from a preview, only if the vault and the
    /// export still mean what it showed (`stale_revision` otherwise).
    func importRows(_ picks: [Int], expect: String, from file: URL, on host: HostEntry) async throws -> ImportResult {
        try await vaultChange(on: host, stale: nil) { try await self.client.vaultImport(file, picks: picks, expect: expect, host: $0) }
    }

    /// A change to a host's vault, then the vault read again, whether it
    /// worked or not. An item changed elsewhere is said in the app's words
    /// (`stale`), or left as latch's error when nil.
    private func vaultChange<T>(on host: HostEntry, stale: String? = itemChangedElsewhere,
                                _ change: (String?) async throws -> T) async throws -> T {
        do {
            let result = try await change(host.isThisMac ? nil : host.name)
            await refreshVault(on: host)
            return result
        } catch {
            await refreshVault(on: host)
            if let stale, (error as? LatchError)?.code == "stale_revision" { throw LatchError.unreachable(stale) }
            throw error
        }
    }

    static let itemChangedElsewhere = "This item changed since you opened it. Nothing was saved; look at it again, then make your change."

    /// For snapshots: what `vault show` said.
    func stage(vaultShow: VaultShow, on host: String) {
        vaultShows[host, default: [:]][vaultShow.item.item] = .success(vaultShow)
    }

    /// Connections: the account's Google accounts in Plow, as `setup
    /// --status` says; nil until it has said.
    private(set) var connections: Connections?

    func refreshConnections() async {
        connections = Connections(try? await client.setupStatus())
    }

    /// Connections' sign-in with Google while its sheet shows, and the
    /// newest link latch gave, kept only to open again.
    private(set) var googleSignIn: GoogleSignIn?
    private var googleSignInTask: Task<Void, Never>?
    private var googleSignInLink: URL?

    /// "Connect Google Account…" or "Add Another Account…" (`adding`), and
    /// the sheet's Try Again: latch gets a sign-in link from Plow and waits
    /// for the new account, then turns Google on here; the sheet shows while
    /// it waits and goes once Google is on. Only ever on a click.
    func connectGoogle(adding: Bool) {
        let sheet = GoogleSignIn(adding: adding)
        endGoogleSignIn()
        googleSignIn = sheet
        googleSignInTask = Task {
            guard let signedIn = await Self.result({
                try await self.signInWithGoogle(addAccount: sheet.adding) { self.googleSignInLink = $0 }
            }) else { return }
            // Over, with an account or not: no link to open again.
            googleSignInLink = nil
            switch signedIn {
            case .success(_?):
                googleSignInTask = nil
                googleSignIn = nil
                await refreshConnections()
                await refreshStatus()
            case .success(nil):
                googleSignIn?.failure = GoogleSignIn.stopped
            case let .failure(error):
                googleSignIn?.failure = GoogleSignIn.problem(error, adding: sheet.adding)
            }
        }
    }

    /// The sheet's "Open the Sign-In Page Again": the newest link.
    func openGoogleSignInAgain() {
        if let googleSignInLink { open(googleSignInLink) }
    }

    var canOpenGoogleSignInAgain: Bool { googleSignInLink != nil }

    /// Cancel, or the sheet going: latch stops waiting.
    func endGoogleSignIn() {
        googleSignInTask?.cancel()
        googleSignInTask = nil
        googleSignInLink = nil
        googleSignIn = nil
    }

    /// Signs in with Google through latch: the first link it gives opens in
    /// the browser, and each is handed to `newLink` for opening again.
    /// latch's enable once Google is on; nil if latch stopped before. Once
    /// the wait is stopped, a line latch printed before it ends goes nowhere.
    func signInWithGoogle(addAccount: Bool, newLink: (URL) -> Void) async throws -> GoogleEnabled? {
        var opened = false
        for try await event in client.signInGoogle(addAccount: addAccount) {
            try Task.checkCancellation()
            switch event {
            case let .link(url):
                newLink(url)
                if !opened {
                    opened = true
                    open(url)
                }
            case let .enabled(result):
                return result
            }
        }
        return nil
    }

    /// For snapshots: Connections' sign-in sheet.
    func stage(googleSignIn: GoogleSignIn?, link: URL? = nil) {
        self.googleSignIn = googleSignIn
        googleSignInLink = link
    }

    /// A plugin's "Manage accounts…": the account's connections.
    func manageAccounts() {
        place = .account(.connections)
    }

    /// For snapshots: what `setup --status` said of Google.
    func stage(connections: Connections?) {
        self.connections = connections
    }

    /// Connections: the account's logged-in machines (`latch devices`).
    /// Host Settings: a start, stop or restart of this Mac's host while it
    /// runs and how it failed.
    private(set) var devices: Result<[DeviceList.Device], LatchError>?
    private(set) var hostAction: HostAction?
    private(set) var hostActionError: LatchError?

    enum HostAction { case stopping, restarting }

    func refreshDevices() async {
        if let devices = await Self.result({ try await self.client.devices().devices }) { self.devices = devices }
    }

    /// Logged-in machines that don't host: what the account's devices list
    /// holds beyond the hosts Plow lists.
    var loggedInDevices: [DeviceList.Device]? {
        guard case let .success(devices)? = devices else { return nil }
        let hostKeys = Set(hosts.compactMap(\.key))
        return devices.filter { !hostKeys.contains($0.key) }
    }

    /// Why the last removal of a machine failed.
    private(set) var deviceRemoveError: LatchError?

    /// Removes a machine from the account; it's logged out there. A
    /// refusal is kept for Connections to say.
    func removeDevice(_ name: String) async {
        guard let removed = await Self.result({ try await self.client.removeDevice(name) }) else { return }
        await refreshDevices()
        if case let .failure(error) = removed { deviceRemoveError = error } else { deviceRemoveError = nil }
    }

    /// The last update check (`latch update --check`) and when it began;
    /// an install of the newest build while it runs, how it failed, and
    /// whether it installed one this window still runs the old version of.
    private(set) var update: Result<UpdateCheck, LatchError>?
    private(set) var checkingUpdate = false
    private(set) var updateCheckedAt: Date?
    private(set) var installingUpdate = false
    private(set) var installUpdateError: LatchError?
    private(set) var updateInstalled = false

    /// "Check for updates automatically", in the app's preferences: on
    /// unless turned off. Nothing installs without a click.
    var checksForUpdates: Bool {
        didSet { preferences?.set(checksForUpdates, forKey: Self.checksForUpdatesKey) }
    }

    private static let checksForUpdatesKey = "CheckForUpdates"
    /// How old the last check may be before an automatic one runs.
    static let updateCheckEvery: TimeInterval = 6 * 60 * 60

    /// Whether there's a newer build to install, and its number when
    /// known: the app's last check, else what this Mac's host last found.
    var updateAvailable: Bool {
        if case let .success(check)? = update { return check.available }
        return status?.update?.available == true
    }

    var availableBuild: Int? {
        if case let .success(check)? = update { return check.latest?.build }
        return status?.update?.build
    }

    func checkForUpdate() async {
        guard !checkingUpdate else { return }
        checkingUpdate = true
        updateCheckedAt = .now
        defer { checkingUpdate = false }
        if let update = await Self.result({ try await self.client.updateCheck() }) { self.update = update }
    }

    /// Checks, when automatic checks are on, if the last check is older
    /// than `updateCheckEvery` or there was none.
    func checkForUpdateIfDue(now: Date = .now) async {
        guard checksForUpdates, !installingUpdate else { return }
        if let checked = updateCheckedAt, now.timeIntervalSince(checked) < Self.updateCheckEvery { return }
        await checkForUpdate()
    }

    /// Installs the newest build with `latch update`, which checks its
    /// signatures first. The installer quits this window and opens it again
    /// on the new version; a window it didn't quit (one run from elsewhere)
    /// says the update is installed.
    func installUpdate() async {
        guard !installingUpdate else { return }
        installingUpdate = true
        installUpdateError = nil
        defer { installingUpdate = false }
        switch await Self.result({ try await self.client.update() }) {
        case let .success(result)?: updateInstalled = result.updated
        case let .failure(error)?: installUpdateError = error
        case nil: return
        }
        await checkForUpdate()
        await refreshStatus()
    }

    /// Stops this Mac's host; agents can't reach it until it's started.
    /// "Keep this Mac awake while plugged in": latch saves it (no host or
    /// restart needed), then the switch shows what latch reads back.
    private(set) var settingKeepAwake = false
    private(set) var keepAwakeError: LatchError?

    func setKeepAwake(_ on: Bool) async {
        // One save at a time; a click meanwhile is dropped.
        guard !settingKeepAwake else { return }
        settingKeepAwake = true
        defer { settingKeepAwake = false }
        do {
            try await client.perform(["settings", "set", "--keep-awake", on ? "on" : "off"])
            keepAwakeError = nil
        } catch {
            keepAwakeError = .from(error)
        }
        await refreshStatus()
    }

    func stopHost() async {
        await hostAction(.stopping) { try await self.client.stop() }
    }

    /// Stops this Mac's host and starts it again as it last ran.
    func restartHost() async {
        await hostAction(.restarting) {
            try await self.client.stop()
            try await self.startHost()
        }
    }

    private func hostAction(_ action: HostAction, _ run: () async throws -> Void) async {
        hostAction = action
        hostActionError = nil
        defer { hostAction = nil }
        do {
            try await run()
        } catch {
            hostActionError = .from(error)
        }
        await refreshStatus()
    }

    /// Host Settings: a plugin of this Mac's being turned on or off while
    /// latch does it, and why the last change failed, in the app's words.
    /// Only this Mac's plugins change here, whichever host shows; another
    /// host's are turned on there.
    private(set) var connectorChange: ConnectorChange?
    private(set) var connectorError: LatchError?

    struct ConnectorChange: Equatable {
        var plugin: String
        var on: Bool
    }

    /// Turns a plugin on this Mac's host on or off, one at a time, then
    /// reads this Mac's status, its plugins among it, again.
    func setConnector(_ name: String, on: Bool) async {
        guard connectorChange == nil, hosts.contains(where: \.isThisMac) else { return }
        connectorChange = ConnectorChange(plugin: name, on: on)
        connectorError = nil
        defer { connectorChange = nil }
        guard let changed = await Self.result({
            if on { _ = try await self.client.enable(name) } else { try await self.client.disable(name) }
        }) else { return }
        await refreshStatus()
        if case let .failure(error) = changed {
            connectorError = .unreachable(PluginCopy.refusal(error, plugin: name, on: on), detail: error.details)
        }
    }

    /// While `latch logout` runs, and after it if Plow couldn't take this
    /// Mac out of the account: what the person must do about it, said
    /// before onboarding takes over.
    private(set) var loggingOut = false
    private(set) var logOutWarning: String?

    /// `latch logout`: this Mac leaves the account, then onboarding takes
    /// over, once the person has read why Plow still lists it, if it does.
    func logOut() async throws {
        loggingOut = true
        // Once it's done, onboarding takes over: the status stream may have
        // said "logged out" already, while this still held it back.
        defer {
            loggingOut = false
            leaveMainIfLoggedOut()
        }
        let result = try await client.logout()
        if let device = result.device, !device.removed {
            logOutWarning = "This Mac is logged out, but Plow couldn't take \(device.name ?? "it") out of your account. "
                + "It may still be listed until Plow can be reached."
        }
        await refreshStatus()
    }

    /// The person read the warning: onboarding takes over.
    func acknowledgeLogOutWarning() {
        logOutWarning = nil
        leaveMainIfLoggedOut()
    }

    /// The agent Agents shows, by uid, kept for a return to it.
    var selectedAgent: String?

    /// The one picked, while it's listed; else the first.
    var shownAgent: AgentsList.Agent? { agents?.first { $0.uid == selectedAgent } ?? agents?.first }

    /// Each cloud agent kind's name in Plow's catalog, by its id.
    private(set) var agentKinds: [String: String]?

    func refreshAgentKinds() async {
        guard case let .success(catalog)? = await Self.result({ try await self.client.agentCatalog() }) else { return }
        agentKinds = Dictionary(catalog.kinds.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
    }

    func agentStartByText() async throws -> AgentStartByText { try await client.agentStartByText() }

    /// Each persona's photo, by its card's URL: read once a session, when an
    /// agent with that card is first shown. One that couldn't be read isn't
    /// asked again; its agent keeps its monogram.
    private(set) var agentPhotos: [String: CGImage] = [:]
    @ObservationIgnored private var photosAsked: Set<String> = []

    func loadPhoto(of agent: AgentsList.Agent) async {
        guard let url = agent.line?.cardUrl, photosAsked.insert(url).inserted else { return }
        switch await Self.result({ try await self.client.agentCard(agent.uid) }) {
        case nil: photosAsked.remove(url)
        case let .success(card)?:
            let photo = await Task.detached { AgentPhoto.image(fromCard: card) }.value
            if let photo { agentPhotos[url] = photo }
        case .failure?: break
        }
    }

    /// For snapshots: a persona's photo, as its card gave it.
    func stage(photo: CGImage, card url: String) { agentPhotos[url] = photo }

    /// For snapshots: as latch said.
    func stage(devices: DeviceList?, update: UpdateCheck?) {
        self.devices = devices.map { .success($0.devices) }
        self.update = update.map { .success($0) }
    }

    /// For snapshots: an install of the update running, or how it ended.
    func stage(installingUpdate: Bool, error: LatchError? = nil, installed: Bool = false) {
        self.installingUpdate = installingUpdate
        installUpdateError = error
        updateInstalled = installed
    }

    /// The account's agents (`latch agents`), and each one's standing on
    /// each host (`latch agents --host`), by host name then agent uid; a
    /// host latch couldn't ask has its error instead.
    private(set) var agents: [AgentsList.Agent]?
    private(set) var agentsError: LatchError?
    private(set) var standings: [String: Result<[String: AgentsList.Agent.Standing], LatchError>] = [:]

    /// When `latch agents` last answered.
    private var agentsReadAt: Date?

    /// Reads the account's agents, without their standing on each host;
    /// false when cancelled.
    @discardableResult
    func refreshAgentList() async -> Bool {
        switch await Self.result({ try await self.client.agents(host: nil).agents }) {
        case nil: return false
        case let .success(listed)?:
            agents = listed
            agentsError = nil
            agentsReadAt = .now
        case let .failure(error)?:
            agentsError = error
        }
        return true
    }

    func refreshAgents() async {
        guard await refreshAgentList() else { return }
        await readStandings(missing: false)
    }

    /// Each reachable host's standing, or only those not held yet, all at
    /// once: each is a read over latch's connection to that host.
    private func readStandings(missing: Bool) async {
        await withTaskGroup { group in
            for host in hosts where host.isThisMac || host.online != false {
                if missing, Self.held(standings[host.name]) { continue }
                group.addTask { await self.refreshStanding(on: host) }
            }
        }
    }

    func refreshStanding(on host: HostEntry) async {
        guard let result = await Self.result({ try await self.client.agents(host: host.name) }) else { return }
        standings[host.name] = result.map { Dictionary($0.agents.compactMap { agent in agent.standing.map { (agent.uid, $0) } },
                                                        uniquingKeysWith: { first, _ in first }) }
    }

    /// Why the last removal of an agent failed, until it's dismissed.
    var agentRemoveError: LatchError?

    /// Removes an agent by its uid (never its name, which another agent
    /// may share), once the person has confirmed it; then the account's
    /// agents are read again, removed or not. A refusal is kept for Agents
    /// to say.
    func removeAgent(_ agent: AgentsList.Agent) async {
        guard let removed = await Self.result({ try await self.client.removeAgent(agent.uid) }) else { return }
        // Gone even if the list can't be read again just now.
        if case .success = removed { agents?.removeAll { $0.uid == agent.uid } }
        await refreshAgentList()
        if case let .failure(error) = removed { agentRemoveError = error } else { agentRemoveError = nil }
    }

    /// The account's rules as `rules list` last gave them (or a change's
    /// answer, which carries them), and why the last read failed.
    private(set) var rules: RulesList?
    private(set) var rulesError: LatchError?
    /// Rules' filters, while the window is open: one agent (by uid) and one
    /// host (by key), each nil for any.
    var rulesFilter = RulesFilter()
    var selectedRule: RulesList.Rule.ID?
    /// Why the last delete from the list failed, until it's dismissed.
    var ruleDeleteError: LatchError?

    struct RulesFilter: Equatable {
        var agent: String?
        var host: String?
    }

    /// Counts the changes saved: a read begun before the last of them
    /// answers with the rules as they were before it.
    @ObservationIgnored private var ruleChanges = 0

    /// Reads the account's rules again; a failure keeps what was held, and
    /// an answer older than a change saved meanwhile is dropped.
    func refreshRules() async {
        let changes = ruleChanges
        guard let read = await Self.result({ try await self.client.rulesList() }), changes == ruleChanges else { return }
        switch read {
        case let .success(list):
            rules = list
            rulesError = nil
        case let .failure(error):
            rulesError = error
        }
    }

    /// The rules Rules lists: those its filters match. A rule for all
    /// agents or all hosts matches any one.
    func ruleRows(_ list: RulesList) -> [RulesList.Rule] {
        list.rules.filter { rule in
            (rulesFilter.agent.map(rule.agents.contains) ?? true) && (rulesFilter.host.map(rule.hosts.contains) ?? true)
        }
    }

    /// Adds a rule and selects it.
    func addRule(_ text: String, agents: RulesList.Scope, hosts: RulesList.Scope) async throws {
        try await ruleChange { try await self.client.rulesAdd(text, agents: agents, hosts: hosts) }
    }

    /// Changes a rule: only what's given; a scope given replaces its own.
    func editRule(_ id: String, text: String?, agents: RulesList.Scope?, hosts: RulesList.Scope?) async throws {
        try await ruleChange { try await self.client.rulesEdit(id, text: text, agents: agents, hosts: hosts) }
    }

    func removeRule(_ rule: RulesList.Rule) async throws {
        try await ruleChange { try await self.client.rulesRemove(rule.id) }
    }

    /// The list's Delete, once confirmed; a refusal is kept for Rules to say.
    func deleteRule(_ rule: RulesList.Rule) async {
        do {
            try await removeRule(rule)
            ruleDeleteError = nil
        } catch {
            ruleDeleteError = .from(error)
        }
    }

    /// A change saved, on this Mac and whichever hosts latch reached: its
    /// answer is the account's rules after it, held at once over any read
    /// still under way; the rule added or changed is selected. Then the
    /// rules are read again, without holding up whoever saved.
    private func ruleChange(_ change: () async throws -> RulesWrite) async throws {
        let written = try await change()
        ruleChanges += 1
        rules = written.list
        rulesError = nil
        if written.change.kind == "remove" {
            if selectedRule == written.change.id { selectedRule = nil }
        } else if let id = written.change.id {
            selectedRule = id
        }
        Task { await refreshRules() }
    }

    /// For snapshots: as `rules list` said.
    func stage(rules: RulesList?, error: LatchError? = nil) {
        self.rules = rules
        rulesError = error
    }

    /// Rules' Try a Request.
    let trial = Trial()

    /// `latch rules try` on a host, judged on its saved rules.
    func tryRequest(_ request: [String], on host: HostEntry, agent: String?, newAgent: Bool) async throws -> RulesTry {
        try await client.rulesTry(.words(request), host: host.isThisMac ? nil : host.name, agent: agent, newAgent: newAgent, draft: nil)
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

    /// The feed Home or Activity shows for the selected host, while one of
    /// them is the screen; the window runs it.
    var activityFeed: ActivityFeed? {
        guard case let .host(screen, host?) = route, screen == .home || screen == .activity,
              host.isThisMac || host.online != false else { return nil }
        return feed(for: host)
    }

    /// The window closed: every feed stops now, not after its grace.
    func endFeeds() {
        feeds.values.forEach { $0.end() }
    }

    func feed(for host: HostEntry) -> ActivityFeed {
        if let feed = feeds[host.name] { return feed }
        let feed = ActivityFeed(client: client, host: host.isThisMac ? nil : host.name)
        feeds[host.name] = feed
        return feed
    }

    /// The key on records of a host's requests of itself: this Mac's own,
    /// else Plow's for that host.
    func key(of host: HostEntry) -> String? {
        host.isThisMac ? status?.thisMachine.key ?? host.key : host.key
    }

    /// The record Activity's inspector shows, on the host it was selected
    /// on; Home's recent rows set it.
    var selectedRecord: OnHost?
    /// Activity's filters, kept as hosts change: its outcome, connections
    /// and the owner's reads shown too, text to search for, and the one
    /// agent asked for on one host.
    var activityFilter = ActivityFilter.all
    var showConnections = false
    var activitySearch = ""
    var activityAgent: AgentTarget?

    /// A host's requests, or one agent's there: that host's Activity with
    /// every outcome, or the one asked for, no search, and no connections or
    /// owner's reads, which Home's counts leave out.
    func openActivity(_ host: HostEntry, agent uid: String? = nil, showing filter: ActivityFilter = .all) {
        chosenHost = host.name
        place = .host(.activity)
        activityAgent = uid.map { AgentTarget(host: host.name, agent: $0) }
        activityFilter = filter
        activitySearch = ""
        showConnections = false
    }

    /// One request on a host: that host's Activity, the request selected
    /// and shown in the inspector, opened if it was closed. Filters it
    /// passes are kept; if one would hide it, the outcome, search and agent
    /// filters go.
    func openRecord(_ record: AuditRecord, on host: HostEntry) {
        if activityRows([record], on: host).isEmpty {
            openActivity(host)
        } else {
            chosenHost = host.name
            place = .host(.activity)
        }
        selectedRecord = OnHost(host: host.name, id: record.id)
        inspecting.insert(.activity)
    }

    /// The record the inspector shows on a host: the one selected there,
    /// while Activity lists it.
    func inspectedRecord(on host: HostEntry) -> AuditRecord? {
        guard let selected = selectedRecord, selected.host == host.name,
              let record = feeds[host.name]?.records.last(where: { $0.id == selected.id }) else { return nil }
        return activityRows([record], on: host).isEmpty ? nil : record
    }

    /// Which of the screens with an inspector show it; each keeps its own.
    var inspecting: Set<HostScreen> = [.activity, .vault]

    /// The agent Activity narrows to on a host: only the one asked for
    /// there.
    func activityAgent(on host: HostEntry) -> String? {
        activityAgent?.host == host.name ? activityAgent?.agent : nil
    }

    /// What Activity lists of a host's loaded records, newest first: those
    /// its filters all match. The agent is matched by uid, never by name.
    func activityRows(_ records: [AuditRecord], on host: HostEntry) -> [AuditRecord] {
        let agent = activityAgent(on: host), hostKey = key(of: host)
        let query = activitySearch.trimmingCharacters(in: .whitespaces).lowercased()
        return records.reversed().filter { record in
            guard showConnections || !record.isBackground else { return false }
            if let agent, record.agent != agent { return false }
            switch activityFilter {
            case .all: break
            case .allowed: guard record.decision == "allow" else { return false }
            case .refused: guard record.isRefused else { return false }
            }
            guard !query.isEmpty else { return true }
            let caller = record.callerLabel(hostKey: hostKey, hostName: host.name, isThisMac: host.isThisMac)
            return [record.command ?? "", caller.who, caller.kind, record.status.title].contains { $0.lowercased().contains(query) }
        }
    }

    /// Follows `status --local --follow` until the main window closes, so
    /// this Mac's state is always latch's latest without asking again.
    func followStatus() async {
        do {
            for try await status in client.statusFollow() {
                statusChanged(status)
            }
        } catch is CancellationError {
        } catch {
            statusError = .from(error)
        }
    }

    /// A new `status --local` from the stream.
    func statusChanged(_ status: LocalStatus) {
        self.status = status
        statusError = nil
        leaveMainIfLoggedOut()
    }

    /// A start from the main window, while it runs and how it failed.
    private(set) var startingHost = false
    private(set) var startHostError: LatchError?

    /// Starts this Mac's host from the main window, then reads back what
    /// latch and Plow say.
    func startHosting() async {
        startingHost = true
        startHostError = nil
        defer { startingHost = false }
        do {
            try await startHost()
        } catch {
            startHostError = .from(error)
        }
        await refreshStatus()
        await refreshAccount()
    }

    /// Starts this Mac's host the way `setup --status` says (with the last
    /// serve's flags when there were any). Nothing to do when it isn't
    /// needed. A host latch says can't start now throws, with latch's reason
    /// as detail.
    func startHost() async throws {
        guard let host = try await client.setupStatus().step("host") else { return }
        switch host.state {
        case "needed": if let args = host.args { try await client.perform(args) }
        case "blocked": throw LatchError.unreachable("Latch can't start on this Mac right now.", detail: host.reason)
        default: break
        }
    }

    func refreshStatus() async {
        do {
            status = try await client.localStatus()
            statusError = nil
        } catch {
            statusError = .from(error)
        }
    }

    /// The onboarding window opened with no flow (at launch): asks latch
    /// whether this Mac is logged in and resumes where the person left off.
    func resumeOnboarding() async {
        guard onboarding == nil else { return }
        await refreshStatus()
        let progress = store.load()
        switch progress.destination(loggedIn: status?.login != nil) {
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
        guard case let .onboarding(step) = progress.destination(loggedIn: status?.login != nil) else { return }
        beginOnboarding(at: step, progress: progress)
    }

    /// The main window learned this Mac is logged out (a `latch logout`):
    /// onboarding takes over, at Log in.
    func leaveMainIfLoggedOut() {
        guard let status, status.login == nil, onboarding == nil, !loggingOut, logOutWarning == nil else { return }
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
    func stage(status: LocalStatus?, error: LatchError? = nil) {
        self.status = status
        statusError = error
    }

    /// For snapshots: as `latch status` said.
    func stage(account: AccountStatus?, error: LatchError? = nil) {
        self.account = account
        accountError = error
        accountReadAt = account.map { _ in .now }
    }
}

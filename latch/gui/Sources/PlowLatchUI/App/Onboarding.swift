import AppKit
import Observation
import ServiceManagement

/// The onboarding flow: which screen shows, each screen's state, and what
/// it asks latch. Views only draw it and call its intents.
@Observable
final class Onboarding {
    enum Login: Equatable {
        case starting
        case waiting(LoginCode)
        case expired(LoginCode)
        case failed(String)
        /// Plow already has a machine by this Mac's name; the text was
        /// received, so another name logs in with no second text.
        case nameTaken
        case verified(name: String)
    }

    enum Check: Equatable {
        case unknown
        case checking
        case done
        case needed(String)
        case failed(String)

        var isNeeded: Bool { if case .needed = self { true } else { false } }
    }

    enum Preview: Equatable {
        case notChecked
        case checking
        case verdict(RulesTry)
        case failed(String)
    }

    /// An example request on the rules screen: what a person reads, its
    /// symbol, and the request after `latch rules try --`.
    nonisolated struct Example: Identifiable, Equatable, Sendable {
        var title: String
        var symbol: String
        var shown: String
        var request: [String]
        var id: String { title }
    }

    /// A default: the rules it fills in, and the requests it is tried on.
    nonisolated struct Deck: Identifiable, Equatable, Sendable {
        var name: String
        var rules: String
        var examples: [Example]
        var id: String { name }
    }

    static let decks = [
        Deck(name: "Personal assistant",
             rules: "Allow agents to check our calendar, find messages about dinner, order groceries online and save shopping lists in ~/Documents. Do not send email or write files outside ~/Documents.",
             examples: [
                 Example(title: "Check our calendar", symbol: "calendar",
                         shown: "latch google -- calendar events primary --from 2026-10-26 --to 2026-11-02",
                         request: ["google", "--", "calendar", "events", "primary", "--from", "2026-10-26", "--to", "2026-11-02"]),
                 Example(title: "Find messages about dinner", symbol: "message",
                         shown: "latch messages search dinner", request: ["messages", "search", "dinner"]),
                 Example(title: "Start a shopping list", symbol: "cart",
                         shown: "latch exec --write ~/Documents -- touch shopping-list.txt",
                         request: ["exec", "--write", "~/Documents", "--", "touch", "shopping-list.txt"]),
                 Example(title: "Write anywhere on this Mac", symbol: "folder",
                         shown: "latch exec --write / --network -- npm install",
                         request: ["exec", "--write", "/", "--network", "--", "npm", "install"]),
                 Example(title: "Send an email", symbol: "envelope",
                         shown: #"latch google -- gmail send --to jordan@example.com --subject Update --body "Ready for review.""#,
                         request: ["google", "--", "gmail", "send", "--to", "jordan@example.com", "--subject", "Update", "--body", "Ready for review."]),
             ]),
        Deck(name: "Executive assistant",
             rules: "Allow agents to read and send work email, check my calendar and install dependencies in ~/work with network access. Do not read my personal messages.",
             examples: [
                 Example(title: "Find unread email", symbol: "envelope",
                         shown: "latch google -- gmail search is:unread", request: ["google", "--", "gmail", "search", "is:unread"]),
                 Example(title: "Send an email", symbol: "arrowshape.turn.up.left",
                         shown: #"latch google -- gmail send --to jordan@example.com --subject Update --body "Ready for review.""#,
                         request: ["google", "--", "gmail", "send", "--to", "jordan@example.com", "--subject", "Update", "--body", "Ready for review."]),
                 Example(title: "Check when I'm free", symbol: "calendar",
                         shown: "latch google -- calendar freebusy primary --from 2026-10-26 --to 2026-11-02",
                         request: ["google", "--", "calendar", "freebusy", "primary", "--from", "2026-10-26", "--to", "2026-11-02"]),
                 Example(title: "Install project dependencies", symbol: "shippingbox",
                         shown: "latch exec --write ~/work/demo --network -- npm install",
                         request: ["exec", "--write", "~/work/demo", "--network", "--", "npm", "install"]),
                 Example(title: "Search personal messages", symbol: "message",
                         shown: "latch messages search dinner", request: ["messages", "search", "dinner"]),
             ]),
    ]

    private(set) var step: Step
    private(set) var progress: Progress
    var login: Login = .starting
    /// "Use another name": what the person typed.
    var loginName = ""
    // Set your rules.
    var savedRules: String?
    var draftRules = ""
    var rulesError: String?
    var savingRules = false
    /// The requests the rules are tried on: the first default's until the
    /// person picks one.
    var deck = Onboarding.decks[0]
    var previews: [String: Preview] = [:]
    /// Why the examples show no verdicts, when they can't.
    var previewNote: String?
    /// While latch is judging the examples (the beam scans).
    var reviewing = false
    /// The Mac lights up briefly when a request is allowed through.
    var macLit = false
    /// Which examples show their request and verdict.
    var openExamples: Set<String> = []
    // Connect Google.
    var google: Check = .unknown
    var googleAccounts: [String] = []
    /// Where latch says to connect Google, once it has said.
    var googleConnectURL: URL?
    /// The person went to Plow to connect Google: coming back checks again.
    private var connectingGoogle = false
    // Contacts & Messages.
    var messages: Check = .unknown
    var contacts: Check = .unknown
    var waitingForAccess = false
    var restarting = false
    /// Why Restart Latch failed, while it shows.
    var restartError: String?
    // Start Latch.
    /// Starting at login runs `serve -d --again`, which needs a host this
    /// Mac started in the background before; so it's only on with it.
    var runInBackground = true {
        didSet { if !runInBackground { startAtLogin = false } }
    }
    var startAtLogin = true
    var starting = false
    var startError: String?
    var started = false

    let app: AppModel
    private var loginTask: Task<Void, Never>?
    private var accessTask: Task<Void, Never>?

    init(app: AppModel, step: Step, progress: Progress) {
        self.app = app
        self.step = step
        self.progress = progress
        // A rerun shows the login item as it is; a first run offers it on.
        if !app.loginItemOffered {
            startAtLogin = false
        } else if progress.rerun {
            startAtLogin = app.loginItemIsOn()
        }
    }

    private var client: any LatchClient { app.client }

    // MARK: Moving between screens

    /// Continue: records the screen as passed and shows the next one.
    func next() { leave(.continued) }

    /// While Continue saves the rules or starts Latch, Back and Skip wait:
    /// its answer moves on from the screen it was asked on.
    var busy: Bool { savingRules || starting }

    /// Skip for now, on the optional screens.
    func skip() {
        guard step.skippable, !busy else { return }
        leave(.skipped)
    }

    func back() {
        guard !busy, let index = Step.allCases.firstIndex(of: step), index > 0 else { return }
        show(Step.allCases[index - 1])
    }

    private func leave(_ how: Passed) {
        progress.passed[step] = how
        app.store.save(progress)
        guard let index = Step.allCases.firstIndex(of: step), index + 1 < Step.allCases.count else { return }
        show(Step.allCases[index + 1])
    }

    func show(_ next: Step) {
        stopWork()
        step = next
    }

    /// Done's "Open Latch": onboarding is finished; the main window takes over.
    func finish() {
        progress.passed[.done] = .continued
        progress.finished = true
        app.store.save(progress)
        stopWork()
        app.finishOnboarding()
    }

    /// Stops what a screen started: leaving it, or closing the window.
    func stopWork() {
        loginTask?.cancel()
        loginTask = nil
        accessTask?.cancel()
        accessTask = nil
        waitingForAccess = false
    }

    /// The footer's main button on each screen: its title, whether it can
    /// be pressed now, and what it does.
    var primaryTitle: String {
        switch step {
        case .welcome: "Get started"
        case .login, .privacy: "Continue"
        case .rules: savingRules ? "Saving…" : rulesChanged ? "Save rules" : "Continue"
        case .google: google == .unknown ? "Check connection" : "Continue"
        case .access: accessDone || accessFailed ? "Continue" : "Open System Settings"
        case .start: starting ? "Starting…" : runInBackground && app.status?.serve.ready != true ? "Start Latch" : "Continue"
        case .done: "Open Latch"
        }
    }

    var primaryEnabled: Bool {
        switch step {
        case .login: verified
        case .rules: !savingRules
        case .google: google != .checking
        case .access: accessDone || !waitingForAccess
        case .start: !starting
        default: true
        }
    }

    func primary() {
        guard primaryEnabled else { return }
        switch step {
        case .welcome, .login, .privacy: next()
        case .rules: Task { await saveRules() }
        case .google:
            if google == .done { next() } else if google == .unknown { Task { await checkGoogle() } } else { skip() }
        case .access: if accessDone { next() } else if accessFailed { skip() } else { requestAccess() }
        case .start: Task { await startLatch() }
        case .done: finish()
        }
    }

    private var verified: Bool {
        if case .verified = login { true } else { false }
    }

    var accessDone: Bool { messages == .done && contacts == .done }

    /// A step already set up shows Continue alone, not Skip for now.
    var stepDone: Bool {
        switch step {
        case .rules: !(savedRules ?? "").isEmpty && !rulesChanged
        case .google: google == .done
        case .access: accessDone
        default: false
        }
    }
    /// Continue moves on without it: the step couldn't be set up here, and
    /// checking again is the screen's own button, so Skip for now would say
    /// the same thing twice.
    var primarySkips: Bool {
        switch step {
        case .google: google.isNeeded || { if case .failed = google { true } else { false } }()
        case .access: !accessDone && accessFailed
        default: false
        }
    }

    /// A connector failed for a reason other than access: System Settings
    /// won't help.
    var accessFailed: Bool {
        [messages, contacts].contains { if case .failed = $0 { true } else { false } }
    }

    /// Whether Continue on the rules screen saves: the box holds rules
    /// other than the saved ones.
    var rulesChanged: Bool {
        let draft = draftRules.trimmingCharacters(in: .whitespacesAndNewlines)
        return !draft.isEmpty && draft != savedRules?.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: Log in to Plow

    /// Runs `latch login --events` until it ends or the screen goes. A Mac
    /// already logged in goes straight to verified. `name`, when given, is
    /// the name this Mac takes in Plow.
    func startLogin(name: String? = nil) {
        loginTask?.cancel()
        login = .starting
        let events = client.login(name: name)
        loginTask = Task {
            do {
                for try await event in events {
                    switch event {
                    case let .code(code): login = .waiting(code)
                    case .waiting: break
                    case let .verified(name):
                        login = .verified(name: name)
                        await app.refreshStatus()
                    case .expired:
                        if case let .waiting(code) = login { login = .expired(code) } else { login = .failed("This code expired.") }
                    case let .error(failure): login = failure.code == "conflict" ? .nameTaken : .failed(failure.message)
                    }
                }
            } catch is CancellationError {
            } catch {
                switch login {
                case .verified, .expired, .nameTaken: return
                default: login = .failed((error as? LatchError)?.message ?? error.localizedDescription)
                }
            }
        }
    }

    /// "Use this name": logs in again under the name typed.
    func useAnotherName() {
        let name = loginName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        startLogin(name: name)
    }

    /// The person clicked the code: Messages opens with the text filled in.
    func openMessages() {
        switch login {
        case let .waiting(code), let .expired(code): app.open(code.smsLink)
        default: break
        }
    }

    // MARK: Set your rules

    /// Reads the saved shared rules into the box, then reviews the examples.
    func loadRules() async {
        do {
            let saved = try await client.rulesShow().everyAgent?.text
            savedRules = saved
            if draftRules.isEmpty { draftRules = saved ?? "" }
            rulesError = nil
        } catch {
            rulesError = (error as? LatchError)?.message ?? error.localizedDescription
        }
        startReview()
    }

    /// "Use a default": its rules in the box, its requests in the deck,
    /// judged at once.
    func choose(_ deck: Deck) {
        self.deck = deck
        openExamples = []
        draftRules = deck.rules
        startReview()
    }

    /// The examples show no verdicts yet: `rules try` judges the saved
    /// rules as the owner, while this screen needs an unsaved draft judged
    /// as a new agent would be, which comes with a later latch.
    private func startReview() {
        for example in deck.examples { previews[example.id] = .notChecked }
        previewNote = "Previews arrive with the next Latch update."
    }

    /// Continue: saves the rules only if the person changed them. An empty
    /// box never clears rules that are saved.
    func saveRules() async {
        guard rulesChanged else { return next() }
        let draft = draftRules.trimmingCharacters(in: .whitespacesAndNewlines)
        savingRules = true
        defer { savingRules = false }
        do {
            try await client.rulesSet(draft)
            savedRules = draft
            rulesError = nil
            next()
        } catch {
            rulesError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    // MARK: Connect Google

    /// `latch plugins enable google`: connected, or Google isn't connected
    /// in Plow yet.
    func checkGoogle() async {
        google = .checking
        do {
            googleAccounts = try await client.enable("google")?.accounts ?? []
            google = .done
        } catch let LatchError.failed(failure) where failure.code == "google_not_connected" {
            google = .needed(failure.message)
            googleConnectURL = failure.connectUrl
        } catch {
            google = .failed((error as? LatchError)?.message ?? error.localizedDescription)
        }
    }

    /// "Connect Google in Plow": the page latch names; only ever on a click.
    func connectGoogleInPlow() {
        guard let url = googleConnectURL else { return }
        connectingGoogle = true
        app.open(url)
    }

    /// Arriving at Google: on when latch says it's on; otherwise Not
    /// checked until the person checks. Looking never turns it on.
    func googleAppeared() async {
        guard google == .unknown, let setup = try? await client.setupStatus() else { return }
        if setup.step("google")?.enabled == true { google = .done }
    }

    /// Back in the app: from connecting Google in Plow, checks again;
    /// otherwise only reads what latch says.
    func returnedToApp() async {
        guard step == .google, google != .done, google != .checking else { return }
        if connectingGoogle {
            connectingGoogle = false
            await checkGoogle()
        } else {
            await googleAppeared()
        }
    }

    // MARK: Contacts & Messages

    /// Opens System Settings at Full Disk Access with the app shown in the
    /// Finder, then checks every 2 s for up to 3 minutes while this screen
    /// shows, as `latch setup` does.
    func requestAccess() {
        accessTask?.cancel()
        accessTask = Task {
            var opened = false
            waitingForAccess = true
            defer { waitingForAccess = false }
            let deadline = ContinuousClock.now + .seconds(180)
            while !Task.isCancelled {
                for plugin in ["messages", "contacts"] where check(plugin) != .done {
                    let result = await enableAccess(plugin)
                    // Left the screen meanwhile: a late answer changes nothing.
                    guard !Task.isCancelled else { return }
                    set(plugin, result.check)
                    if let failure = result.failure, !opened, let settings = failure.settings.flatMap(URL.init(string:)) {
                        opened = true
                        app.open(settings)
                        if let grant = failure.grant { app.reveal(URL(filePath: grant)) }
                    }
                }
                if messages == .done && contacts == .done { return }
                if [messages, contacts].contains(where: { if case .failed = $0 { true } else { false } }) { return }
                guard ContinuousClock.now < deadline else { return }
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    /// Arriving at Contacts & Messages: what latch says is on shows as on;
    /// each connector that isn't is tried once (`plugins enable`), opening
    /// nothing, so access granted earlier shows at once. A connector that's
    /// on is never enabled again.
    func accessAppeared() async {
        guard let steps = try? await client.setupStatus().steps else { return }
        for plugin in ["messages", "contacts"] where steps.first(where: { $0.step == plugin })?.enabled == true {
            set(plugin, .done)
        }
        checkAccess()
    }

    /// One check of each connector not on yet, opening nothing.
    func checkAccess() {
        accessTask?.cancel()
        accessTask = Task {
            for plugin in ["messages", "contacts"] where check(plugin) != .done {
                set(plugin, .checking)
                let result = await enableAccess(plugin)
                guard !Task.isCancelled else { return }
                set(plugin, result.check)
            }
        }
    }

    /// Whether access is missing while a host latch started in the
    /// background runs: macOS gives a running process no access granted
    /// after it started, so latch must restart to see it.
    var backgroundHostNeedsRestart: Bool {
        guard messages.isNeeded || contacts.isNeeded, let serve = app.status?.serve else { return false }
        return serve.ready && serve.managed && serve.attached != true
    }

    /// The same, for a host held by a terminal: the person restarts it
    /// there; the app never does.
    var terminalHostNeedsRestart: Bool {
        guard messages.isNeeded || contacts.isNeeded, let serve = app.status?.serve else { return false }
        return serve.ready && !backgroundHostNeedsRestart
    }

    /// The person's "Restart Latch": stops the background host and starts
    /// it again as latch says to, then checks access again. A failure stays
    /// on screen, with Try again, which starts it (stopping it first only
    /// if it is still running).
    func restartLatch() async {
        restarting = true
        defer { restarting = false }
        restartError = nil
        do {
            if app.status?.serve.running == true { try await client.stop() }
            try await startHost()
            await app.refreshStatus()
            checkAccess()
        } catch {
            restartError = (error as? LatchError)?.message ?? error.localizedDescription
            await app.refreshStatus()
        }
    }

    private func enableAccess(_ plugin: String) async -> (check: Check, failure: LatchFailure?) {
        do {
            _ = try await client.enable(plugin)
            return (.done, nil)
        } catch let LatchError.failed(failure) where failure.code == "needs_setup" && failure.settings != nil {
            return (.needed("Waiting for access"), failure)
        } catch {
            return (.failed((error as? LatchError)?.message ?? error.localizedDescription), nil)
        }
    }

    func check(_ plugin: String) -> Check { plugin == "messages" ? messages : contacts }

    private func set(_ plugin: String, _ check: Check) {
        if plugin == "messages" { messages = check } else { contacts = check }
    }

    // MARK: Start Latch

    /// Starts the host the way `setup --status` says (with the last serve's
    /// flags when there were any). Nothing to do when it isn't needed.
    private func startHost() async throws {
        guard let host = try await client.setupStatus().step("host"), host.state == "needed", let args = host.args else { return }
        try await client.perform(args)
    }

    /// Starts the host in the background and sets the login item, as the
    /// toggles say.
    func startLatch() async {
        starting = true
        defer { starting = false }
        startError = nil
        do {
            if runInBackground, app.status?.serve.ready != true { try await startHost() }
            if app.loginItemOffered { try app.setLoginItem(runInBackground && startAtLogin) }
            started = runInBackground
            await app.refreshStatus()
            next()
        } catch {
            startError = (error as? LatchError)?.message ?? error.localizedDescription
            await app.refreshStatus()
        }
    }

    /// Puts the flow at a given point without running it, for snapshots.
    func stage(_ step: Step) {
        self.step = step
    }
}

/// "Start when you log in": the agent in the app's bundle that runs its
/// latch's `serve -d --again` at login (Contents/Library/LaunchAgents),
/// registered with ServiceManagement, so System Settings ▸ General ▸ Login
/// Items lists the app.
enum LoginItem {
    static var service: SMAppService {
        .agent(plistName: "\(Bundle.main.bundleIdentifier ?? "co.plow.latch.cli").login.plist")
    }

    static var isOn: Bool { service.status == .enabled }

    static func set(_ on: Bool) throws {
        switch (on, service.status) {
        case (true, .enabled), (false, .notRegistered), (false, .notFound): return
        case (true, _): try service.register()
        case (false, _): try service.unregister()
        }
    }
}

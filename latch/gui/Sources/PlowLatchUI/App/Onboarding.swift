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
             rules: "Allow my family assistant to check our calendar, search messages about dinner, and order groceries online. Never let it send email or write outside the project folder.",
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
             rules: "Allow my work assistant to read and send work email, check my calendar, and install dependencies in ~/work/demo with network access. Keep it out of my personal messages.",
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
    /// Each edit starts a new review; a late answer to an older one is dropped.
    private var review = 0
    private var reviewTask: Task<Void, Never>?
    // Connect Google.
    var google: Check = .unknown
    var googleAccounts: [String] = []
    /// Where latch says to connect Google, once it has said.
    var googleConnectURL: URL?
    // Contacts & Messages.
    var messages: Check = .unknown
    var contacts: Check = .unknown
    var waitingForAccess = false
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
    }

    private var client: any LatchClient { app.client }

    // MARK: Moving between screens

    /// Continue: records the screen as passed and shows the next one.
    func next() { leave(.continued) }

    /// Skip for now, on the optional screens.
    func skip() {
        guard step.skippable else { return }
        leave(.skipped)
    }

    func back() {
        guard let index = Step.allCases.firstIndex(of: step), index > 0 else { return }
        show(Step.allCases[index - 1])
    }

    private func leave(_ how: Passed) {
        progress.passed[step] = how
        progress.save()
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
        progress.save()
        stopWork()
        app.finishOnboarding()
    }

    /// Stops what a screen started: leaving it, or closing the window.
    func stopWork() {
        loginTask?.cancel()
        loginTask = nil
        reviewTask?.cancel()
        reviewTask = nil
        reviewing = false
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
        case .google: google == .done ? "Continue" : google == .unknown ? "Check connection" : "Check again"
        case .access: accessDone ? "Continue" : accessFailed ? "Check again" : "Open System Settings"
        case .start: starting ? "Starting…" : runInBackground && app.status?.serve.state != "serving" ? "Start Latch" : "Continue"
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
        case .google: if google == .done { next() } else { Task { await checkGoogle() } }
        case .access: if accessDone { next() } else { requestAccess() }
        case .start: Task { await startLatch() }
        case .done: finish()
        }
    }

    private var verified: Bool {
        if case .verified = login { true } else { false }
    }

    var accessDone: Bool { messages == .done && contacts == .done }
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
    /// already logged in goes straight to verified.
    func startLogin() {
        loginTask?.cancel()
        login = .starting
        let events = client.login()
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
                    case let .error(failure): login = .failed(failure.message)
                    }
                }
            } catch is CancellationError {
            } catch {
                if case .verified = login { return }
                if case .expired = login { return }
                login = .failed((error as? LatchError)?.message ?? error.localizedDescription)
            }
        }
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
        startReview(after: .zero)
    }

    /// The box changed: every verdict is stale at once, and the examples are
    /// judged again a second after the typing stops.
    func rulesEdited() {
        startReview(after: .seconds(1))
    }

    /// "Use a default": its rules in the box, its requests in the deck,
    /// judged at once.
    func choose(_ deck: Deck) {
        self.deck = deck
        openExamples = []
        draftRules = deck.rules
        startReview(after: .zero)
    }

    /// Whether latch can judge the examples as the rules screen needs: an
    /// unsaved draft, judged by its rules alone, as a new agent would be.
    /// Today's `rules try` judges the saved rules as the owner, which only
    /// ever refuses less, so the examples wait for that update.
    static let previewsAvailable = false

    private func startReview(after delay: Duration) {
        review += 1
        let current = review
        reviewTask?.cancel()
        guard Self.previewsAvailable else {
            for example in deck.examples { previews[example.id] = .notChecked }
            previewNote = "Previews arrive with the next Latch update."
            return
        }
        for example in deck.examples { previews[example.id] = .checking }
        previewNote = nil
        reviewTask = Task {
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, current == review else { return }
            await judge(current)
        }
    }

    /// Asks `latch rules try` for every example at once and shows the
    /// verdicts together. A draft can't be judged until latch can preview
    /// unsaved rules, so an edited box shows no verdicts rather than the
    /// saved rules' ones.
    private func judge(_ current: Int) async {
        guard draftRules == (savedRules ?? "") else {
            for example in deck.examples { previews[example.id] = .notChecked }
            previewNote = "The examples can't check unsaved rules yet."
            return
        }
        reviewing = true
        defer { if current == review { reviewing = false } }
        let client = self.client
        // Every example at once; the answers are shown together.
        let asked = deck.examples.map { example in
            (example.id, Task.detached { () -> Result<RulesTry, Error> in
                do { return .success(try await client.rulesTry(example.request)) } catch { return .failure(error) }
            })
        }
        var results: [String: Result<RulesTry, Error>] = [:]
        for (id, task) in asked { results[id] = await task.value }
        guard current == review else { return }
        if results.values.contains(where: { if case let .failure(error as LatchError) = $0 { error.code == "serve_required" } else { false } }) {
            for example in deck.examples { previews[example.id] = .notChecked }
            previewNote = "Previews need Latch running."
            return
        }
        for (id, result) in results {
            switch result {
            case let .success(answer): previews[id] = .verdict(answer)
            // A path this Mac won't let any request use is its refusal,
            // made before the judge is asked, as a real call's would be.
            case let .failure(LatchError.failed(failure)) where failure.code == "bad_path":
                previews[id] = .verdict(RulesTry(verdict: "host_refused", refused: failure))
            case let .failure(error): previews[id] = .failed((error as? LatchError)?.message ?? error.localizedDescription)
            }
        }
        if results.values.contains(where: { if case let .success(answer) = $0 { answer.verdict == "allow" } else { false } }) {
            Task {
                try? await Task.sleep(for: .milliseconds(420))
                macLit = true
                try? await Task.sleep(for: .milliseconds(260))
                macLit = false
            }
        }
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
            google = .needed("Connect Google in Plow, then come back here.")
            googleConnectURL = failure.connectUrl
        } catch {
            google = .failed((error as? LatchError)?.message ?? error.localizedDescription)
        }
    }

    /// "Connect Google in Plow": the page latch names, or Plow's home page
    /// until latch names one. Only ever on a click.
    func connectGoogleInPlow() {
        app.open(googleConnectURL ?? URL(string: "https://plow.co")!)
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

    /// Starts the host in the background (with the last serve's flags when
    /// there were any) and sets the login item, as the toggles say.
    func startLatch() async {
        starting = true
        defer { starting = false }
        startError = nil
        do {
            if runInBackground, app.status?.serve.state != "serving" {
                do {
                    try await client.serve(again: true)
                } catch let error as LatchError where error.code == "nothing_to_start_again" {
                    try await client.serve(again: false)
                }
            }
            try app.setLoginItem(runInBackground && startAtLogin)
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

import AppKit
import Observation
import ServiceManagement
import Synchronization

/// The onboarding flow: which screen shows, each screen's state, and what
/// it asks latch. Views only draw it and call its intents.
@Observable
final class Onboarding {
    enum Login: Equatable {
        case starting
        case waiting(LoginCode)
        case expired(LoginCode)
        case failed(LatchError)
        /// Another of the owner's machines has this Mac's name, and when
        /// Plow last saw it. The text was received, so another name, or
        /// replacing that machine, logs in with no second text.
        case nameTaken(existing: String?, lastSeen: Date?)
        case verified(name: String)
    }

    enum Check: Equatable {
        case unknown
        case checking
        case done
        case needed(String)
        case failed(LatchError)

        var isNeeded: Bool { if case .needed = self { true } else { false } }
    }

    /// Connect Google, as the screen shows it.
    enum Google: Equatable {
        /// Asking latch: on arrival, on coming back, or Try again.
        case checking
        /// No Google account connected in Plow yet.
        case notConnected
        /// The person went to sign in; latch waits for the account.
        case waiting
        /// On here, with an account connected in Plow.
        case connected
        /// Plow didn't answer, so nobody can say whether Google is connected.
        case unreachable
        /// Latch couldn't check or turn Google on: why, in the app's words.
        case failed(String)
    }

    enum Preview: Equatable {
        case notChecked
        case checking
        case verdict(RulesTry)
        case failed(LatchError)
        /// latch gave no answer before `checkDeadline`.
        case noAnswer
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
             rules: "Allow agents to read our calendar, find messages about dinner, order groceries online and save shopping lists in ~/Documents. Do not send email or write files outside ~/Documents.",
             examples: [
                 Example(title: "Read our calendar", symbol: "calendar",
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
             rules: "Allow agents to read and send email, read my calendar and install dependencies in ~/work with network access. Do not read or search this Mac's Messages.",
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
                         shown: "latch exec --write ~/work --network -- npm install",
                         request: ["exec", "--write", "~/work", "--network", "--", "npm", "install"]),
                 Example(title: "Search this Mac's Messages", symbol: "message",
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
    var rulesError: LatchError?
    var savingRules = false
    /// The requests the rules are tried on: the first default's until the
    /// person picks one.
    var deck = Onboarding.decks[0]
    var previews: [String: Preview] = [:]
    /// While latch is judging the examples (the beam scans).
    var reviewing = false
    /// How long an example waits for latch before it says it got no answer.
    var checkDeadline: Duration = .seconds(15)
    /// The draft the examples were last judged on, and which review is the
    /// current one: an answer from an older one is dropped.
    private var reviewed: String?
    private var review = 0
    /// The Mac lights up briefly when a request is allowed through.
    var macLit = false
    /// Which examples show their request and verdict.
    var openExamples: Set<String> = []
    // Connect Google.
    var google: Google = .checking
    var googleAccounts = GoogleAccounts(accounts: [])
    /// Why the last wait for a sign-in ended with no new account, in the
    /// app's words.
    var googleWaitEnded: String?
    /// The newest sign-in link latch gave, kept only to open again.
    private var googleSignInLink: URL?
    private(set) var googleTask: Task<Void, Never>?
    /// Try again's or a return's check, which leaving the screen stops.
    private(set) var googleCheck: Task<Void, Never>?
    /// One look at a time: a return to the app during a check adds none.
    private var lookingAtGoogle = false
    // Contacts & Messages.
    var messages: Check = .unknown
    var contacts: Check = .unknown
    var waitingForAccess = false
    /// Whether Plow Latch has Full Disk Access, as latch last said.
    var fullDiskAccess: String?
    var restarting = false
    /// Why Restart Latch failed, while it shows.
    var restartError: LatchError?
    // Start Latch.
    /// Starting at login runs `serve -d --again`, which needs a host this
    /// Mac started in the background before; so it's only on with it.
    var runInBackground = true {
        didSet { if !runInBackground { startAtLogin = false } }
    }
    var startAtLogin = true
    var starting = false
    var startError: LatchError?
    var started = false

    let app: AppModel
    private var loginTask: Task<Void, Never>?
    private(set) var reviewTask: Task<Void, Never>?
    private(set) var accessTask: Task<Void, Never>?
    /// The Full Disk Access helper, while it floats.
    private var grantGuide: (any GrantGuide)?

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
        reviewTask?.cancel()
        reviewTask = nil
        reviewing = false
        // A review stopped part way is not judged: its rows are unchecked
        // again, and the same text is judged afresh.
        for (id, preview) in previews where preview == .checking { previews[id] = .notChecked }
        reviewed = nil
        googleTask?.cancel()
        googleTask = nil
        googleCheck?.cancel()
        googleCheck = nil
        googleSignInLink = nil
        if google == .waiting { google = .notConnected }
        accessTask?.cancel()
        accessTask = nil
        grantGuide?.stop()
        grantGuide = nil
        waitingForAccess = false
    }

    /// The footer's main button on each screen: its title, whether it can
    /// be pressed now, and what it does.
    var primaryTitle: String {
        switch step {
        case .welcome: "Get started"
        case .login, .privacy: "Continue"
        case .rules: savingRules ? "Saving…" : rulesChanged ? "Save rules" : "Continue"
        case .access: accessDone || accessFailed ? "Continue" : accessGranted ? "Turn on" : "Open System Settings"
        case .start: starting ? "Starting…" : runInBackground && app.status?.thisMachine.hosting.ready != true ? "Start Latch" : "Continue"
        case .done: textAgent.map { "Text \($0.name)" } ?? "Open Latch"
        }
    }

    var primaryEnabled: Bool {
        switch step {
        case .login: verified
        case .rules: !savingRules
        // Continue never skips: until Google is on too, Skip for now does that.
        case .access: accessDone ? google == .connected : accessFailed || !waitingForAccess
        case .start: !starting
        default: true
        }
    }

    func primary() {
        guard primaryEnabled else { return }
        switch step {
        case .welcome, .login, .privacy: next()
        case .rules: Task { await saveRules() }
        case .access:
            if accessDone { next() } else if accessFailed { skip() } else if accessGranted { checkAccess() } else { requestAccess() }
        case .start: Task { await startLatch() }
        case .done: if let textAgent { text(textAgent) } else { finish() }
        }
    }

    /// The agent Done offers to text: the newest with a phone line that is
    /// past provisioning.
    var textAgent: AgentsList.Agent? {
        (app.agents ?? []).filter { $0.line?.number != nil && $0.status != "provisioning" }
            .max { ($0.createdAt ?? "") < ($1.createdAt ?? "") }
    }

    /// The first thing to ask an agent: Messages opens with it filled in,
    /// to the agent's line.
    static let demoRequest = #"Use Latch to say "hello world" out loud on my Mac."#

    /// "Text ‹agent›": Messages, to its line, with the demo request.
    func text(_ agent: AgentsList.Agent) {
        guard let number = agent.line?.number else { return }
        app.open(Self.smsLink(to: number))
    }

    static func smsLink(to number: String) -> URL {
        // Only URL-unreserved characters stay as they are.
        let body = demoRequest.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? ""
        return URL(string: "sms:\(number)?&body=\(body)")!
    }

    private var verified: Bool {
        if case .verified = login { true } else { false }
    }

    var accessDone: Bool { messages == .done && contacts == .done }
    /// latch found Full Disk Access already granted: turning the connectors
    /// on needs no trip to System Settings.
    var accessGranted: Bool { fullDiskAccess == "granted" }

    /// A step already set up shows Continue alone, not Skip for now.
    var stepDone: Bool {
        switch step {
        case .rules: !(savedRules ?? "").isEmpty && !rulesChanged
        case .access: accessDone && google == .connected
        default: false
        }
    }
    /// Continue moves on without it: the step couldn't be set up here, and
    /// checking again is the screen's own button, so Skip for now would say
    /// the same thing twice.
    var primarySkips: Bool {
        switch step {
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
    /// The rules Continue leaves in place: the box's, or, while it's empty,
    /// the saved ones it keeps.
    var previewedRules: String {
        let draft = draftRules.trimmingCharacters(in: .whitespacesAndNewlines)
        return draft.isEmpty ? savedRules?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "" : draft
    }

    var rulesChanged: Bool {
        let draft = draftRules.trimmingCharacters(in: .whitespacesAndNewlines)
        return !draft.isEmpty && draft != savedRules?.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: Log in to Plow

    /// Runs `latch login --events` until it ends or the screen goes. A Mac
    /// already logged in goes straight to verified. `name`, when given, is
    /// the name this Mac takes in Plow; `replace` takes it from the machine
    /// that has it, only ever on the person's choice.
    func startLogin(name: String? = nil, replace: Bool = false) {
        loginTask?.cancel()
        login = .starting
        let events = client.login(name: name, replace: replace)
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
                        if case let .waiting(code) = login { login = .expired(code) } else { login = .failed(.unreachable("This code expired.")) }
                    case let .error(failure) where failure.code == "conflict":
                        login = .nameTaken(existing: failure.existingName, lastSeen: failure.lastSeenAt)
                    case let .error(failure): login = .failed(.failed(failure))
                    }
                }
            } catch is CancellationError {
            } catch {
                switch login {
                case .verified, .expired, .nameTaken: return
                default: login = .failed(.from(error))
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

    /// "Replace studio": that machine leaves the account, signed out of
    /// Plow, and this Mac takes its name.
    func replaceMachine() {
        guard case let .nameTaken(existing?, _) = login else { return }
        startLogin(name: existing, replace: true)
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
            rulesError = .from(error)
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

    /// The box changed: the examples show Checking… at once and are judged
    /// a second (`reviewDelay`) after the person stops typing.
    func draftChanged() {
        guard previewedRules != reviewed else { return }
        startReview(after: reviewDelay)
    }

    var reviewDelay: Duration = .seconds(1)

    /// Judges each example against the draft as a new agent would be judged
    /// (`rules try --new-agent --draft`), all at once, each verdict shown
    /// as it comes, or no answer after `checkDeadline`. Nothing is saved.
    /// A newer review drops older answers.
    private func startReview(after delay: Duration = .zero) {
        let draft = previewedRules
        reviewed = draft
        review += 1
        let current = review
        let examples = deck.examples
        let deadline = checkDeadline
        for example in examples { previews[example.id] = .checking }
        reviewing = true
        macLit = false
        reviewTask?.cancel()
        reviewTask = Task {
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled else { return }
            await withTaskGroup(of: (String, Preview).self) { group in
                for example in examples {
                    group.addTask { [client] in
                        (example.id, await Self.judge(example, draft: draft, client: client, within: deadline))
                    }
                }
                for await (id, preview) in group where current == review && !Task.isCancelled {
                    previews[id] = preview
                }
            }
            guard current == review, !Task.isCancelled else { return }
            reviewing = false
            // An allowed request reaches the Mac: it lights up briefly.
            guard examples.contains(where: { if case let .verdict(answer)? = previews[$0.id] { answer.verdict == "allow" } else { false } })
            else { return }
            try? await Task.sleep(for: .milliseconds(420))
            guard current == review, !Task.isCancelled else { return }
            macLit = true
            try? await Task.sleep(for: .milliseconds(260))
            macLit = false
        }
    }

    /// One example's verdict, or `.noAnswer` once `deadline` passes first.
    /// The row never waits on a latch past it: that latch is stopped, and
    /// whatever it says after is dropped.
    private nonisolated static func judge(_ example: Example, draft: String, client: any LatchClient, within deadline: Duration) async -> Preview {
        let first = FirstAnswer()
        let asking = Task {
            do {
                first.give(.verdict(try await client.rulesTry(example.request, host: nil, agent: nil, newAgent: true, draft: draft)))
            } catch {
                first.give(.failed(.from(error)))
            }
        }
        let timer = Task {
            try? await Task.sleep(for: deadline)
            first.give(.noAnswer)
        }
        let preview = await withTaskCancellationHandler {
            await first.value()
        } onCancel: {
            first.give(.noAnswer)
        }
        asking.cancel()
        timer.cancel()
        return preview
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
            rulesError = .from(error)
        }
    }

    // MARK: Connect Google

    /// What latch says of Google now: on arrival, on every return to the
    /// app, and on Try again. An account connected in Plow turns Google on
    /// here, with no press.
    func checkGoogle() async {
        guard !lookingAtGoogle else { return }
        lookingAtGoogle = true
        defer { lookingAtGoogle = false }
        // Waiting, or already on, the screen keeps saying so until latch answers.
        if google != .waiting, google != .connected { google = .checking }
        let before = googleAccounts.working
        if let seen = await lookAtGoogle() { google = seen }
        // A sign-in finished after the wait ended, adding an account or
        // signing one in again: its timeout no longer holds.
        if googleAccounts.working.contains(where: { !before.contains($0) }) { googleWaitEnded = nil }
    }

    /// Try again, or the app back in front: a check that leaving the
    /// screen stops. One already under way stands for it, and while latch
    /// waits for a sign-in, its answer does.
    func recheckGoogle() {
        guard step == .access, !lookingAtGoogle, google != .waiting else { return }
        googleCheck = Task { await checkGoogle() }
    }

    /// "Connect Google…", "Add Another Account…" or "Sign In Again…", as
    /// `googleAccounts` offers: latch gets a sign-in link from Plow, which
    /// opens in the browser, and waits (up to 5 minutes, while the screen
    /// shows) for the account, then turns Google on; only ever on a click.
    func connectGoogle() {
        let adding = googleAccounts.addsAccount
        googleWaitEnded = nil
        google = .waiting
        googleTask?.cancel()
        googleTask = Task {
            do {
                let enabled = try await app.signInWithGoogle(addAccount: adding) { googleSignInLink = $0 }
                guard !Task.isCancelled else { return }
                googleSignInLink = nil
                guard let enabled else {
                    // latch stopped before it answered: what is there now.
                    google = .checking
                    return await checkGoogle()
                }
                googleAccounts = GoogleAccounts(enabled)
                google = .connected
            } catch {
                guard !Task.isCancelled else { return }
                googleSignInLink = nil
                if let error = error as? LatchError, error.code == "sign_in_timeout" {
                    // Adding, the accounts already there still work.
                    google = adding ? .connected : .notConnected
                    googleWaitEnded = GoogleSignIn.problem(error, adding: adding)
                } else {
                    google = Self.googleProblem(error)
                }
            }
        }
    }

    /// "Open the sign-in page again": the newest link latch gave.
    func openGoogleSignInAgain() {
        if let googleSignInLink { app.open(googleSignInLink) }
    }

    var canOpenGoogleSignInAgain: Bool { googleSignInLink != nil }

    /// For snapshots: latch has given a sign-in link.
    func stage(googleSignInLink: URL) {
        self.googleSignInLink = googleSignInLink
    }

    /// One look: `setup --status` reads what Plow says without changing
    /// anything; an account there and Google off here runs `plugins enable
    /// google`. Nil when the screen went meanwhile.
    private func lookAtGoogle() async -> Google? {
        let setup: SetupStatus
        do {
            setup = try await client.setupStatus()
        } catch {
            return Task.isCancelled ? nil : Self.googleProblem(error)
        }
        guard !Task.isCancelled else { return nil }
        guard setup.step("login")?.state == "done" else { return .failed(Self.notLoggedIn) }
        guard let step = setup.step("google") else { return .failed(Self.couldNotCheck) }
        // Logged in as the owner, Google's step lacks accounts only when
        // Plow couldn't be asked.
        guard let accounts = GoogleAccounts(step) else { return .unreachable }
        googleAccounts = accounts
        if step.state == "done" { return .connected }
        // While latch waits for a sign-in, turning Google on is its to do.
        if google == .waiting { return .waiting }
        guard !accounts.accounts.isEmpty else { return .notConnected }
        // Turning Google on is this screen's alone: not once it was left.
        guard !Task.isCancelled, self.step == .access else { return nil }
        do {
            googleAccounts = try await client.enable("google").map(GoogleAccounts.init) ?? accounts
            return .connected
        } catch {
            return Task.isCancelled ? nil : Self.googleProblem(error)
        }
    }

    static let notLoggedIn = "This Mac isn't logged in to Plow. Go back to log in, then try again."
    static let couldNotCheck = "Latch couldn't check Google just now. Try again."
    static let toolsMissing = "Latch's Google tools are missing from this Mac. Install Latch again, then try again."

    /// latch's refusal, said plainly: decided on its code, never shown.
    private static func googleProblem(_ error: Error) -> Google {
        switch (error as? LatchError)?.code {
        case "plow_unreachable": .unreachable
        case "not_logged_in": .failed(notLoggedIn)
        case "plow_error": .failed("Plow couldn't answer just now. Try again in a moment.")
        case "gog_not_configured": .failed(toolsMissing)
        // No connected account works until the owner signs in again:
        // Connect Google runs latch's sign-in, as for none connected.
        case "google_needs_sign_in": .notConnected
        default: .failed(couldNotCheck)
        }
    }

    // MARK: Contacts & Messages

    /// Opens System Settings at Full Disk Access with the helper floating
    /// under it, then checks every 2 s (`accessRecheck`) for up to 3 minutes
    /// while this screen shows, as `latch setup` does. The helper says when access lands, and
    /// goes when the checks stop.
    func requestAccess() {
        accessTask?.cancel()
        grantGuide?.stop()
        grantGuide = nil
        accessTask = Task {
            var opened = false
            var guide: (any GrantGuide)?
            waitingForAccess = true
            defer {
                waitingForAccess = false
                if accessDone { guide?.granted() } else { guide?.stop() }
            }
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
                        if let grant = failure.grant {
                            guide = app.guideGrant(URL(filePath: grant))
                            grantGuide = guide
                        }
                    }
                }
                if messages == .done && contacts == .done { return }
                if [messages, contacts].contains(where: { if case .failed = $0 { true } else { false } }) { return }
                guard ContinuousClock.now < deadline else { return }
                try? await Task.sleep(for: accessRecheck)
            }
        }
    }

    var accessRecheck: Duration = .seconds(2)

    /// Arriving at Contacts & Messages: what latch says is on shows as on,
    /// and whether Full Disk Access is granted already. Looking never runs
    /// `plugins enable`; turning one on is the person's click.
    func accessAppeared() async {
        guard let setup = try? await client.setupStatusCheckingAccess() else { return }
        fullDiskAccess = setup.step("messages")?.access
        for plugin in ["messages", "contacts"] where setup.step(plugin)?.state == "done" {
            set(plugin, .done)
        }
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
        guard messages.isNeeded || contacts.isNeeded, let serve = app.status?.thisMachine.hosting else { return false }
        return serve.ready && serve.managed && serve.attached != true
    }

    /// The same, for a host held by a terminal: the person restarts it
    /// there; the app never does.
    var terminalHostNeedsRestart: Bool {
        guard messages.isNeeded || contacts.isNeeded, let serve = app.status?.thisMachine.hosting else { return false }
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
            if app.status?.thisMachine.hosting.running == true { try await client.stop() }
            try await app.startHost()
            await app.refreshStatus()
            checkAccess()
        } catch {
            restartError = .from(error)
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
            return (.failed(.from(error)), nil)
        }
    }

    func check(_ plugin: String) -> Check { plugin == "messages" ? messages : contacts }

    private func set(_ plugin: String, _ check: Check) {
        if plugin == "messages" { messages = check } else { contacts = check }
    }

    // MARK: Start Latch

    /// Starts the host in the background and sets the login item, as the
    /// toggles say.
    func startLatch() async {
        starting = true
        defer { starting = false }
        startError = nil
        do {
            if runInBackground, app.status?.thisMachine.hosting.ready != true { try await app.startHost() }
            if app.loginItemOffered { try app.setLoginItem(runInBackground && startAtLogin) }
            started = runInBackground
            await app.refreshStatus()
            next()
        } catch {
            startError = .from(error)
            await app.refreshStatus()
        }
    }

    /// Start is waiting for the login item to be allowed in Login Items.
    var loginItemNeedsApproval: Bool { startError == LoginItem.needsApproval }

    /// "Open Login Items Settings…".
    func openLoginItemsSettings() {
        app.openLoginItems()
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
        try set(on, status: { service.status }, register: { try service.register() }, unregister: { try service.unregister() })
    }

    /// macOS can register the agent but hold it until the person allows it
    /// in Login Items (`requiresApproval`): then it hasn't started at login,
    /// so this throws `needsApproval` and leaves it registered, as Login
    /// Items lists it only while it is.
    static func set(_ on: Bool, status: () -> SMAppService.Status, register: () throws -> Void,
                    unregister: () throws -> Void) throws {
        switch (on, status()) {
        case (true, .enabled), (false, .notRegistered), (false, .notFound): return
        case (true, .requiresApproval): throw needsApproval
        case (true, _):
            do {
                try register()
            } catch where status() == .requiresApproval {
                throw needsApproval
            }
            if status() == .requiresApproval { throw needsApproval }
        case (false, _): try unregister()
        }
    }

    static let needsApproval = LatchError.unreachable(
        "Latch is running now. To start it when you log in, allow \(AppName.shown) in Login Items & Extensions. Then click Continue, or turn off “Start when you log in”.")

    static func openSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}

/// The first answer given; any given after it is dropped.
nonisolated private final class FirstAnswer: Sendable {
    private struct State {
        var answer: Onboarding.Preview?
        var waiting: CheckedContinuation<Onboarding.Preview, Never>?
    }
    private let state = Mutex(State())

    func give(_ answer: Onboarding.Preview) {
        let waiting = state.withLock { state -> CheckedContinuation<Onboarding.Preview, Never>? in
            guard case .none = state.answer else { return nil }
            state.answer = answer
            defer { state.waiting = nil }
            return state.waiting
        }
        waiting?.resume(returning: answer)
    }

    func value() async -> Onboarding.Preview {
        await withCheckedContinuation { continuation in
            let answer = state.withLock { state in
                if case .none = state.answer { state.waiting = continuation }
                return state.answer
            }
            if let answer { continuation.resume(returning: answer) }
        }
    }
}

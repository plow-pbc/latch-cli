import Foundation
import ServiceManagement
import Synchronization
import Testing
@testable import PlowLatchUI

struct ResumeTests {
    @Test func aFreshMacStartsAtWelcome() {
        #expect(Progress().destination(loggedIn: false) == .onboarding(.welcome))
    }

    @Test func notLoggedInAfterWelcomeResumesAtLogIn() {
        let progress = Progress(passed: [.welcome: .continued])
        #expect(progress.destination(loggedIn: false) == .onboarding(.login))
    }

    @Test func loggedInResumesAtTheFirstScreenNotPassed() {
        #expect(Progress().destination(loggedIn: true) == .onboarding(.privacy))
        let skippedRules = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .skipped])
        #expect(skippedRules.destination(loggedIn: true) == .onboarding(.access))
        let allPassed = Progress(passed: [.privacy: .continued, .rules: .continued, .access: .skipped, .start: .continued])
        #expect(allPassed.destination(loggedIn: true) == .onboarding(.done))
    }

    @Test func finishedOpensTheMainWindow() {
        #expect(Progress(finished: true).destination(loggedIn: true) == .main)
    }

    @Test func notLoggedInBeatsFinished() {
        let loggedOut = Progress(passed: [.welcome: .continued, .login: .continued], finished: true)
        #expect(loggedOut.destination(loggedIn: false) == .onboarding(.login))
    }

    /// Logged in is what latch's `login` says: a Mac latch reports signed
    /// out opens onboarding at Log in, though its setup was finished.
    @MainActor @Test func aSignedOutMacOpensOnboardingAtLogIn() async {
        let client = ScriptedClient { _ in nil }
        client.status = Fixtures.loggedOut
        let store = ProgressStore(memory: .init(), stateDir: client.stateDir)
        store.save(Progress(passed: [.welcome: .continued, .login: .continued], finished: true))
        let signedOut = AppModel(client: client, store: store, still: true)
        await signedOut.resumeOnboarding()
        #expect(signedOut.onboarding?.step == .login)
        client.status = Fixtures.serving
        let signedIn = AppModel(client: client, store: store, still: true)
        await signedIn.resumeOnboarding()
        #expect(signedIn.onboarding == nil)
    }

    @Test func setUpAgainWalksEveryScreen() {
        let all = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .continued,
                                    .access: .skipped, .start: .continued, .done: .continued], finished: true)
        let again = all.settingUpAgain()
        #expect(again.destination(loggedIn: true) == .onboarding(.welcome))
        #expect(again.passed.isEmpty)
        #expect(!again.finished)
        // Quit midway through: the next launch carries on from there.
        var midway = again
        for step in [Step.welcome, .login, .privacy, .rules] { midway.passed[step] = .continued }
        #expect(midway.destination(loggedIn: true) == .onboarding(.access))
    }

    @Test func progressIsKeptPerStateFolder() {
        let memory = ProgressStore.Memory()
        let home = ProgressStore(memory: memory, stateDir: "/Users/me/.latch")
        let dev = ProgressStore(memory: memory, stateDir: "/Users/me/dev/.dev/state")
        let progress = Progress(passed: [.welcome: .continued, .access: .skipped], finished: true)
        home.save(progress)
        #expect(home.load() == progress)
        #expect(dev.load() == Progress())
    }
}

struct DecodingTests {
    @Test func loginEventsDecodeAsLatchPrintsThem() throws {
        let code: LoginLine = try Fixtures.decode(Fixtures.code)
        guard case let .code(login) = code.event else { Issue.record("not a code"); return }
        #expect(login.code == "JFO3S")
        #expect(login.smsto == "SMSTO:+15550000001:JFO3S")
        #expect(login.expiresAt == nil)
        let verified: LoginLine = try Fixtures.decode(#"{"type":"verified","peer":{"name":"studio","key":"d9438ef1","kind":"device"}}"#)
        #expect(verified.event == .verified(name: "studio"))
        let error: LoginLine = try Fixtures.decode(#"{"type":"error","error":{"code":"code_expired","message":"the code expired"}}"#)
        #expect(error.event == .error(LatchFailure(code: "code_expired", message: "the code expired")))
    }

    @Test func aLoginSkipsHelloAndEventsItDoesNotKnow() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        let lines = [#"{"type":"hello","schema":1}"#, Fixtures.code, #"{"type":"progress","step":"texted"}"#,
                     #"{"type":"verified","peer":{"name":"studio"}}"#]
        try ("#!/bin/sh\ncat <<'EOF'\n" + lines.joined(separator: "\n") + "\nEOF\n").write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)
        var events: [LoginEvent] = []
        for try await event in LiveLatchClient(executable: latch, stateDir: folder.path).login(name: nil, replace: false) {
            events.append(event)
        }
        #expect(events.count == 2)
        guard case .code = events.first else { Issue.record("not a code first"); return }
        #expect(events.last == .verified(name: "studio"))
    }

    @Test func localStatusDecodes() throws {
        let serving: LocalStatus = try Fixtures.decode(Fixtures.serving)
        #expect(serving.loggedInAs == "studio")
        #expect(serving.thisMachine.hosting.state == "serving")
        let blocked: LocalStatus = try Fixtures.decode(Fixtures.otherVersion)
        #expect(blocked.thisMachine.hosting.blocked?.code == "other_version")
        let loggedOut: LocalStatus = try Fixtures.decode(Fixtures.loggedOut)
        #expect(loggedOut.login == nil && loggedOut.loggedInAs == nil)
    }

    @Test func rulesTryVerdictsDecode() throws {
        let refused: RulesTry = try Fixtures.decode(Fixtures.hostRefused)
        #expect(refused.verdict == "host_refused")
        #expect(refused.refused?.code == "plugin_disabled")
    }

    @Test func timestampsKeepUpToMilliseconds() {
        #expect(LatchJSON.parseTimestamp("2026-10-22T22:05:50.582906Z") != nil)
        #expect(LatchJSON.parseTimestamp("2026-10-22T23:59:00Z") != nil)
    }
}

@MainActor
struct GatekeeperTests {
    @Test func examplesAreJudgedOnTheDraftAsANewAgent() async {
        let client = ScriptedClient { _ in nil }
        client.verdict = { draft, request in draft.contains("Never let it share") && request.words?.first == "exec" ? "owner_said_no" : "allow" }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.choose(Onboarding.decks[0])
        #expect(flow.reviewing)
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .checking })
        await waitUntil { !flow.reviewing }
        let verdicts = flow.deck.examples.map { if case let .verdict(answer)? = flow.previews[$0.id] { answer.verdict } else { "-" } }
        #expect(verdicts == ["allow", "allow", "allow", "owner_said_no", "owner_said_no"])
        #expect(client.tries.allSatisfy { $0.draft == Onboarding.decks[0].rules })
        #expect(client.saved.isEmpty)
    }

    /// The box on arrival: the first default's on a first run, else the
    /// saved rules; the deck is the default whose text that is, else the
    /// last one shown.
    @Test(arguments: [(nil, 0, 0), (Onboarding.decks[1].rules, 0, 1), ("Allow everything.", 1, 1)] as [(String?, Int, Int)])
    func theBoxArrivesFilled(saved: String?, lastShown: Int, expected: Int) async {
        let client = ScriptedClient { _ in nil }
        client.savedText = saved
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.deck = Onboarding.decks[lastShown]
        await flow.loadRules()
        #expect(flow.draftRules == saved ?? Onboarding.decks[0].rules)
        #expect(flow.deck.name == Onboarding.decks[expected].name)
        // A first run's box isn't saved yet; saved rules already are.
        #expect(flow.primaryTitle == (saved == nil ? "Save rules" : "Continue"))
        await waitUntil { !flow.reviewing }
        #expect(client.tries.allSatisfy { $0.draft == saved ?? Onboarding.decks[0].rules })
    }

    /// An emptied box saves nothing and moves on; the saved rules stay.
    @Test func anEmptiedBoxSavesNothing() async {
        let client = ScriptedClient { _ in nil }
        client.savedText = "Allow everything."
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        await flow.loadRules()
        flow.draftRules = ""
        #expect(flow.primaryTitle == "Continue")
        flow.primary()
        await waitUntil { flow.step != .rules }
        #expect(client.saved.isEmpty)
        #expect(flow.savedRules == "Allow everything.")
    }

    @Test func anEmptyBoxPreviewsTheSavedRulesItKeeps() async {
        let client = ScriptedClient { _ in nil }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.reviewDelay = .zero
        flow.savedRules = "Allow everything."
        flow.draftRules = "  "
        flow.draftChanged()
        await waitUntil { !flow.reviewing && !client.tries.isEmpty }
        // Continue keeps the saved rules, so they are what the examples show.
        #expect(flow.primaryTitle == "Continue")
        #expect(!client.tries.isEmpty && client.tries.allSatisfy { $0.draft == "Allow everything." })
    }

    @Test func typingWaitsAPauseAndDropsOlderAnswers() async throws {
        let client = ScriptedClient { _ in nil }
        client.verdict = { draft, _ in draft == "second" ? "not_covered" : "allow" }
        let first = Gate()
        client.holds["try first"] = first
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        // Still typing: checking at once, but nothing asked during the pause.
        flow.reviewDelay = .seconds(3600)
        flow.draftRules = "typing"
        flow.draftChanged()
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .checking })
        // Typing on restarts the pause; once it passes, "first" is asked.
        flow.reviewDelay = .zero
        flow.draftRules = "first"
        flow.draftChanged()
        await waitUntil { client.tries.count == flow.deck.examples.count }
        #expect(client.tries.allSatisfy { $0.draft == "first" })
        // A change while "first" is out: its answers, let go after the
        // newer ones, never show.
        let stale = flow.reviewTask
        flow.draftRules = "second"
        flow.draftChanged()
        await waitUntil { !flow.reviewing }
        first.open()
        await stale?.value
        #expect(flow.deck.examples.allSatisfy { if case let .verdict(answer)? = flow.previews[$0.id] { answer.verdict == "not_covered" } else { false } })
        // The text typed first was never asked about.
        #expect(!client.tries.contains { $0.draft == "typing" })
        // The same text again asks nothing new.
        let asked = client.tries.count
        flow.draftRules = " second "
        flow.draftChanged()
        #expect(!flow.reviewing)
        #expect(client.tries.count == asked)
    }

    @Test func leavingMidReviewLeavesNoRowCheckingAndTheSameTextAsksAgain() async {
        let client = ScriptedClient { _ in nil }
        let deck = Onboarding.decks[1]
        let held = Gate()
        client.holds["try \(deck.rules)"] = held
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.reviewDelay = .zero
        flow.choose(deck)
        await waitUntil { client.tries.count == deck.examples.count }
        let stopped = flow.reviewTask
        flow.stopWork()
        #expect(!deck.examples.contains { flow.previews[$0.id] == .checking })
        // Back on the screen with the same text: it is judged again.
        flow.draftChanged()
        await waitUntil { client.tries.count == 2 * deck.examples.count }
        held.open()
        await waitUntil { deck.examples.allSatisfy { if case .verdict? = flow.previews[$0.id] { true } else { false } } }
        await stopped?.value
    }

    @Test func aCheckThatNeverAnswersSaysSo() async {
        let client = ScriptedClient { _ in nil }
        let deck = Onboarding.decks[1]
        let never = Gate()
        client.holds["try \(deck.rules)"] = never
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.checkDeadline = .milliseconds(50)
        flow.choose(deck)
        await flow.reviewTask?.value
        #expect(!flow.reviewing)
        #expect(deck.examples.allSatisfy { flow.previews[$0.id] == .noAnswer })
        never.open()
    }

    @Test func startAtLoginOnlyWithRunInTheBackground() async {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var loginItem: [Bool] = []
        app.setLoginItem = { loginItem.append($0) }
        let flow = app.stage(.start)
        flow.runInBackground = false
        #expect(!flow.startAtLogin)
        flow.startAtLogin = true
        await flow.startLatch()
        #expect(loginItem == [false])
        let again = app.stage(.start)
        await again.startLatch()
        #expect(loginItem == [false, true])
    }

    /// macOS may register the login item yet hold it for the person to
    /// allow in Login Items: Start stays on its screen, says so, and offers
    /// Login Items in System Settings, opened only on that click; once it's
    /// allowed, Start goes on.
    @Test func startWaitsForTheLoginItemToBeAllowed() async {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var status = SMAppService.Status.notRegistered
        var calls: [String] = []
        app.setLoginItem = { on in
            try LoginItem.set(on, status: { status }, register: { calls.append("register"); status = .requiresApproval },
                              unregister: { calls.append("unregister") })
        }
        var settingsOpened = 0
        app.openLoginItems = { settingsOpened += 1 }
        let flow = app.stage(.start)
        flow.startAtLogin = true
        await flow.startLatch()
        #expect(flow.step == .start)
        #expect(flow.startError == LoginItem.needsApproval && flow.loginItemNeedsApproval)
        // Left registered: Login Items lists it only while it is.
        #expect(calls == ["register"])
        // Settings opens on the button alone.
        #expect(settingsOpened == 0)
        flow.openLoginItemsSettings()
        #expect(settingsOpened == 1)
        // Asked again before it's allowed: still waiting, nothing re-registered.
        await flow.startLatch()
        #expect(flow.step == .start && calls == ["register"])
        // Allowed in Login Items.
        status = .enabled
        await flow.startLatch()
        #expect(flow.step == .done)
        #expect(flow.startError == nil && !flow.loginItemNeedsApproval)
        #expect(settingsOpened == 1)
    }

    /// Registering can also refuse outright while macOS waits for approval.
    @Test func aRegistrationRefusedForApprovalIsSaidAsSuch() {
        var status = SMAppService.Status.notRegistered
        struct Refused: Error {}
        #expect(throws: LoginItem.needsApproval) {
            try LoginItem.set(true, status: { status }, register: { status = .requiresApproval; throw Refused() }, unregister: {})
        }
        status = .notRegistered
        #expect(throws: Refused.self) {
            try LoginItem.set(true, status: { status }, register: { throw Refused() }, unregister: {})
        }
    }

    @Test func googleNotConnectedCarriesLatchsConnectURL() throws {
        struct Line: Decodable { var error: LatchFailure }
        let json = #"{"error":{"code":"google_not_connected","message":"connect Google in Plow","connect_url":"https://plow.co/connect/google"}}"#
        let line = try LatchJSON.decoder.decode(Line.self, from: Data(json.utf8))
        #expect(line.error.connectUrl == URL(string: "https://plow.co/connect/google"))
    }

    @Test func setupRowsComeFromLatchNotFromScreensPassed() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.stage(status: try? Fixtures.decode(Fixtures.serving))
        // Every screen continued, but latch says nothing is on.
        _ = app.stage(.done, progress: Progress(passed: [.rules: .continued, .access: .continued]))
        app.stage(rulesSaved: false, setup: try? Fixtures.decode(Fixtures.setup(on: ["messages"])))
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Logged in", "Not set up", "Not set up", "Not set up", "Running"])
        app.stage(rulesSaved: true, setup: try? Fixtures.decode(Fixtures.setup(on: ["google", "messages", "contacts"])))
        #expect(Summary.rows(app: app, flow: nil).map(\.state) == ["Logged in", "Saved", "Connected", "On", "Running"])
    }

    @Test func aFailedStatusReadSaysSo() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.stage(status: nil, error: .unreachable("Latch returned an unreadable response."))
        app.stage(rulesSaved: nil, setup: nil)
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check"])
    }

    @Test func onlyMissingAccessIsAccessNeeded() {
        #expect(Onboarding.Check.needed("Waiting for access").isNeeded)
        #expect(!Onboarding.Check.failed(.unreachable("the Messages reader is missing")).isNeeded)
    }
}

/// The Full Disk Access helper, as calls made to it.
final class FakeGuide: GrantGuide {
    private(set) var calls: [String] = []
    func granted() { calls.append("granted") }
    func stop() { calls.append("stop") }
}

/// A latch whose `plugins enable` answers as a test says and counts its
/// calls. A call answers at once unless the test holds it (`holds`): the
/// nth `enable N`, the nth `look N` of `setup --status`, `try DRAFT` of
/// `rules try`, or `save` of `rules set`.
final class ScriptedClient: LatchClient, @unchecked Sendable {
    let stateDir = "/Users/me/.latch"
    var status = Fixtures.stopped
    private(set) var stopped = 0
    /// The setup steps run, as `setup --status` gave them.
    private(set) var performed: [[String]] = []
    /// Connectors `setup --status` says are on.
    var connectorsOn: [String] = []
    /// Names `login --events --name` was given.
    private(set) var loginNames: [String?] = []
    /// Whether each login was given `--replace`.
    private(set) var replaced: [Bool] = []
    /// How `login --events` ends.
    var loginEvents: [LoginEvent] = []
    let enableAnswer: @Sendable (String) throws -> GoogleEnabled?
    private(set) var enabled: [String] = []
    var holds: [String: Gate] = [:]

    init(enable: @escaping @Sendable (String) throws -> GoogleEnabled?) {
        self.enableAnswer = enable
    }

    private func held(_ call: String) async {
        await holds[call]?.wait()
    }

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(status) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    /// What `status --local --follow` sends, then it stays open; nil ends it at once.
    var followed: [String]?
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> {
        AsyncThrowingStream { continuation in
            guard let followed else { return continuation.finish() }
            for line in followed { if let status = try? Fixtures.decode(line) as LocalStatus { continuation.yield(status) } }
        }
    }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { $0.finish() } }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> {
        loginNames.append(name)
        replaced.append(replace)
        let events = loginEvents
        return AsyncThrowingStream { continuation in
            for event in events { continuation.yield(event) }
            continuation.finish()
        }
    }
    /// The Google and host steps `setup --status` gives, when not the default.
    var google: String?
    var host: String?
    /// How many times `setup --status` was read.
    private(set) var looked = 0
    func setupStatus() async throws -> SetupStatus {
        looked += 1
        await held("look \(looked)")
        return try Fixtures.decode(host.map { Fixtures.setup(on: connectorsOn, google: google, host: $0) } ?? Fixtures.setup(on: connectorsOn, google: google))
    }
    /// What `--check-access` says of Full Disk Access.
    var access: String?
    func setupStatusCheckingAccess() async throws -> SetupStatus {
        try Fixtures.decode(Fixtures.setup(on: connectorsOn, google: google, access: access))
    }
    /// The shared rules `rules show` gives, if any.
    var savedText: String?
    func rulesShow(host: String?) async throws -> RulesShow {
        let every = try savedText.map { String(decoding: try JSONEncoder().encode(["text": $0, "revision": "r1"]), as: UTF8.self) } ?? "null"
        return try Fixtures.decode(#"{"agents":[],"every_agent":\#(every)}"#)
    }
    func rulesSet(_ text: String) async throws {
        saved.append(text)
        await held("save")
    }
    private(set) var saved: [String] = []
    /// What `rules try` answers for a draft and request, and what it was asked.
    var verdict: @Sendable (String, TryRequest) -> String = { _, _ in "allow" }
    /// `rules try` runs several at once.
    private let asked = Mutex<[(draft: String, request: TryRequest)]>([])
    var tries: [(draft: String, request: TryRequest)] { asked.withLock { $0 } }
    func rulesTry(_ request: TryRequest, host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        let draft = draft ?? ""
        asked.withLock { $0.append((draft, request)) }
        await held("try \(draft)")
        return try Fixtures.decode(#"{"verdict":"\#(verdict(draft, request))","host":"studio","as":"new agent","gate":null}"#)
    }
    /// How a setup step (`serve -d`) fails, if it does.
    var serveFailure: LatchFailure?
    func perform(_ args: [String]) async throws {
        performed.append(args)
        if let serveFailure { throw LatchError.failed(serveFailure) }
    }
    func stop() async throws {
        stopped += 1
        status = Fixtures.stopped
    }
    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        enabled.append(plugin)
        await held("enable \(enabled.count)")
        return try enableAnswer(plugin)
    }
}

@MainActor
struct ClosingTests {
    @Test func cancellingARunStopsLatch() async throws {
        let started = FileManager.default.temporaryDirectory.appending(path: "started-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: started) }
        let process = LatchProcess(executable: URL(filePath: "/bin/sh"), arguments: ["-c", "touch '\(started.path)'; exec sleep 30"])
        let began = ContinuousClock.now
        let run = Task { try await process.output() }
        // Running, not merely asked to start.
        await waitUntil { FileManager.default.fileExists(atPath: started.path) }
        run.cancel()
        let result = await run.result
        #expect(throws: CancellationError.self) { try result.get() }
        #expect(ContinuousClock.now - began < .seconds(3))
    }

    @Test func leavingContactsAndMessagesStopsItsChecks() async throws {
        let notGranted = LatchFailure(code: "needs_setup", message: "no Full Disk Access", grant: "/Applications/Plow Latch.app",
                                      settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient { _ in throw LatchError.failed(notGranted) }
        let reply = Gate()
        client.holds["enable 1"] = reply
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        app.guideGrant = { opened.append($0); return FakeGuide() }
        let flow = app.stage(.access)
        flow.requestAccess()
        await waitUntil { client.enabled == ["messages"] }
        let checks = flow.accessTask
        flow.skip()
        // Messages' refusal arrives after the screen went.
        reply.open()
        await checks?.value
        #expect(flow.step == .start)
        #expect(opened.isEmpty)
        #expect(flow.messages == .unknown)
        #expect(client.enabled == ["messages"])
    }

    @Test func theHelperFloatsUntilAccessLands() async throws {
        let notGranted = LatchFailure(code: "needs_setup", message: "no Full Disk Access", grant: "/Applications/Plow Latch.app",
                                      settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let calls = Mutex(0)
        let client = ScriptedClient { _ in
            // Messages and Contacts both wait for the grant once, then have it.
            if calls.withLock({ $0 += 1; return $0 }) <= 2 { throw LatchError.failed(notGranted) }
            return nil
        }
        // The next check's first answer waits for the test.
        let granted = Gate()
        client.holds["enable 3"] = granted
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let guide = FakeGuide()
        var guided: [URL] = []
        app.guideGrant = { guided.append($0); return guide }
        let flow = app.stage(.access)
        flow.accessRecheck = .zero
        flow.requestAccess()
        await waitUntil { client.enabled.count == 3 }
        #expect(flow.messages.isNeeded && flow.contacts.isNeeded)
        #expect(opened == [URL(string: notGranted.settings!)!])
        #expect(guided == [URL(filePath: "/Applications/Plow Latch.app")])
        #expect(guide.calls.isEmpty)
        granted.open()
        await flow.accessTask?.value
        #expect(flow.accessDone)
        #expect(guide.calls == ["granted"])
        // Leaving afterwards stops it at once, granted or not.
        flow.next()
        #expect(guide.calls == ["granted", "stop"])
    }

    @Test func leavingContactsAndMessagesTakesTheHelperAway() async throws {
        let notGranted = LatchFailure(code: "needs_setup", message: "no Full Disk Access", grant: "/Applications/Plow Latch.app",
                                      settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient { _ in throw LatchError.failed(notGranted) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { _ in }
        let guide = FakeGuide()
        app.guideGrant = { _ in guide }
        let flow = app.stage(.access)
        flow.requestAccess()
        await waitUntil { flow.messages.isNeeded && flow.contacts.isNeeded }
        let checks = flow.accessTask
        flow.back()
        #expect(guide.calls == ["stop"])
        // The checks end too, and their ending stops it again, never "granted".
        await checks?.value
        #expect(guide.calls == ["stop", "stop"])
    }

    @Test func aRerunShowsTheLoginItemAsItIs() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.loginItemIsOn = { false }
        #expect(app.stage(.start, progress: Progress(rerun: true)).startAtLogin == false)
        #expect(app.stage(.start, progress: Progress()).startAtLogin == true)
        app.loginItemIsOn = { true }
        #expect(app.stage(.start, progress: Progress(rerun: true)).startAtLogin == true)
        #expect(Progress().settingUpAgain().rerun)
    }

    @Test func aLaunchOnAnotherStateFolderLeavesTheLoginItemAlone() async {
        // The login item starts the default state folder only.
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/tmp/dev-state"), still: true,
                           loginItemOffered: false)
        var set: [Bool] = []
        app.setLoginItem = { set.append($0) }
        app.loginItemIsOn = { true }
        let flow = app.stage(.start, progress: Progress(rerun: true))
        #expect(!flow.startAtLogin)
        await flow.startLatch()
        #expect(set.isEmpty)
    }

    @Test func skipWaitsWhileTheRulesSave() async {
        let client = ScriptedClient { _ in nil }
        let save = Gate()
        client.holds["save"] = save
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.draftRules = "Never send email."
        let saving = Task { await flow.saveRules() }
        await waitUntil { flow.savingRules }
        flow.skip()
        #expect(flow.step == .rules)
        save.open()
        await saving.value
        // The save moves on from Set your rules, and Grant access is still to do.
        #expect(flow.step == .access)
        #expect(flow.progress.passed[.rules] == .continued)
        #expect(flow.progress.passed[.access] == nil)
    }

    @Test func notSetUpIsOnlyLatchsAnswer() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.stage(status: try? Fixtures.decode(Fixtures.stopped))
        _ = app.stage(.done, progress: Progress(passed: [.access: .skipped, .rules: .skipped]))
        app.stage(rulesSaved: true, setup: try? Fixtures.decode(Fixtures.setup(on: ["google"])))
        // Skipped this time, but latch has rules and Google, with no host running.
        #expect(Summary.rows(app: app, flow: nil).map(\.state) == ["Logged in", "Saved", "Connected", "Not set up", "Not running"])
    }

    /// Contacts & Messages refused for want of Full Disk Access, on a host
    /// this Mac runs in the background or in a terminal (`attached`), and
    /// checked once: both wait for access until `granted` says so.
    private func accessFlow(attached: Bool, granted: @escaping @Sendable () -> Bool = { false }) async -> (ScriptedClient, AppModel, Onboarding) {
        let notGranted = LatchFailure(code: "needs_setup", message: "no Full Disk Access; restart the host",
                                      grant: "/Applications/Plow Latch.app", settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient { _ in
            if granted() { return nil }
            throw LatchError.failed(notGranted)
        }
        client.status = attached ? Fixtures.servingAttached : Fixtures.serving
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { _ in }
        app.guideGrant = { _ in FakeGuide() }
        await app.refreshStatus()
        let flow = app.stage(.access)
        flow.checkAccess()
        await waitUntil { flow.messages.isNeeded && flow.contacts.isNeeded }
        return (client, app, flow)
    }

    @Test func aBackgroundHostRestartsOnlyWhenAsked() async throws {
        let (client, _, flow) = await accessFlow(attached: false)
        #expect(flow.backgroundHostNeedsRestart)
        #expect(!flow.terminalHostNeedsRestart)
        // Never by itself: only the person's Restart Latch stops and starts it.
        #expect(client.stopped == 0 && client.performed.isEmpty)
        await flow.restartLatch()
        #expect(client.stopped == 1)
        #expect(client.performed == [["serve", "-d", "--again"]])
        await waitUntil { client.enabled.count >= 4 }
        #expect(client.enabled == ["messages", "contacts", "messages", "contacts"])
    }

    /// Granted in System Settings, then both readers on: the Full Disk
    /// Access row reads latch's answer again, whether System Settings was
    /// opened or the readers were only checked.
    @Test(arguments: [true, false])
    func grantingAccessShowsOnItsRow(opened: Bool) async {
        let client = ScriptedClient { _ in nil }
        client.access = "not_granted"
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.access)
        await flow.accessAppeared()
        #expect(flow.fullDiskAccess == "not_granted")
        client.access = "granted"
        if opened { flow.requestAccess() } else { flow.checkAccess() }
        await waitUntil { flow.accessDone && flow.fullDiskAccess == "granted" }
        #expect(flow.fullDiskAccess == "granted")
    }

    @Test func aTerminalHostIsLeftToLatchsWords() async {
        let (client, _, flow) = await accessFlow(attached: true)
        #expect(flow.terminalHostNeedsRestart)
        #expect(!flow.backgroundHostNeedsRestart)
        #expect(client.stopped == 0)
    }

    /// Grant access: while Contacts & Messages aren't on, the main button
    /// works on them; once they are, it is Continue, which never skips
    /// Google: until Google is on too, Skip for now does that.
    @Test func continueNeverSkipsGoogleSilently() async {
        let client = ScriptedClient { _ in nil }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.access)
        #expect(flow.primaryTitle == "Open System Settings")
        flow.messages = .done
        flow.contacts = .done
        for state: Onboarding.Google in [.checking, .notConnected, .waiting, .unreachable, .failed(Onboarding.couldNotCheck)] {
            flow.google = state
            #expect(flow.primaryTitle == "Continue")
            #expect(!flow.primaryEnabled)
            #expect(!flow.stepDone)
            flow.primary()
            #expect(flow.step == .access)
            #expect(flow.progress.passed[.access] == nil)
        }
        // Skipping is its own, visible choice.
        flow.skip()
        #expect(flow.step == .start)
        #expect(flow.progress.passed[.access] == .skipped)

        let connected = app.stage(.access)
        connected.messages = .done
        connected.contacts = .done
        connected.google = .connected
        #expect(connected.stepDone)
        connected.primary()
        #expect(connected.step == .start)
        #expect(connected.progress.passed[.access] == .continued)

        // Contacts & Messages couldn't turn on: Continue still waits for
        // Google, and Skip for now stays; with Google on, Continue moves on
        // as a skip, since access isn't set up.
        let failed = app.stage(.access)
        failed.messages = .failed(LatchError.failed(LatchFailure(code: "messages_not_installed", message: "")))
        failed.contacts = .done
        for state: Onboarding.Google in [.notConnected, .waiting] {
            failed.google = state
            #expect(failed.primaryTitle == "Continue")
            #expect(!failed.primaryEnabled)
            #expect(!failed.primarySkips)
            failed.primary()
            #expect(failed.step == .access)
            #expect(failed.progress.passed[.access] == nil)
        }
        failed.google = .connected
        #expect(failed.primaryEnabled && failed.primarySkips)
        failed.primary()
        #expect(failed.step == .start)
        #expect(failed.progress.passed[.access] == .skipped)
    }

    /// Every screen's Back and Skip for now: Back goes to the screen
    /// before, from Log in on; Skip for now is on Set your rules and Grant
    /// access only, and goes to the screen after.
    @Test func backAndSkipOnEveryScreen() {
        let order: [Step] = [.welcome, .login, .privacy, .rules, .access, .start, .done]
        #expect(Step.allCases == order)
        #expect(order.filter(\.skippable) == [.rules, .access])
        let app = AppModel(client: ScriptedClient { _ in nil }, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        for (index, step) in order.enumerated() {
            let back = app.stage(step)
            back.back()
            #expect(back.step == (index > 0 ? order[index - 1] : step), "Back from \(step)")
            let skip = app.stage(step)
            skip.skip()
            #expect(skip.step == (step.skippable ? order[index + 1] : step), "Skip from \(step)")
        }
    }

    @Test func googleTurnsOnOnceAnAccountIsConnected() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["testuser@plow.test"]) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { Issue.record("opened \($0)") }

        client.google = Fixtures.googleNotConnected
        let notConnected = app.stage(.access)
        await notConnected.checkGoogle()
        #expect(notConnected.google == .notConnected)

        client.google = Fixtures.googleUnreachable
        let unreachable = app.stage(.access)
        await unreachable.checkGoogle()
        #expect(unreachable.google == .unreachable)
        #expect(client.enabled.isEmpty)

        // Connected in Plow, off here: on with no press.
        client.google = Fixtures.googleConnectedOff
        let off = app.stage(.access)
        await off.checkGoogle()
        #expect(off.google == .connected)
        #expect(off.googleAccounts.accounts == ["testuser@plow.test"])
        #expect(client.enabled == ["google"])

        // Already on: nothing to run.
        client.google = Fixtures.googleOn
        let on = app.stage(.access, progress: Progress(rerun: true))
        await on.checkGoogle()
        #expect(on.google == .connected)
        #expect(client.enabled == ["google"])

        // Turned on once, then disconnected in Plow: not connected.
        client.google = #"{"step":"google","state":"needed","reason":"Google is not connected in Plow.","args":null,"enabled":true,"ready":false,"accounts":[],"connect_url":"http://localhost:3000/connections"}"#
        let disconnected = app.stage(.access, progress: Progress(rerun: true))
        await disconnected.checkGoogle()
        #expect(disconnected.google == .notConnected)
        app.stage(rulesSaved: true, setup: try? await client.setupStatus())
        #expect(Summary.rows(app: app, flow: nil).first { $0.title == "Google" }?.state == "Not set up")
    }

    @Test func aCheckStillAnsweringWhenGoogleIsSkippedTurnsNothingOn() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["testuser@plow.test"]) }
        client.google = Fixtures.googleConnectedOff
        let look = Gate()
        client.holds["look 1"] = look
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.access)
        flow.google = .unreachable
        // Try again, then Skip for now before latch answers.
        let check = Task { await flow.checkGoogle() }
        await waitUntil { client.looked == 1 }
        flow.skip()
        look.open()
        await check.value
        #expect(flow.step == .start)
        #expect(client.enabled.isEmpty)
    }

    @Test func leavingGoogleStopsTryAgain() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["testuser@plow.test"]) }
        client.google = Fixtures.googleConnectedOff
        let look = Gate()
        client.holds["look 1"] = look
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.access)
        flow.google = .unreachable
        flow.recheckGoogle()
        await waitUntil { client.looked == 1 }
        let check = flow.googleCheck
        flow.back()
        // latch answers after the screen went.
        look.open()
        await check?.value
        #expect(flow.step == .rules)
        #expect(client.enabled.isEmpty)
        // Elsewhere, a return to the app checks nothing.
        flow.recheckGoogle()
        #expect(client.looked == 1)
    }

    @Test func comingBackChecksAGoogleAlreadyOn() async {
        let client = ScriptedClient { _ in nil }
        client.google = Fixtures.googleOn
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.access)
        await flow.checkGoogle()
        #expect(flow.google == .connected)
        // Disconnected in Plow meanwhile; the app comes back to the front.
        client.google = #"{"step":"google","state":"needed","reason":"Google is not connected in Plow.","args":null,"enabled":true,"ready":false,"accounts":[],"connect_url":"http://localhost:3000/connections"}"#
        flow.recheckGoogle()
        #expect(flow.google == .connected)
        await waitUntil { flow.google == .notConnected }
        #expect(flow.google == .notConnected)
    }

    @Test func googleProblemsAreSaidPlainly() async {
        let answers: [(String, Onboarding.Google)] = [
            ("plow_unreachable", .unreachable),
            ("gog_not_configured", .failed(Onboarding.toolsMissing)),
            ("google_needs_sign_in", .notConnected),
            ("something_new", .failed(Onboarding.couldNotCheck)),
        ]
        for (code, shown) in answers {
            let refusal = LatchFailure(code: code, message: "could not reach Plow at http://127.0.0.1:9; run `latch plugins enable google`")
            let client = ScriptedClient { _ in throw LatchError.failed(refusal) }
            client.google = Fixtures.googleConnectedOff
            let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
            let flow = app.stage(.access)
            await flow.checkGoogle()
            #expect(flow.google == shown)
            if case let .failed(why) = flow.google {
                #expect(!why.contains("latch ") && !why.contains("http") && !why.contains("`"))
            }
        }
    }

    @Test func accessAlreadyGrantedTurnsOnWithoutSystemSettings() async {
        let client = ScriptedClient { _ in nil }
        client.access = "granted"
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { Issue.record("opened \($0)") }
        let flow = app.stage(.access)
        await flow.accessAppeared()
        #expect(flow.primaryTitle == "Turn on")
        #expect(client.enabled.isEmpty)
        flow.primary()
        await waitUntil { flow.accessDone }
        #expect(client.enabled == ["messages", "contacts"])
        #expect(flow.primaryTitle == "Continue")

        client.access = "not_granted"
        let notYet = app.stage(.access)
        await notYet.accessAppeared()
        #expect(notYet.primaryTitle == "Open System Settings")
    }

    @Test func arrivingAtAccessOnlyReadsWhatsOn() async throws {
        let client = ScriptedClient { _ in nil }
        client.connectorsOn = ["messages"]
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { Issue.record("opened \($0)") }
        let flow = app.stage(.access)
        await flow.accessAppeared()
        await flow.accessTask?.value
        #expect(flow.messages == .done)
        #expect(flow.contacts == .unknown)
        // Looking turns nothing on.
        #expect(client.enabled.isEmpty)
    }

    @Test func aBlockedHostNeitherMovesOnNorSetsTheLoginItem() async {
        let client = ScriptedClient { _ in nil }
        client.host = #"{"step":"host","state":"blocked","reason":"a host from another latch version (pid 4242) holds this state folder; stop it with `kill 4242`, then start latch again","args":null}"#
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var loginItem: [Bool] = []
        app.setLoginItem = { loginItem.append($0) }
        let flow = app.stage(.start)
        await flow.startLatch()
        #expect(flow.step == .start)
        #expect(loginItem.isEmpty)
        #expect(client.performed.isEmpty)
        #expect(flow.startError?.message == "Latch can't start on this Mac right now.")
        #expect(flow.startError?.details?.hasPrefix("a host from another latch version (pid 4242)") == true)
    }

    @Test func aTakenNameAsksForAnotherOrReplacesThatMachine() async throws {
        let line: LoginLine = try Fixtures.decode(#"{"type":"error","error":{"code":"conflict","message":"a machine named \"studio\" already exists","existing_name":"studio","last_seen_at":"2026-10-21T04:44:15Z"}}"#)
        let client = ScriptedClient { _ in nil }
        client.loginEvents = [try #require(line.event)]
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.login)
        flow.startLogin()
        let taken = Onboarding.Login.nameTaken(existing: "studio", lastSeen: LatchJSON.parseTimestamp("2026-10-21T04:44:15Z"))
        await waitUntil { flow.login == taken }
        #expect(flow.login == taken)
        client.loginEvents = [.verified(name: "studio-2")]
        flow.loginName = " studio-2 "
        flow.useAnotherName()
        await waitUntil { flow.login == .verified(name: "studio-2") }
        #expect(client.loginNames == [nil, "studio-2"])
        // Replacing is the person's choice, under the name that's taken.
        #expect(client.replaced == [false, false])
        client.loginEvents = [line.event!]
        flow.startLogin()
        await waitUntil { flow.login == taken }
        client.loginEvents = [.verified(name: "studio")]
        flow.replaceMachine()
        await waitUntil { flow.login == .verified(name: "studio") }
        #expect(client.loginNames.last == "studio")
        #expect(client.replaced == [false, false, false, true])
    }

    @Test func aRelaunchWithNoArgumentsKeepsTheStateFolder() throws {
        try withScratchDefaults { defaults in
            #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/Users/me/.latch", true))
            #expect(StateFolder.choose(given: "/tmp/dev", launched: .other, defaults: defaults) == ("/tmp/dev", false))
            // Quit & Reopen: no arguments, the same folder, and no login item.
            #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/tmp/dev", false))
            // Naming the default folder goes back to it for good.
            #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .defaultFolder, defaults: defaults) == ("/Users/me/.latch", true))
            #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/Users/me/.latch", true))
        }
    }

    @Test func aFailedRestartSaysTheHostIsStoppedAndWhyAndRetries() async {
        let (client, app, flow) = await accessFlow(attached: false)
        client.serveFailure = LatchFailure(code: "serve_failed", message: "the host did not answer within 30 s; see serve.log")
        await flow.restartLatch()
        #expect(flow.restartError?.details == "the host did not answer within 30 s; see serve.log")
        #expect(app.status?.thisMachine.hosting.state == "stopped")
        // Try again: the host is stopped already, so it is only started.
        client.serveFailure = nil
        await flow.restartLatch()
        #expect(client.stopped == 1)
        #expect(client.performed == [["serve", "-d", "--again"], ["serve", "-d", "--again"]])
        #expect(flow.restartError == nil)
    }

    @Test func checkAgainAfterATerminalRestartTurnsBothOn() async {
        nonisolated final class Grant: @unchecked Sendable { var on = false }
        let grant = Grant()
        let (client, _, flow) = await accessFlow(attached: true) { grant.on }
        #expect(flow.terminalHostNeedsRestart)
        // The person restarts latch in their terminal, then Check again.
        grant.on = true
        flow.checkAccess()
        await waitUntil { flow.accessDone }
        #expect(flow.accessDone)
        #expect(client.stopped == 0 && client.performed.isEmpty)
    }
}

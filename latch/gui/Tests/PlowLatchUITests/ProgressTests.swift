import Foundation
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
        #expect(skippedRules.destination(loggedIn: true) == .onboarding(.google))
        let allPassed = Progress(passed: [.privacy: .continued, .rules: .continued, .google: .skipped, .access: .skipped, .start: .continued])
        #expect(allPassed.destination(loggedIn: true) == .onboarding(.done))
    }

    @Test func finishedOpensTheMainWindow() {
        #expect(Progress(finished: true).destination(loggedIn: true) == .main)
    }

    @Test func notLoggedInBeatsFinished() {
        let loggedOut = Progress(passed: [.welcome: .continued, .login: .continued], finished: true)
        #expect(loggedOut.destination(loggedIn: false) == .onboarding(.login))
    }

    @Test func setUpAgainWalksEveryScreen() {
        let all = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .continued,
                                    .google: .skipped, .access: .skipped, .start: .continued, .done: .continued], finished: true)
        let again = all.settingUpAgain()
        #expect(again.destination(loggedIn: true) == .onboarding(.welcome))
        #expect(again.passed.isEmpty)
        #expect(!again.finished)
        // Quit midway through: the next launch carries on from there.
        var midway = again
        for step in [Step.welcome, .login, .privacy, .rules] { midway.passed[step] = .continued }
        #expect(midway.destination(loggedIn: true) == .onboarding(.google))
    }

    @Test func progressIsKeptPerStateFolder() {
        let memory = ProgressStore.Memory()
        let home = ProgressStore(memory: memory, stateDir: "/Users/me/.latch")
        let dev = ProgressStore(memory: memory, stateDir: "/Users/me/dev/.dev/state")
        let progress = Progress(passed: [.welcome: .continued, .google: .skipped], finished: true)
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
        #expect(serving.peer?.name == "studio")
        #expect(serving.serve.state == "serving")
        let blocked: LocalStatus = try Fixtures.decode(Fixtures.otherVersion)
        #expect(blocked.serve.blocked?.code == "other_version")
        let loggedOut: LocalStatus = try Fixtures.decode(Fixtures.loggedOut)
        #expect(loggedOut.peer == nil)
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
    /// Waits for `done`, up to two seconds.
    private func until(_ done: () -> Bool) async {
        for _ in 0..<200 where !done() { try? await Task.sleep(for: .milliseconds(10)) }
    }

    @Test func examplesAreJudgedOnTheDraftAsANewAgent() async {
        let client = ScriptedClient { _ in nil }
        client.verdict = { draft, request in draft.contains("Do not send email") && request.contains("send") ? "owner_said_no" : "allow" }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.choose(Onboarding.decks[0])
        #expect(flow.reviewing)
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .checking })
        await until { !flow.reviewing }
        let verdicts = flow.deck.examples.map { if case let .verdict(answer)? = flow.previews[$0.id] { answer.verdict } else { "-" } }
        #expect(verdicts == ["allow", "allow", "allow", "allow", "owner_said_no"])
        #expect(client.tries.allSatisfy { $0.draft == Onboarding.decks[0].rules })
        #expect(client.saved.isEmpty)
    }

    @Test func anEmptyBoxPreviewsTheSavedRulesItKeeps() async {
        let client = ScriptedClient { _ in nil }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.savedRules = "Allow everything."
        flow.draftRules = "  "
        flow.draftChanged()
        await until { !flow.reviewing && !client.tries.isEmpty }
        // Continue keeps the saved rules, so they are what the examples show.
        #expect(flow.primaryTitle == "Continue")
        #expect(!client.tries.isEmpty && client.tries.allSatisfy { $0.draft == "Allow everything." })
    }

    @Test func typingWaitsASecondAndDropsOlderAnswers() async throws {
        let client = ScriptedClient(pause: .milliseconds(300)) { _ in nil }
        client.verdict = { draft, _ in draft == "second" ? "not_covered" : "allow" }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.draftRules = "first"
        flow.draftChanged()
        // Checking at once, but nothing asked before the person pauses.
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .checking })
        try await Task.sleep(for: .milliseconds(1100))
        #expect(client.tries.map(\.draft).allSatisfy { $0 == "first" } && !client.tries.isEmpty)
        // A change while "first" is out: its answers never show.
        flow.draftRules = "second"
        flow.draftChanged()
        await until { client.tries.contains { $0.draft == "second" } }
        await until { !flow.reviewing }
        #expect(flow.deck.examples.allSatisfy { if case let .verdict(answer)? = flow.previews[$0.id] { answer.verdict == "not_covered" } else { false } })
        // The same text again asks nothing new.
        let asked = client.tries.count
        flow.draftRules = " second "
        flow.draftChanged()
        try await Task.sleep(for: .milliseconds(1200))
        #expect(client.tries.count == asked)
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
        _ = app.stage(.done, progress: Progress(passed: [.rules: .continued, .google: .continued, .access: .continued]))
        app.stage(rulesSaved: false, setup: try? Fixtures.decode(Fixtures.setup(on: ["messages"])))
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Logged in", "Not set up", "Not set up", "Not set up", "Running"])
        app.stage(rulesSaved: true, setup: try? Fixtures.decode(Fixtures.setup(on: ["google", "messages", "contacts"])))
        #expect(Summary.rows(app: app, flow: nil).map(\.state) == ["Logged in", "Saved", "Connected", "On", "Running"])
    }

    @Test func aFailedStatusReadSaysSo() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.stage(status: nil, error: "Latch returned an unreadable response.")
        app.stage(rulesSaved: nil, setup: nil)
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check"])
    }

    @Test func onlyMissingAccessIsAccessNeeded() {
        #expect(Onboarding.Check.needed("Waiting for access").isNeeded)
        #expect(!Onboarding.Check.failed("the Messages reader is missing").isNeeded)
    }
}

/// The Full Disk Access helper, as calls made to it.
final class FakeGuide: GrantGuide {
    private(set) var calls: [String] = []
    func granted() { calls.append("granted") }
    func stop() { calls.append("stop") }
}

/// A latch whose `plugins enable` answers as a test says, after a pause,
/// and counts its calls; `rules set` takes the same pause.
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
    let pause: Duration
    private(set) var enabled: [String] = []

    init(pause: Duration = .zero, enable: @escaping @Sendable (String) throws -> GoogleEnabled?) {
        self.pause = pause
        self.enableAnswer = enable
    }

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(status) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func pluginsList(host: String?) async throws -> PluginsList { throw CancellationError() }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { $0.finish() } }
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
        return try Fixtures.decode(host.map { Fixtures.setup(on: connectorsOn, google: google, host: $0) } ?? Fixtures.setup(on: connectorsOn, google: google))
    }
    /// What `--check-access` says of Full Disk Access.
    var access: String?
    func setupStatusCheckingAccess() async throws -> SetupStatus {
        try Fixtures.decode(Fixtures.setup(on: connectorsOn, google: google, access: access))
    }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {
        saved.append(text)
        try await Task.sleep(for: pause)
    }
    private(set) var saved: [String] = []
    /// What `rules try` answers for a draft and request, and what it was asked.
    var verdict: @Sendable (String, [String]) -> String = { _, _ in "allow" }
    /// `rules try` runs several at once.
    private let asked = Mutex<[(draft: String, request: [String])]>([])
    var tries: [(draft: String, request: [String])] { asked.withLock { $0 } }
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        let draft = draft ?? ""
        asked.withLock { $0.append((draft, request)) }
        try await Task.sleep(for: pause)
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
        try await Task.sleep(for: pause)
        return try enableAnswer(plugin)
    }
}

@MainActor
struct ClosingTests {
    private func until(_ done: () -> Bool) async {
        for _ in 0..<200 where !done() { try? await Task.sleep(for: .milliseconds(10)) }
    }

    @Test func cancellingARunStopsLatch() async throws {
        let process = LatchProcess(executable: URL(filePath: "/bin/sleep"), arguments: ["30"])
        let started = ContinuousClock.now
        let run = Task { try await process.output() }
        try await Task.sleep(for: .milliseconds(100))
        run.cancel()
        let result = await run.result
        #expect(throws: CancellationError.self) { try result.get() }
        #expect(ContinuousClock.now - started < .seconds(3))
    }

    @Test func leavingContactsAndMessagesStopsItsChecks() async throws {
        let notGranted = LatchFailure(code: "needs_setup", message: "no Full Disk Access", grant: "/Applications/Plow Latch.app",
                                      settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient(pause: .milliseconds(300)) { _ in throw LatchError.failed(notGranted) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        app.guideGrant = { opened.append($0); return FakeGuide() }
        let flow = app.stage(.access)
        flow.requestAccess()
        try await Task.sleep(for: .milliseconds(50))
        flow.skip()
        try await Task.sleep(for: .milliseconds(700))
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
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let guide = FakeGuide()
        var guided: [URL] = []
        app.guideGrant = { guided.append($0); return guide }
        let flow = app.stage(.access)
        flow.requestAccess()
        try await Task.sleep(for: .milliseconds(200))
        #expect(opened == [URL(string: notGranted.settings!)!])
        #expect(guided == [URL(filePath: "/Applications/Plow Latch.app")])
        #expect(guide.calls.isEmpty)
        try await Task.sleep(for: .milliseconds(2300))
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
        try await Task.sleep(for: .milliseconds(200))
        flow.back()
        #expect(guide.calls.first == "stop")
        try await Task.sleep(for: .milliseconds(100))
        #expect(!guide.calls.contains("granted"))
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
        let client = ScriptedClient(pause: .milliseconds(300)) { _ in nil }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.draftRules = "Never send email."
        let saving = Task { await flow.saveRules() }
        await until { flow.savingRules }
        flow.skip()
        #expect(flow.step == .rules)
        await saving.value
        // The save moves on from Set your rules, and Google is still to do.
        #expect(flow.step == .google)
        #expect(flow.progress.passed[.rules] == .continued)
        #expect(flow.progress.passed[.google] == nil)
    }

    @Test func notSetUpIsOnlyLatchsAnswer() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.stage(status: try? Fixtures.decode(Fixtures.stopped))
        _ = app.stage(.done, progress: Progress(passed: [.google: .skipped, .access: .skipped, .rules: .skipped]))
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
        await until { flow.messages.isNeeded && flow.contacts.isNeeded }
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
        await until { client.enabled.count >= 4 }
        #expect(client.enabled == ["messages", "contacts", "messages", "contacts"])
    }

    @Test func aTerminalHostIsLeftToLatchsWords() async {
        let (client, _, flow) = await accessFlow(attached: true)
        #expect(flow.terminalHostNeedsRestart)
        #expect(!flow.backgroundHostNeedsRestart)
        #expect(client.stopped == 0)
    }

    @Test func continueNeverSkipsGoogleSilently() async {
        let client = ScriptedClient { _ in nil }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.google)
        for state: Onboarding.Google in [.checking, .notConnected, .waiting, .unreachable, .failed(Onboarding.couldNotCheck)] {
            flow.google = state
            #expect(flow.primaryTitle == "Continue")
            #expect(!flow.primaryEnabled)
            #expect(!flow.stepDone)
            flow.primary()
            #expect(flow.step == .google)
            #expect(flow.progress.passed[.google] == nil)
        }
        // Skipping is its own, visible choice.
        flow.skip()
        #expect(flow.step == .access)
        #expect(flow.progress.passed[.google] == .skipped)

        let connected = app.stage(.google)
        connected.google = .connected
        #expect(connected.stepDone)
        connected.primary()
        #expect(connected.step == .access)
        #expect(connected.progress.passed[.google] == .continued)
    }

    @Test func googleTurnsOnOnceAnAccountIsConnected() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["testuser@plow.test"]) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { Issue.record("opened \($0)") }

        client.google = Fixtures.googleNotConnected
        let notConnected = app.stage(.google)
        await notConnected.checkGoogle()
        #expect(notConnected.google == .notConnected)
        #expect(notConnected.googleConnectURL == URL(string: "http://localhost:3000/connections"))

        client.google = Fixtures.googleUnreachable
        let unreachable = app.stage(.google)
        await unreachable.checkGoogle()
        #expect(unreachable.google == .unreachable)
        #expect(client.enabled.isEmpty)

        // Connected in Plow, off here: on with no press.
        client.google = Fixtures.googleConnectedOff
        let off = app.stage(.google)
        await off.checkGoogle()
        #expect(off.google == .connected)
        #expect(off.googleAccounts == ["testuser@plow.test"])
        #expect(client.enabled == ["google"])

        // Already on: nothing to run.
        client.google = Fixtures.googleOn
        let on = app.stage(.google, progress: Progress(rerun: true))
        await on.checkGoogle()
        #expect(on.google == .connected)
        #expect(client.enabled == ["google"])

        // Turned on once, then disconnected in Plow: not connected.
        client.google = #"{"step":"google","state":"needed","reason":"Google is not connected in Plow.","args":null,"enabled":true,"ready":false,"accounts":[],"connect_url":"http://localhost:3000/connections"}"#
        let disconnected = app.stage(.google, progress: Progress(rerun: true))
        await disconnected.checkGoogle()
        #expect(disconnected.google == .notConnected)
        app.stage(rulesSaved: true, setup: try? await client.setupStatus())
        #expect(Summary.rows(app: app, flow: nil).first { $0.title == "Google" }?.state == "Not set up")
    }

    @Test func connectingWaitsForTheAccountThenTurnsGoogleOn() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["testuser@plow.test"]) }
        client.google = Fixtures.googleNotConnected
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let flow = app.stage(.google)
        flow.googleRecheck = .milliseconds(20)
        await flow.checkGoogle()
        flow.connectGoogle()
        #expect(opened == [URL(string: "http://localhost:3000/connections")!])
        #expect(flow.google == .waiting)
        // Signing in takes a few checks; each one only reads.
        let before = client.looked
        await until { client.looked >= before + 3 }
        #expect(flow.google == .waiting)
        #expect(client.enabled.isEmpty)
        client.google = Fixtures.googleConnectedOff
        await until { flow.google == .connected }
        #expect(client.enabled == ["google"])
        #expect(flow.googleAccounts == ["testuser@plow.test"])
        // Once connected, the checks stop.
        let after = client.looked
        try? await Task.sleep(for: .milliseconds(100))
        #expect(client.looked == after)
        #expect(flow.step == .google)
    }

    @Test func leavingGoogleStopsItsChecks() async {
        let client = ScriptedClient { _ in nil }
        client.google = Fixtures.googleNotConnected
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { _ in }
        let flow = app.stage(.google)
        flow.googleRecheck = .milliseconds(20)
        await flow.checkGoogle()
        flow.connectGoogle()
        await until { client.looked >= 3 }
        flow.skip()
        let after = client.looked
        try? await Task.sleep(for: .milliseconds(150))
        #expect(client.looked == after)
        #expect(flow.google == .notConnected)
    }

    @Test func waitingForGoogleEndsAtItsLimit() async {
        let client = ScriptedClient { _ in nil }
        client.google = Fixtures.googleNotConnected
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        app.open = { _ in }
        let flow = app.stage(.google)
        flow.googleRecheck = .milliseconds(20)
        flow.googleWaitLimit = .milliseconds(150)
        await flow.checkGoogle()
        flow.connectGoogle()
        await until { flow.google != .waiting }
        #expect(flow.google == .notConnected)
        #expect(flow.googleWaitEnded)
        #expect(client.enabled.isEmpty)
    }

    @Test func googleProblemsAreSaidPlainly() async {
        let answers: [(String, Onboarding.Google)] = [
            ("plow_unreachable", .unreachable),
            ("gog_not_configured", .failed(Onboarding.toolsMissing)),
            ("something_new", .failed(Onboarding.couldNotCheck)),
        ]
        for (code, shown) in answers {
            let refusal = LatchFailure(code: code, message: "could not reach Plow at http://127.0.0.1:9; run `latch plugins enable google`")
            let client = ScriptedClient { _ in throw LatchError.failed(refusal) }
            client.google = Fixtures.googleConnectedOff
            let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
            let flow = app.stage(.google)
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
        await until { flow.accessDone }
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
        try await Task.sleep(for: .milliseconds(100))
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
        #expect(flow.startError?.hasPrefix("a host from another latch version (pid 4242)") == true)
    }

    @Test func aTakenNameAsksForAnotherOrReplacesThatMachine() async throws {
        let line: LoginLine = try Fixtures.decode(#"{"type":"error","error":{"code":"conflict","message":"a machine named \"studio\" already exists","existing_name":"studio","last_seen_at":"2026-10-21T04:44:15Z"}}"#)
        let client = ScriptedClient { _ in nil }
        client.loginEvents = [try #require(line.event)]
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.login)
        flow.startLogin()
        let taken = Onboarding.Login.nameTaken(existing: "studio", lastSeen: LatchJSON.parseTimestamp("2026-10-21T04:44:15Z"))
        await until { flow.login == taken }
        #expect(flow.login == taken)
        client.loginEvents = [.verified(name: "studio-2")]
        flow.loginName = " studio-2 "
        flow.useAnotherName()
        await until { flow.login == .verified(name: "studio-2") }
        #expect(client.loginNames == [nil, "studio-2"])
        // Replacing is the person's choice, under the name that's taken.
        #expect(client.replaced == [false, false])
        client.loginEvents = [line.event!]
        flow.startLogin()
        await until { flow.login == taken }
        client.loginEvents = [.verified(name: "studio")]
        flow.replaceMachine()
        await until { flow.login == .verified(name: "studio") }
        #expect(client.loginNames.last == "studio")
        #expect(client.replaced == [false, false, false, true])
    }

    @Test func aRelaunchWithNoArgumentsKeepsTheStateFolder() throws {
        let defaults = try #require(UserDefaults(suiteName: "co.plow.latch.tests.\(UUID())"))
        #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/Users/me/.latch", true))
        #expect(StateFolder.choose(given: "/tmp/dev", launched: .other, defaults: defaults) == ("/tmp/dev", false))
        // Quit & Reopen: no arguments, the same folder, and no login item.
        #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/tmp/dev", false))
        // Naming the default folder goes back to it for good.
        #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .defaultFolder, defaults: defaults) == ("/Users/me/.latch", true))
        #expect(StateFolder.choose(given: "/Users/me/.latch", launched: .unnamed, defaults: defaults) == ("/Users/me/.latch", true))
    }

    @Test func aFailedRestartSaysTheHostIsStoppedAndWhyAndRetries() async {
        let (client, app, flow) = await accessFlow(attached: false)
        client.serveFailure = LatchFailure(code: "serve_failed", message: "the host did not answer within 30 s; see serve.log")
        await flow.restartLatch()
        #expect(flow.restartError == "the host did not answer within 30 s; see serve.log")
        #expect(app.status?.serve.state == "stopped")
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
        await until { flow.accessDone }
        #expect(flow.accessDone)
        #expect(client.stopped == 0 && client.performed.isEmpty)
    }
}

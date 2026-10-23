import Foundation
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
        let code: LoginEvent = try Fixtures.decode(Fixtures.code)
        guard case let .code(login) = code else { Issue.record("not a code"); return }
        #expect(login.code == "JFO3S")
        #expect(login.smsto == "SMSTO:+15550000001:JFO3S")
        #expect(login.expiresAt == nil)
        let verified: LoginEvent = try Fixtures.decode(#"{"type":"verified","peer":{"name":"studio","key":"d9438ef1","kind":"device"}}"#)
        #expect(verified == .verified(name: "studio"))
        let error: LoginEvent = try Fixtures.decode(#"{"type":"error","error":{"code":"code_expired","message":"the code expired"}}"#)
        #expect(error == .error(LatchFailure(code: "code_expired", message: "the code expired")))
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

    @Test func examplesWaitForTheNextLatchWithoutVerdicts() async {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.rules)
        flow.choose(Onboarding.decks[1])
        #expect(flow.draftRules == Onboarding.decks[1].rules)
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .notChecked })
        await flow.loadRules()
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
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

    @Test func connectGoogleInPlowOpensWhatLatchNames() {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let flow = app.stage(.google)
        flow.googleConnectURL = URL(string: "https://plow.co/connect/google")
        flow.connectGoogleInPlow()
        #expect(opened == [URL(string: "https://plow.co/connect/google")!])
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
    func login(name: String?) -> AsyncThrowingStream<LoginEvent, Error> {
        loginNames.append(name)
        let events = loginEvents
        return AsyncThrowingStream { continuation in
            for event in events { continuation.yield(event) }
            continuation.finish()
        }
    }
    /// The host step `setup --status` gives, when not the default.
    var host: String?
    func setupStatus() async throws -> SetupStatus {
        try Fixtures.decode(host.map { Fixtures.setup(on: connectorsOn, host: $0) } ?? Fixtures.setup(on: connectorsOn))
    }
    func rulesShow() async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws { try await Task.sleep(for: pause) }
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
        app.reveal = { opened.append($0) }
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

    @Test func connectGoogleInPlowOnlyWithLatchsURL() async {
        let quiet = LatchFailure(code: "google_not_connected", message: "connect a Google account in Plow")
        let client = ScriptedClient { _ in throw LatchError.failed(quiet) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let flow = app.stage(.google)
        // Coming back to the app only reads what latch says: no enable.
        await flow.returnedToApp()
        #expect(client.enabled.isEmpty)
        #expect(flow.google == .unknown)
        await flow.checkGoogle()
        #expect(flow.google == .needed("connect a Google account in Plow"))
        #expect(flow.googleConnectURL == nil)
        flow.connectGoogleInPlow()
        #expect(opened.isEmpty)
        // No page opened, so nothing to come back from.
        await flow.returnedToApp()
        #expect(client.enabled == ["google"])
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
        app.reveal = { _ in }
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

    @Test func setUpAgainShowsGoogleAsItIs() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["owner@example.com"]) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        client.connectorsOn = ["google"]
        let flow = app.stage(.google, progress: Progress(rerun: true))
        await flow.googleAppeared()
        #expect(flow.google == .done)
        #expect(flow.stepDone)
        // Looking never turns it on: a Google that's off waits for the person.
        client.connectorsOn = []
        let first = app.stage(.google)
        await first.googleAppeared()
        #expect(first.google == .unknown)
        #expect(client.enabled.isEmpty)
    }

    @Test func googleNotConnectedContinuesWithoutIt() async {
        let notConnected = LatchFailure(code: "google_not_connected", message: "connect Google in Plow")
        let client = ScriptedClient { _ in throw LatchError.failed(notConnected) }
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.google)
        #expect(flow.primaryTitle == "Check connection")
        flow.primary()
        await until { flow.google.isNeeded }
        // Continue moves on; checking again is the screen's own button.
        #expect(flow.primaryTitle == "Continue")
        #expect(flow.primarySkips)
        flow.primary()
        #expect(flow.step == .access)
        #expect(flow.progress.passed[.google] == .skipped)
        #expect(client.enabled == ["google"])
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

    @Test func aTakenNameAsksForAnother() async {
        let client = ScriptedClient { _ in nil }
        client.loginEvents = [.error(LatchFailure(code: "conflict", message: "a machine named \"studio\" already exists"))]
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.login)
        flow.startLogin()
        await until { flow.login == .nameTaken }
        #expect(flow.login == .nameTaken)
        client.loginEvents = [.verified(name: "studio-2")]
        flow.loginName = " studio-2 "
        flow.useAnotherName()
        await until { flow.login == .verified(name: "studio-2") }
        #expect(client.loginNames == [nil, "studio-2"])
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

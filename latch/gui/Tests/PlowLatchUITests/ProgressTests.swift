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
        let scratch = ScratchDefaults()
        let home = ProgressStore(defaults: scratch.defaults, stateDir: "/Users/me/.latch")
        let dev = ProgressStore(defaults: scratch.defaults, stateDir: "/Users/me/dev/.dev/state")
        let progress = Progress(passed: [.welcome: .continued, .google: .skipped], finished: true)
        home.save(progress)
        #expect(home.load() == progress)
        #expect(dev.load() == Progress())
    }
}

/// A throwaway preferences suite, removed with its file when the test ends,
/// so no test leaves preferences behind.
final class ScratchDefaults {
    let name = "PlowLatchUITests.\(UUID().uuidString)"
    let defaults: UserDefaults

    init() { defaults = UserDefaults(suiteName: name)! }

    deinit {
        UserDefaults.standard.removePersistentDomain(forName: name)
        let file = FileManager.default.homeDirectoryForCurrentUser.appending(path: "Library/Preferences/\(name).plist")
        try? FileManager.default.removeItem(at: file)
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
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
        let flow = app.stage(.rules)
        flow.choose(Onboarding.decks[1])
        #expect(flow.draftRules == Onboarding.decks[1].rules)
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .notChecked })
        await flow.loadRules()
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
    }

    @Test func startAtLoginOnlyWithRunInTheBackground() async {
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped), defaults: scratch.defaults, still: true)
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
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
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
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
        app.stage(status: try? Fixtures.decode(Fixtures.serving))
        // Every screen continued, but latch says nothing is on.
        _ = app.stage(.done, progress: Progress(passed: [.rules: .continued, .google: .continued, .access: .continued]))
        app.stage(rulesSaved: false, plugins: ["google": "disabled", "messages": "enabled", "contacts": "disabled"])
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Logged in", "Not set up", "Not set up", "Not set up", "Running"])
        app.stage(rulesSaved: true, plugins: ["google": "enabled", "messages": "enabled", "contacts": "enabled"])
        #expect(Summary.rows(app: app, flow: nil).map(\.state) == ["Logged in", "Saved", "Connected", "On", "Running"])
    }

    @Test func aFailedStatusReadSaysSo() {
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
        app.stage(status: nil, error: "Latch returned an unreadable response.")
        app.stage(rulesSaved: nil, plugins: nil)
        let states = Summary.rows(app: app, flow: nil).map(\.state)
        #expect(states == ["Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check", "Couldn't check"])
    }

    @Test func onlyMissingAccessIsAccessNeeded() {
        #expect(Onboarding.Check.needed("Waiting for access").isNeeded)
        #expect(!Onboarding.Check.failed("the Messages reader is missing").isNeeded)
    }
}

/// A latch whose `plugins enable` answers as a test says, after a pause,
/// and counts its calls.
final class ScriptedClient: LatchClient, @unchecked Sendable {
    let stateDir = "/Users/me/.latch"
    var status = Fixtures.stopped
    private(set) var stopped = 0
    private(set) var served: [Bool] = []
    let enableAnswer: @Sendable (String) throws -> GoogleEnabled?
    let pause: Duration
    private(set) var enabled: [String] = []

    init(pause: Duration = .zero, enable: @escaping @Sendable (String) throws -> GoogleEnabled?) {
        self.pause = pause
        self.enableAnswer = enable
    }

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(status) }
    func login() -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow() async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func rulesTry(_ request: [String]) async throws -> RulesTry { throw CancellationError() }
    func pluginsList() async throws -> PluginsList { throw CancellationError() }
    func serve(again: Bool) async throws { served.append(again) }
    func stop() async throws { stopped += 1 }
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
        let scratch = ScratchDefaults()
        let app = AppModel(client: client, defaults: scratch.defaults, still: true)
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
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
        app.loginItemIsOn = { false }
        #expect(app.stage(.start, progress: Progress(rerun: true)).startAtLogin == false)
        #expect(app.stage(.start, progress: Progress()).startAtLogin == true)
        app.loginItemIsOn = { true }
        #expect(app.stage(.start, progress: Progress(rerun: true)).startAtLogin == true)
        #expect(Progress().settingUpAgain().rerun)
    }

    @Test func connectGoogleInPlowOnlyWithLatchsURL() async {
        let quiet = LatchFailure(code: "google_not_connected", message: "connect a Google account in Plow")
        let client = ScriptedClient { _ in throw LatchError.failed(quiet) }
        let scratch = ScratchDefaults()
        let app = AppModel(client: client, defaults: scratch.defaults, still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let flow = app.stage(.google)
        // Coming back to the app checks, even before any check.
        await flow.returnedToApp()
        #expect(client.enabled == ["google"])
        #expect(flow.google == .needed("connect a Google account in Plow"))
        #expect(flow.googleConnectURL == nil)
        flow.connectGoogleInPlow()
        #expect(opened.isEmpty)
        await flow.returnedToApp()
        #expect(client.enabled == ["google", "google"])
    }

    @Test func notSetUpIsOnlyLatchsAnswer() {
        let scratch = ScratchDefaults()
        let app = AppModel(client: FakeLatchClient(), defaults: scratch.defaults, still: true)
        app.stage(status: try? Fixtures.decode(Fixtures.stopped))
        _ = app.stage(.done, progress: Progress(passed: [.google: .skipped, .access: .skipped, .rules: .skipped]))
        app.stage(rulesSaved: true, plugins: nil)
        // Skipped this time, but latch has rules; the host isn't running to say about connectors.
        #expect(Summary.rows(app: app, flow: nil).map(\.state) == ["Logged in", "Saved", "Couldn't check", "Couldn't check", "Not running"])
    }

    @Test func aBackgroundHostRestartsOnlyWhenAsked() async throws {
        let notGranted = LatchFailure(code: "needs_setup", message: "this host has no Full Disk Access; turn it on for Plow Latch.app in System Settings, restart the host, then run `latch plugins enable messages` again",
                                      grant: "/Applications/Plow Latch.app", settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient { _ in throw LatchError.failed(notGranted) }
        client.status = #"{"peer":{"name":"studio"},"serve":{"state":"serving","ready":true,"managed":true,"attached":false,"blocked":null},"last_error":null}"#
        let scratch = ScratchDefaults()
        let app = AppModel(client: client, defaults: scratch.defaults, still: true)
        app.open = { _ in }
        app.reveal = { _ in }
        await app.refreshStatus()
        let flow = app.stage(.access)
        flow.checkAccess()
        await until { flow.messages.isNeeded && flow.contacts.isNeeded }
        #expect(flow.backgroundHostNeedsRestart)
        #expect(!flow.terminalHostNeedsRestart)
        // Never by itself: only the person's Restart Latch stops and starts it.
        #expect(client.stopped == 0 && client.served.isEmpty)
        await flow.restartLatch()
        #expect(client.stopped == 1)
        #expect(client.served == [true])
        await until { client.enabled.count >= 4 }
        #expect(client.enabled == ["messages", "contacts", "messages", "contacts"])
    }

    @Test func aTerminalHostIsLeftToLatchsWords() async {
        let notGranted = LatchFailure(code: "needs_setup", message: "this host has no Full Disk Access; restart the host",
                                      grant: "/Applications/Plow Latch.app", settings: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        let client = ScriptedClient { _ in throw LatchError.failed(notGranted) }
        client.status = #"{"peer":{"name":"studio"},"serve":{"state":"serving","ready":true,"managed":true,"attached":true,"blocked":null},"last_error":null}"#
        let scratch = ScratchDefaults()
        let app = AppModel(client: client, defaults: scratch.defaults, still: true)
        await app.refreshStatus()
        let flow = app.stage(.access)
        flow.checkAccess()
        await until { flow.messages.isNeeded && flow.contacts.isNeeded }
        #expect(flow.terminalHostNeedsRestart)
        #expect(!flow.backgroundHostNeedsRestart)
        #expect(flow.accessMessage == "this host has no Full Disk Access; restart the host")
        #expect(client.stopped == 0)
    }

    @Test func setUpAgainShowsGoogleAsItIs() async {
        let client = ScriptedClient { _ in GoogleEnabled(accounts: ["owner@example.com"]) }
        let scratch = ScratchDefaults()
        let app = AppModel(client: client, defaults: scratch.defaults, still: true)
        let flow = app.stage(.google, progress: Progress(rerun: true))
        await flow.googleAppeared()
        #expect(flow.google == .done)
        #expect(flow.googleAccounts == ["owner@example.com"])
        #expect(flow.stepDone)
        // A first run waits for the person.
        let first = app.stage(.google)
        await first.googleAppeared()
        #expect(first.google == .unknown)
    }
}

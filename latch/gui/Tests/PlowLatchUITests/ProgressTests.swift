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

    @Test func setUpAgainWalksEveryScreenAfterLogIn() {
        let all = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .continued,
                                    .google: .skipped, .access: .skipped, .start: .continued, .done: .continued], finished: true)
        let again = all.settingUpAgain()
        #expect(again.destination(loggedIn: true) == .onboarding(.privacy))
        #expect(again.passed == [.welcome: .continued, .login: .continued])
        #expect(!again.finished)
        // Quit midway through: the next launch carries on from there.
        var midway = again
        midway.passed[.privacy] = .continued
        midway.passed[.rules] = .continued
        #expect(midway.destination(loggedIn: true) == .onboarding(.google))
    }

    @Test func progressIsSavedAndLoaded() throws {
        let defaults = try #require(UserDefaults(suiteName: "PlowLatchUITests.\(UUID())"))
        let progress = Progress(passed: [.welcome: .continued, .google: .skipped], finished: true)
        progress.save(defaults)
        #expect(Progress.load(defaults) == progress)
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
        let app = AppModel(client: FakeLatchClient(), still: true)
        let flow = app.stage(.rules)
        flow.choose(Onboarding.decks[1])
        #expect(flow.draftRules == Onboarding.decks[1].rules)
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
        #expect(flow.deck.examples.allSatisfy { flow.previews[$0.id] == .notChecked })
        await flow.loadRules()
        #expect(flow.previewNote == "Previews arrive with the next Latch update.")
    }

    @Test func startAtLoginOnlyWithRunInTheBackground() async {
        let app = AppModel(client: FakeLatchClient(status: Fixtures.stopped), still: true)
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

    @Test func connectGoogleInPlowOpensWhatLatchNamesOrPlow() {
        let app = AppModel(client: FakeLatchClient(), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        let flow = app.stage(.google)
        flow.connectGoogleInPlow()
        flow.googleConnectURL = URL(string: "https://plow.co/connect/google")
        flow.connectGoogleInPlow()
        #expect(opened == [URL(string: "https://plow.co")!, URL(string: "https://plow.co/connect/google")!])
    }

    @Test func googleNotConnectedCarriesLatchsConnectURL() throws {
        struct Line: Decodable { var error: LatchFailure }
        let json = #"{"error":{"code":"google_not_connected","message":"connect Google in Plow","connect_url":"https://plow.co/connect/google"}}"#
        let line = try LatchJSON.decoder.decode(Line.self, from: Data(json.utf8))
        #expect(line.error.connectUrl == URL(string: "https://plow.co/connect/google"))
    }

    @Test func setupRowsComeFromLatchNotFromScreensPassed() {
        let app = AppModel(client: FakeLatchClient(), still: true)
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
        let app = AppModel(client: FakeLatchClient(), still: true)
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

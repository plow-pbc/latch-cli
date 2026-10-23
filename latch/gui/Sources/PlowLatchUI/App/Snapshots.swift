#if DEBUG
import AppKit
import SwiftUI

/// A latch that answers from JSON latch really prints (`latch/APP.md` and
/// captured runs), for snapshots and tests. It never runs anything.
nonisolated struct FakeLatchClient: LatchClient {
    var stateDir = "/Users/me/.latch"
    var status = Fixtures.serving
    var rules: String? = nil

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(status) }
    func login(name: String?) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow() async throws -> RulesShow {
        try Fixtures.decode(rules.map { #"{"agents":[],"every_agent":{"revision":"r1","text":"\#($0)"},"host":"studio"}"# }
            ?? #"{"agents":[],"every_agent":null,"host":"studio"}"#)
    }
    func rulesSet(_ text: String) async throws {}
    var connectorsOn: [String] = []
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: connectorsOn)) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

nonisolated enum Fixtures {
    /// `latch setup --status` on a logged-in Mac whose host is stopped,
    /// with these connectors on.
    static func setup(on: [String], host: String = #"{"step":"host","state":"needed","reason":"latch is not running.","args":["serve","-d","--again"]}"#) -> String {
        let connectors = ["google", "messages", "contacts"].map { name in
            on.contains(name)
                ? #"{"step":"\#(name)","state":"done","reason":"The \#(name.capitalized) connector is on; agents can use it once latch is running.","args":null,"enabled":true,"ready":false}"#
                : #"{"step":"\#(name)","state":"needed","reason":"The \#(name.capitalized) connector is off.","args":["plugins","enable","\#(name)"],"enabled":false,"ready":false}"#
        }
        return #"{"steps":[{"step":"login","state":"done","reason":"Logged in as studio.","args":null},"#
            + connectors.joined(separator: ",")
            + "," + host + "]}"
    }

    static let serving = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":32250,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let stopped = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":true,"build":"1342","checked_at":"2026-10-22T22:07:54.186472Z"}}"#
    static let loggedOut = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":null,"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let servingAttached = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":true,"state":"serving","pid":32250,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let otherVersion = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":false,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":4242,"blocked":{"code":"other_version","message":"a host from another latch version (pid 4242) holds this state folder; stop it with `kill 4242`, then start latch again"}},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let code = #"{"type":"code","code":"JFO3S","send_to":"+15550000001","send_to_display":"+1 555-000-0001","smsto":"SMSTO:+15550000001:JFO3S","sms_link":"sms:+15550000001?&body=JFO3S","expires_at":null}"#
    static let allow = #"{"verdict":"allow","host":"studio","as":"owner","request":{"op":"exec","action":{}},"gate":null}"#
    static let notCovered = #"{"verdict":"not_covered","host":"studio","as":"owner","request":{"op":"google","action":{}},"gate":null}"#
    static let saidNo = #"{"verdict":"owner_said_no","host":"studio","as":"owner","request":{"op":"google","action":{}},"gate":null}"#
    static let hostRefused = #"{"verdict":"host_refused","host":"studio","as":"owner","request":{"op":"messages","action":{}},"gate":null,"refused":{"code":"plugin_disabled","message":"the messages connector is off on this host"}}"#

    static func decode<T: Decodable>(_ json: String) throws -> T {
        try LatchJSON.decoder.decode(T.self, from: Data(json.utf8))
    }
}

/// Renders every onboarding screen and state, and the main window, in
/// light and dark, on the fake latch: `swift run Snapshots OUTPUT_DIR`.
/// Each view draws its own layers, so no screen-recording permission.
package enum SnapshotRunner {
    package static func run(into output: URL) async throws {
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let code: LoginCode = try Fixtures.decode(Fixtures.code)
        let rules = "Agents may read and summarize anything in ~/Projects. Ask me before sending any email or deleting files. Never touch ~/Finance."
        let skipped = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .skipped,
                                        .google: .skipped, .access: .skipped, .start: .continued])
        let full = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .continued,
                                     .google: .continued, .access: .continued, .start: .continued])

        typealias Stage = (Onboarding) -> Void
        let states: [(String, Step, String, Progress, Stage)] = [
            ("01-welcome", .welcome, Fixtures.loggedOut, Progress(), { _ in }),
            ("02-login-starting", .login, Fixtures.loggedOut, Progress(), { $0.login = .starting }),
            ("02-login-waiting", .login, Fixtures.loggedOut, Progress(), { $0.login = .waiting(code) }),
            ("02-login-expired", .login, Fixtures.loggedOut, Progress(), { $0.login = .expired(code) }),
            ("02-login-error", .login, Fixtures.loggedOut, Progress(), {
                $0.login = .failed("Plow couldn't be reached at http://127.0.0.1:18934; check your connection, then try again")
            }),
            ("02-login-name-taken", .login, Fixtures.loggedOut, Progress(), { $0.login = .nameTaken; $0.loginName = "studio-2" }),
            ("02-login-verified", .login, Fixtures.serving, Progress(), { $0.login = .verified(name: "studio") }),
            ("03-privacy", .privacy, Fixtures.stopped, Progress(), { _ in }),
            ("04-rules-first-run", .rules, Fixtures.stopped, Progress(), {
                $0.previewNote = "Previews arrive with the next Latch update."
            }),
            ("04-rules-rerun-prefilled", .rules, Fixtures.stopped, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules; $0.previewNote = "Previews arrive with the next Latch update."
            }),
            ("04-rules-analyzing-after-1b", .rules, Fixtures.serving, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules; $0.reviewing = true
                for example in $0.deck.examples { $0.previews[example.id] = .checking }
            }),
            ("04-rules-verdicts-after-1b-fake-latch", .rules, Fixtures.serving, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules
                let answers = [Fixtures.allow, Fixtures.allow, Fixtures.allow, Fixtures.hostRefused, Fixtures.saidNo]
                for (example, answer) in zip($0.deck.examples, answers) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.macLit = true
            }),
            ("04-rules-executive-open-after-1b-fake-latch", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[1]
                $0.deck = deck; $0.savedRules = deck.rules; $0.draftRules = deck.rules
                let answers = [Fixtures.allow, Fixtures.allow, Fixtures.allow, Fixtures.notCovered, Fixtures.saidNo]
                for (example, answer) in zip(deck.examples, answers) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.openExamples = [deck.examples[4].id]
            }),
            ("04-rules-save-failed", .rules, Fixtures.stopped, Progress(), {
                $0.draftRules = rules; $0.previewNote = "Previews arrive with the next Latch update."
                $0.rulesError = "rules are at most 8 KB together; shorten them, then save again"
            }),
            ("05-google-not-checked", .google, Fixtures.stopped, Progress(), { $0.google = .unknown }),
            ("05-google-not-connected", .google, Fixtures.stopped, Progress(), {
                $0.google = .needed("Plow has no Google account this host can use; connect one in Plow, then run `latch plugins enable google` again")
            }),
            ("05-google-not-connected-with-link", .google, Fixtures.stopped, Progress(), {
                $0.google = .needed("connect a Google account in Plow"); $0.googleConnectURL = URL(string: "https://plow.co/connect/google")
            }),
            ("05-google-connected", .google, Fixtures.stopped, Progress(), {
                $0.google = .done; $0.googleAccounts = ["owner@example.com"]
            }),
            ("05-google-rerun-connected", .google, Fixtures.serving, Progress(rerun: true), {
                $0.google = .done; $0.googleAccounts = ["owner@example.com", "work@example.com"]
            }),
            ("05-google-error", .google, Fixtures.stopped, Progress(), {
                $0.google = .failed("Plow couldn't be reached at http://127.0.0.1:18934; check your connection, then try again")
            }),
            ("06-access-not-checked", .access, Fixtures.stopped, Progress(), { _ in }),
            ("06-access-waiting", .access, Fixtures.stopped, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access"); $0.waitingForAccess = true
            }),
            ("06-access-allowed", .access, Fixtures.stopped, Progress(), {
                $0.messages = .done; $0.contacts = .done
            }),
            ("06-access-restart-latch", .access, Fixtures.serving, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
            }),
            ("06-access-restart-failed", .access, Fixtures.stopped, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
                $0.restartError = "the host did not answer within 30 s; see /Users/me/.latch/serve.log"
            }),
            ("06-access-terminal-host", .access, Fixtures.servingAttached, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
            }),
            ("06-access-unavailable", .access, Fixtures.stopped, Progress(), {
                $0.messages = .failed("messages cannot be checked on this host: the Messages reader is missing; run install.sh again")
                $0.contacts = .done
            }),
            ("07-start", .start, Fixtures.stopped, Progress(), { _ in }),
            ("07-start-background-off", .start, Fixtures.stopped, Progress(), { $0.runInBackground = false }),
            ("07-start-already-running", .start, Fixtures.serving, Progress(), { _ in }),
            ("07-start-other-version", .start, Fixtures.otherVersion, Progress(), { _ in }),
            ("07-start-error", .start, Fixtures.stopped, Progress(), {
                $0.startError = "the host did not answer within 30 s; see /Users/me/.latch/serve.log"
            }),
            ("08-done-all-set", .done, Fixtures.serving, full, {
                $0.app.stage(rulesSaved: true, setup: try! Fixtures.decode(Fixtures.setup(on: ["google", "messages", "contacts"])))
            }),
            ("08-done-with-skips", .done, Fixtures.serving, skipped, {
                $0.app.stage(rulesSaved: false, setup: try! Fixtures.decode(Fixtures.setup(on: [])))
            }),
            ("08-done-not-running", .done, Fixtures.stopped, full, {
                $0.app.stage(rulesSaved: true, setup: try! Fixtures.decode(Fixtures.setup(on: ["google"])))
            }),
        ]

        for dark in [false, true] {
            for (name, step, status, progress, stage) in states {
                let app = AppModel(client: FakeLatchClient(status: status), still: true)
                app.stage(status: try Fixtures.decode(status))
                let flow = app.stage(step, progress: progress)
                stage(flow)
                try await render(OnboardingView(entranceTime: step == .welcome ? 10 : nil).environment(app),
                                 size: Brand.windowSize, dark: dark, file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            let mains: [(String, String?, Progress, Bool?, [String]?)] = [
                ("09-main-running", Fixtures.serving, full, true, ["google", "messages", "contacts"]),
                ("09-main-stopped-skipped-update", Fixtures.stopped, skipped, false, nil),
                ("09-main-logged-out", Fixtures.loggedOut, full, nil, nil),
                ("09-main-status-failed", nil, full, nil, nil),
            ]
            for (name, status, progress, rulesSaved, on) in mains {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try status.map { try Fixtures.decode($0) },
                          error: status == nil ? "Latch couldn't start: The file “latch” doesn't exist." : nil)
                app.stage(rulesSaved: rulesSaved, setup: try on.map { try Fixtures.decode(Fixtures.setup(on: $0)) })
                _ = app.stage(.done, progress: progress)
                try await render(MainView().environment(app), size: CGSize(width: 520, height: 420), dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // The welcome entrance, mid-way.
        let app = AppModel(client: FakeLatchClient(), still: true)
        _ = app.stage(.welcome)
        try await render(OnboardingView(entranceTime: 1.0).environment(app), size: Brand.windowSize, dark: true,
                         file: output.appending(path: "01-welcome-entrance-1.0s-dark.png"))
    }

    private static func render(_ view: some View, size: CGSize, dark: Bool, file: URL) async throws {
        let host = NSHostingView(rootView: view)
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.titled, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentView = host
        window.alphaValue = 0.01
        window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(400))
        try Render.png(host, to: file)
        window.orderOut(nil)
        print(file.path)
    }
}
#endif

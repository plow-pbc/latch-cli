#if DEBUG
import AppKit
import SwiftUI

/// A latch that answers from JSON latch really prints (`latch/APP.md` and
/// captured runs), for snapshots and tests. It never runs anything.
nonisolated struct FakeLatchClient: LatchClient {
    var stateDir = "/Users/me/.latch"
    var status = Fixtures.serving
    var rules: String? = nil
    var account = Fixtures.studioHosts

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(status) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(account) }
    func login() -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow(host: String?) async throws -> RulesShow {
        try Fixtures.decode(rules.map { #"{"agents":[],"every_agent":{"revision":"r1","text":"\#($0)"},"host":"studio"}"# }
            ?? #"{"agents":[],"every_agent":null,"host":"studio"}"#)
    }
    func rulesSet(_ text: String) async throws {}
    func pluginsList(host: String?) async throws -> PluginsList {
        throw LatchError.failed(LatchFailure(code: "serve_required", message: "this machine needs a live receiver; start `latch serve` on this state folder, then retry"))
    }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func serve(again: Bool) async throws {}
    func stop() async throws {}
}

nonisolated enum Fixtures {
    static let serving = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":32250,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let stopped = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":true,"build":"1342","checked_at":"2026-10-22T22:07:54.186472Z"}}"#
    static let loggedOut = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":null,"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let servingAttached = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":true,"state":"serving","pid":32250,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let otherVersion = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":true,"ready":false,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":4242,"blocked":{"code":"other_version","message":"a host from another latch version (pid 4242) holds this state folder; stop it with `kill 4242`, then start latch again"}},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    static let laptop = #"{"state_dir":"/Users/me/.latch","version":"0.1.0 (commit 6031e5a50, built 2026-10-22 22:03 UTC, local build, protocol plow/latch/2)","protocol":2,"peer":{"name":"laptop","key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","kind":"device"},"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":false,"build":null,"checked_at":null}}"#
    // `latch status` on studio (a host) and on laptop (never served), and
    // laptop's once pi-kitchen had stopped.
    static let studioHosts = #"{"peer":{"key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","kind":"device","name":"studio","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:26.724881Z","online":true},"api_url":"http://127.0.0.1:18934","hosts":[{"key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","kind":"device","name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:30.037004Z","online":true},{"key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","kind":"device","name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:33.384336Z","online":true},{"key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","kind":"device","name":"studio","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:26.724881Z","online":true,"self":true}]}"#
    static let laptopHosts = #"{"peer":{"key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","kind":"device","name":"laptop","relay_url":null,"last_seen_at":"2026-10-22T22:05:50.582906Z","online":true},"api_url":"http://127.0.0.1:18934","hosts":[{"key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","kind":"device","name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:30.037004Z","online":true},{"key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","kind":"device","name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:33.384336Z","online":true},{"key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","kind":"device","name":"studio","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:26.724881Z","online":true}]}"#
    static let laptopHostsPiOffline = #"{"peer":{"key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","kind":"device","name":"laptop","relay_url":null,"last_seen_at":"2026-10-22T22:05:50.582906Z","online":true},"api_url":"http://127.0.0.1:18934","hosts":[{"key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","kind":"device","name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":null,"online":false},{"key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","kind":"device","name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:33.384336Z","online":true},{"key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","kind":"device","name":"studio","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:26.724881Z","online":true}]}"#
    static let noHosts = #"{"peer":{"key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","kind":"device","name":"laptop","relay_url":null,"last_seen_at":"2026-10-22T22:05:50.582906Z","online":true},"api_url":"http://127.0.0.1:18934","hosts":[]}"#
    // `rules show`, `vault list` and `plugins list` on studio, and on
    // pi-kitchen (rules and connectors read from laptop with `--host`).
    static let studioRules = #"{"agents":[{"agent_name":"scout","agent_uid":"0432eb0ee1e99e24c40c54fc2bada229","note":"no latch registered when set","revision":"8be9ed94de19f40ae8a8f06b98bb6ee54da22900b8191b70dd2f2486edf1a1f0","text":"scout may run builds and tests in ~/Projects/latch-demo with --write there, and may use the network for package downloads."}],"audit_id":"81c7dce2aa3758db17a746b2284596fcfb4e2773a99d34cc49cee9e6071b2e3f","every_agent":{"revision":"9b1e8ee3911c42472937f948983a24e6c087c4f70d5bc5b924b84c799f0bff05","text":"Agents may read and summarize anything in ~/Projects. Ask me before sending any email or deleting files. Never touch ~/Finance."},"host":"studio","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    static let studioVault = #"{"audit_id":"bcaf28dedfe777a60303c3ccd802a8a2380237b6f02988bc33dabb307431e0a6","items":[{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v1","kind":"login","revision":1,"sites":["shop.example"],"title":"Shop (test)","totp":false,"username":"ada"},{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v2","kind":"login","other_top_sites":["grocer-login.example"],"revision":1,"sites":["grocer.example"],"title":"Grocer (test)","totp":false,"username":"ada@example.com"},{"fields":[],"item":"v3","kind":"note","revision":1,"sites":[],"title":"Building notes (test)","totp":false},{"fields":[{"alias":false,"custom":false,"hidden":true,"label":"number"},{"alias":false,"custom":false,"hidden":true,"label":"code"},{"alias":false,"custom":false,"hidden":false,"label":"cardholder name"}],"item":"v4","kind":"card","revision":1,"sites":["shop.example"],"title":"Visa (test)","totp":false}]}"#
    static let studioPlugins = #"{"audit_id":"4bb10df4087e3d779901b77fecce312db061776f1f5fffa1716f6d193bb298cd","plugins":[{"enable":"latch plugins enable google","name":"google","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable messages","name":"messages","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable contacts","name":"contacts","reason":"its owner has not enabled it on this host","state":"disabled"}]}"#
    static let piRules = #"{"agents":[],"audit_id":"c57f1209db34c3767c262960f596b34ec61e8b932a2b3011b3677db297f74bea","every_agent":{"revision":"8419fd077d6e4e3adc59789beedadb9864635746da5e22ae63b727c9b42c8715","text":"This Pi runs the kitchen display. Agents may read ~/recipes and write only to ~/recipes/drafts."},"host":"pi-kitchen","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    static let piPlugins = #"{"audit_id":"9727a671710eb349e52bc36fc65f7958273c76056619f26705bee57f164b7a5e","plugins":[{"enable":"latch plugins enable google","name":"google","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable messages","name":"messages","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable contacts","name":"contacts","reason":"its owner has not enabled it on this host","state":"disabled"}]}"#
    static let piVault = #"{"audit_id":"1e059a4e60011ac425fc08b23c8c43e6de7a4db7a571fe8fcce7881388c9f00e","items":[]}"#
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
                $0.app.stage(rulesSaved: true, plugins: ["google": "enabled", "messages": "enabled", "contacts": "enabled"])
            }),
            ("08-done-with-skips", .done, Fixtures.serving, skipped, {
                $0.app.stage(rulesSaved: false, plugins: ["google": "disabled", "messages": "disabled", "contacts": "disabled"])
            }),
            ("08-done-not-running", .done, Fixtures.stopped, full, {
                $0.app.stage(rulesSaved: true, plugins: nil); $0.google = .done
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
            let mains: [(String, String?, Progress, Bool?, [String: String]?)] = [
                ("09-main-running", Fixtures.serving, full, true, ["google": "enabled", "messages": "enabled", "contacts": "enabled"]),
                ("09-main-stopped-skipped-update", Fixtures.stopped, skipped, false, nil),
                ("09-main-logged-out", Fixtures.loggedOut, full, nil, nil),
                ("09-main-status-failed", nil, full, nil, nil),
            ]
            for (name, status, progress, rulesSaved, plugins) in mains {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try status.map { try Fixtures.decode($0) },
                          error: status == nil ? "Latch couldn't start: The file “latch” doesn't exist." : nil)
                app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
                app.stage(rulesSaved: rulesSaved, plugins: plugins)
                _ = app.stage(.done, progress: progress)
                try await renderWindow(MainView().environment(app), size: mainSize, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // The main window's host tier: each kind of host, and switching.
            let plowDown = "Plow couldn't be reached at http://127.0.0.1:18934; check your connection, then try again"
            let homes: [(String, String, String?, String?, SidebarItem)] = [
                ("10-home-this-mac-serving", Fixtures.serving, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-terminal", Fixtures.servingAttached, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-stopped-update", Fixtures.stopped, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-other-version", Fixtures.otherVersion, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-plow-unreachable", Fixtures.serving, nil, nil, .home),
                ("11-home-viewer-only", Fixtures.laptop, Fixtures.laptopHosts, nil, .home),
                ("11-home-viewer-only-no-hosts", Fixtures.laptop, Fixtures.noHosts, nil, .home),
                ("12-home-remote-online", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", .home),
                ("12-home-remote-offline-viewer", Fixtures.laptop, Fixtures.laptopHostsPiOffline, "pi-kitchen", .home),
                ("13-agents-on-studio", Fixtures.serving, Fixtures.studioHosts, nil, .agents),
                ("13-agents-after-switch-to-pi-kitchen", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", .agents),
                ("13-rules-on-pi-kitchen", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", .rules),
            ]
            for (name, status, account, chosen, section) in homes {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(status))
                app.stage(account: try account.map { try Fixtures.decode($0) }, error: account == nil ? plowDown : nil)
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.studioRules)),
                                               vault: .success(try Fixtures.decode(Fixtures.studioVault)),
                                               plugins: .success(try Fixtures.decode(Fixtures.studioPlugins))),
                          for: "studio")
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.piRules)),
                                               vault: .success(try Fixtures.decode(Fixtures.piVault)),
                                               plugins: .success(try Fixtures.decode(Fixtures.piPlugins))),
                          for: "pi-kitchen")
                app.chosenHost = chosen
                app.section = section
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: mainSize, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // The welcome entrance, mid-way.
        let app = AppModel(client: FakeLatchClient(), still: true)
        _ = app.stage(.welcome)
        try await render(OnboardingView(entranceTime: 1.0).environment(app), size: Brand.windowSize, dark: true,
                         file: output.appending(path: "01-welcome-entrance-1.0s-dark.png"))
    }

    private static let mainSize = CGSize(width: 860, height: 560)

    /// The main window as the window server shows it (sidebar materials and
    /// toolbar included), drawn off screen.
    private static func renderWindow(_ view: some View, size: CGSize, dark: Bool, file: URL) async throws {
        let window = NSWindow(contentRect: CGRect(origin: CGPoint(x: -4000, y: -4000), size: size),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentViewController = NSHostingController(rootView: view)
        window.setFrame(CGRect(origin: CGPoint(x: -4000, y: -4000), size: size), display: true)
        window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(600))
        try Render.window(window, to: file)
        window.orderOut(nil)
        print(file.path)
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

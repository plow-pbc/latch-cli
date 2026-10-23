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
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow(host: String?) async throws -> RulesShow {
        try Fixtures.decode(rules.map { #"{"agents":[],"every_agent":{"revision":"r1","text":"\#($0)"},"host":"studio"}"# }
            ?? #"{"agents":[],"every_agent":null,"host":"studio"}"#)
    }
    func rulesSet(_ text: String) async throws {}
    var connectorsOn: [String] = []
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: connectorsOn)) }
    func pluginsList(host: String?) async throws -> PluginsList {
        throw LatchError.failed(LatchFailure(code: "serve_required", message: "this machine needs a live receiver; start `latch serve` on this state folder, then retry"))
    }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesTry(draft: String, request: [String]) async throws -> RulesTry { try Fixtures.decode(Fixtures.allow) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> {
        AsyncThrowingStream { continuation in
            if let status = try? Fixtures.decode(status) as LocalStatus { continuation.yield(status) }
        }
    }
    func audit(host: String?, before: String?) async throws -> AuditPage {
        try AuditPage.parse(Data((host == nil ? Fixtures.studioAudit : Fixtures.kitchenAudit).utf8))
    }
    /// The fixture's lines, then nothing more until stopped.
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> {
        AsyncThrowingStream { continuation in
            for line in (host == nil ? Fixtures.studioAudit : Fixtures.kitchenAudit).split(separator: "\n") {
                if let line = try? LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)) { continuation.yield(line) }
            }
        }
    }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

nonisolated enum Fixtures {
    /// `latch setup --status` on a logged-in Mac whose host is stopped,
    /// with these connectors on.
    static func setup(on: [String], google: String? = nil, access: String? = nil,
                      host: String = #"{"step":"host","state":"needed","reason":"latch is not running.","args":["serve","-d","--again"]}"#) -> String {
        let connectors = ["google", "messages", "contacts"].map { name in
            if name == "google", let google { return google }
            let checked = name != "google" ? access.map { #","access":"\#($0)""# } ?? "" : ""
            return on.contains(name)
                ? #"{"step":"\#(name)","state":"done","reason":"The \#(name.capitalized) connector is on; agents can use it once latch is running.","args":null,"enabled":true,"ready":false\#(checked)}"#
                : #"{"step":"\#(name)","state":"needed","reason":"The \#(name.capitalized) connector is off.","args":["plugins","enable","\#(name)"],"enabled":false,"ready":false\#(checked)}"#
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
    // `latch audit` on studio and, from laptop, `latch audit --host pi-kitchen`:
    // real runs, with names and home folders shortened.
    static let studioAudit = #"""
{"schema":1,"type":"hello"}
{"cursor":"0000000000000000000000000000000000000000000000000000000000000000","has_older":false,"type":"snapshot"}
{"cursor":"c30c5f19c96e7339175054f21735bff913330ad55e3a59820062ea3031e5ce86","record":{"agent":null,"caller":"6b55a78f","caller_name":"studio","decision":"allow","details":{"args":{"after":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c","agent_uid":null,"before":null,"command":"set","scope":"every agent"},"gate":null,"started":null},"id":"c30c5f19c96e7339175054f21735bff913330ad55e3a59820062ea3031e5ce86","op":"rules","outcome":"ok","time":"2026-10-23T04:57:17.443592Z"},"type":"record"}
{"cursor":"c979a3eeb29cc2d8a2b265b030b35e2d1b87455b2fcf098c0345c8618f6944dc","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"c979a3eeb29cc2d8a2b265b030b35e2d1b87455b2fcf098c0345c8618f6944dc","op":"connect","outcome":"connected","time":"2026-10-23T04:57:25.011137Z"},"type":"record"}
{"cursor":"6340de838b0f5a8037872c6c931e34bffb3925247f970736b518b7b6546ef6db","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":24,"exit_code":1,"network":false,"secret_env":[],"signal":null,"stderr_bytes":40,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":313,"model":"typesafe/jev-1.14-20261014","model_request":"req_clvmzkwubekyhohm","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:25.059946Z"},"id":"6340de838b0f5a8037872c6c931e34bffb3925247f970736b518b7b6546ef6db","op":"exec","outcome":"ok","time":"2026-10-23T04:57:25.406595Z"},"type":"record"}
{"cursor":"bc5c3f4e741ea9dbc17f448c97957a9b73d7bc60df9dfcbb688ee75a629badc3","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"bc5c3f4e741ea9dbc17f448c97957a9b73d7bc60df9dfcbb688ee75a629badc3","op":"connect","outcome":"connected","time":"2026-10-23T04:57:25.82139Z"},"type":"record"}
{"cursor":"26ce20c4e91c5ecac785db00f672f07e6e631b547677cae5c382ba9820cbe54c","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":{"argv":["rm","-rf","Finance"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":278,"model":"typesafe/jev-1.14-20261014","model_request":"req_bexwvhrmekgdazuq","p":{"COVERED":0.18,"FORBIDDEN":0.82,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"owner_said_no"},"started":"2026-10-23T04:57:25.83365Z"},"id":"26ce20c4e91c5ecac785db00f672f07e6e631b547677cae5c382ba9820cbe54c","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:57:26.112307Z"},"type":"record"}
{"cursor":"4e23cb5b48d536704e495b9d29d4cb9b162330ec60dbfaf236a31667bd9be033","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"4e23cb5b48d536704e495b9d29d4cb9b162330ec60dbfaf236a31667bd9be033","op":"connect","outcome":"connected","time":"2026-10-23T04:57:26.50193Z"},"type":"record"}
{"cursor":"7f7c8b8278358cab8fc81abc4d14ae90d8c9cefa599574d411a65184f4b3b59f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["curl","https://example.com"],"duration_ms":45,"exit_code":6,"network":false,"secret_env":[],"signal":null,"stderr_bytes":283,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":164,"model":"typesafe/jev-1.14-20261014","model_request":"req_pcprjkoskeurcceu","p":{"COVERED":0.87,"FORBIDDEN":0.09,"NOT_COVERED":0.04},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:26.533038Z"},"id":"7f7c8b8278358cab8fc81abc4d14ae90d8c9cefa599574d411a65184f4b3b59f","op":"exec","outcome":"ok","time":"2026-10-23T04:57:26.743828Z"},"type":"record"}
{"cursor":"d44d2a3920ab63c74ae9f70dc9b00491556992ef712ff66239a57cacdfa7cf6f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"d44d2a3920ab63c74ae9f70dc9b00491556992ef712ff66239a57cacdfa7cf6f","op":"connect","outcome":"connected","time":"2026-10-23T04:57:51.903676Z"},"type":"record"}
{"cursor":"bdc143c43361be60435e26ebf7b9eca7c5774335a27152279f049d39aa5b24f8","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":18,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":303,"model":"typesafe/jev-1.14-20261014","model_request":"req_mhkhqxhakqjfnmqr","p":{"COVERED":0.91,"FORBIDDEN":0.07,"NOT_COVERED":0.02},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:51.956162Z"},"id":"bdc143c43361be60435e26ebf7b9eca7c5774335a27152279f049d39aa5b24f8","op":"exec","outcome":"ok","time":"2026-10-23T04:57:52.278366Z"},"type":"record"}
{"cursor":"e38f38d8a451677c09e82eb84c6e8494f5b0e33a9d2c4636696393489b6caab8","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"e38f38d8a451677c09e82eb84c6e8494f5b0e33a9d2c4636696393489b6caab8","op":"connect","outcome":"connected","time":"2026-10-23T04:57:52.732852Z"},"type":"record"}
{"cursor":"e2c90eb59636a3339cd0df42518405c3610ea5f89ffff272591830bb249ad5c0","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["cat","Projects/latch-demo/README.md"],"duration_ms":19,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000012,"latency_ms":180,"model":"typesafe/jev-1.14-20261014","model_request":"req_zkxautxbszsppfdz","p":{"COVERED":0.97,"FORBIDDEN":0.03,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:52.781233Z"},"id":"e2c90eb59636a3339cd0df42518405c3610ea5f89ffff272591830bb249ad5c0","op":"exec","outcome":"ok","time":"2026-10-23T04:57:52.981672Z"},"type":"record"}
{"cursor":"d40383d218b037fd86c7f97184cbc5fc6420355acfbb0a14d5583847a1a3161c","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"d40383d218b037fd86c7f97184cbc5fc6420355acfbb0a14d5583847a1a3161c","op":"connect","outcome":"connected","time":"2026-10-23T04:57:53.426433Z"},"type":"record"}
{"cursor":"93a9614ca6028415ac52f3cee6f959daebd09ba0b10abeef00989ef7c394997f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":{"argv":["cat","Finance/2026.csv"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":214,"model":"typesafe/jev-1.14-20261014","model_request":"req_mzzdawsrcgoaluii","p":{"COVERED":0.34,"FORBIDDEN":0.66,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"owner_said_no"},"started":"2026-10-23T04:57:53.466317Z"},"id":"93a9614ca6028415ac52f3cee6f959daebd09ba0b10abeef00989ef7c394997f","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:57:53.681028Z"},"type":"record"}
{"cursor":"8dfb79662f4b3aefa6bf291451ef0dd37d5ed38a5206c990b85caf0610475fde","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"8dfb79662f4b3aefa6bf291451ef0dd37d5ed38a5206c990b85caf0610475fde","op":"connect","outcome":"connected","time":"2026-10-23T04:57:54.097018Z"},"type":"record"}
{"cursor":"84c65cf8b063d5adfdc7e4ef045d0c204723ac0145342781702700a82917431b","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["uname","-a"],"duration_ms":18,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":186,"model":"typesafe/jev-1.14-20261014","model_request":"req_hdxckffgexqhxskg","p":{"COVERED":0.99,"FORBIDDEN":0.01,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:54.122272Z"},"id":"84c65cf8b063d5adfdc7e4ef045d0c204723ac0145342781702700a82917431b","op":"exec","outcome":"ok","time":"2026-10-23T04:57:54.328652Z"},"type":"record"}
{"cursor":"7c3c1c7e6558c03a3c4de0d5efff3ad2a0afbcfd994481968ac2ad6859b7cd7e","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"7c3c1c7e6558c03a3c4de0d5efff3ad2a0afbcfd994481968ac2ad6859b7cd7e","op":"connect","outcome":"connected","time":"2026-10-23T04:57:54.695349Z"},"type":"record"}
{"cursor":"4dd3f640612fb52093107aadafa86c014a39f00c77b386041d0f88d4a31ab814","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":null,"gate":null,"started":"2026-10-23T04:57:54.719856Z"},"id":"4dd3f640612fb52093107aadafa86c014a39f00c77b386041d0f88d4a31ab814","op":"google","outcome":"not_enabled","time":"2026-10-23T04:57:54.71992Z"},"type":"record"}
{"cursor":"fa1629671e2b00ff12cd7a6c97b386dd6df5caec50ed83e22c03fccb0fc710b8","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"fa1629671e2b00ff12cd7a6c97b386dd6df5caec50ed83e22c03fccb0fc710b8","op":"connect","outcome":"connected","time":"2026-10-23T04:58:04.519463Z"},"type":"record"}
{"cursor":"7b22c2ecbeb978b5ae0802dc6dd6c56d87d14c7d891c53d121dccb85d929a669","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"deny","details":{"args":{"argv":["ls","Projects"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","cost":0.00001,"latency_ms":416,"model":"typesafe/jev-1.14-20261014","model_request":"req_pjacfdrynfkuwigb","p":{"COVERED":0.44,"FORBIDDEN":0.0,"NOT_COVERED":0.56},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"not_covered"},"started":"2026-10-23T04:58:04.541515Z"},"id":"7b22c2ecbeb978b5ae0802dc6dd6c56d87d14c7d891c53d121dccb85d929a669","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:58:04.967315Z"},"type":"record"}
{"cursor":"1f8656f30d7ad55e5bbe4ffc9ad1e915a6848c4ba0b15f613951794f9c4f5456","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"1f8656f30d7ad55e5bbe4ffc9ad1e915a6848c4ba0b15f613951794f9c4f5456","op":"connect","outcome":"connected","time":"2026-10-23T04:58:05.362647Z"},"type":"record"}
{"cursor":"0cb70259b986c3f4a06dad3e850c70bb3791f2e5ab4371517fd2658b2b8573a2","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"deny","details":{"args":{"argv":["cat","Finance/2026.csv"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","cost":0.00001,"latency_ms":286,"model":"typesafe/jev-1.14-20261014","model_request":"req_xcprcsejhfkehxgp","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"owner_said_no"},"started":"2026-10-23T04:58:05.400589Z"},"id":"0cb70259b986c3f4a06dad3e850c70bb3791f2e5ab4371517fd2658b2b8573a2","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:58:05.688109Z"},"type":"record"}
{"cursor":"8f79bb19999f991d95fa4d5f40a8105d5a781460010c3338db102f95a60928fc","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"8f79bb19999f991d95fa4d5f40a8105d5a781460010c3338db102f95a60928fc","op":"connect","outcome":"connected","time":"2026-10-23T04:58:17.143652Z"},"type":"record"}
{"cursor":"f85509e49fcc611c3f36f66b4cbabaf6a396e6aa4e332ea0c51c951aaae3635f","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":28,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","chat":"cht_RlfbPCRJOw5jubw4FJ4nqw","cost":0.000011,"latency_ms":505,"model":"typesafe/jev-1.14-20261014","model_request":"req_hzjunxllqsenomkr","owner_messages":["msg_pzYDNrGQn0K2KzEIbRNvIg"],"p":{"COVERED":0.87,"FORBIDDEN":0.0,"NOT_COVERED":0.13},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"chat_and_rules","verdict":"allow","window":{"count":1,"first":"msg_pzYDNrGQn0K2KzEIbRNvIg","last":"msg_pzYDNrGQn0K2KzEIbRNvIg","truncated":false}},"started":"2026-10-23T04:58:17.190603Z"},"id":"f85509e49fcc611c3f36f66b4cbabaf6a396e6aa4e332ea0c51c951aaae3635f","op":"exec","outcome":"ok","time":"2026-10-23T04:58:17.738563Z"},"type":"record"}
{"cursor":"2b5df2550f8a2afbf4abf629b92bdaf2fd6985313989e80be3464f8cdf8dbd3a","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"2b5df2550f8a2afbf4abf629b92bdaf2fd6985313989e80be3464f8cdf8dbd3a","op":"connect","outcome":"connected","time":"2026-10-23T04:58:18.208986Z"},"type":"record"}
{"cursor":"5e49f93c5f5f60d2ab1b2e49d4552403bd143e1e8bc95917096631b8ca00cab7","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":{"argv":["cat","Projects/latch-demo/README.md"],"duration_ms":95,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","chat":"cht_RlfbPCRJOw5jubw4FJ4nqw","cost":0.000011,"latency_ms":416,"model":"typesafe/jev-1.14-20261014","model_request":"req_lcjvkoufkvslpbuy","owner_messages":["msg_pzYDNrGQn0K2KzEIbRNvIg"],"p":{"COVERED":0.97,"FORBIDDEN":0.0,"NOT_COVERED":0.03},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"chat_and_rules","verdict":"allow","window":{"count":1,"first":"msg_pzYDNrGQn0K2KzEIbRNvIg","last":"msg_pzYDNrGQn0K2KzEIbRNvIg","truncated":false}},"started":"2026-10-23T04:58:18.273433Z"},"id":"5e49f93c5f5f60d2ab1b2e49d4552403bd143e1e8bc95917096631b8ca00cab7","op":"exec","outcome":"ok","time":"2026-10-23T04:58:18.787025Z"},"type":"record"}
{"cursor":"6d94c96d9b41d92efb792aceb3eff19c222ddfedcf737eb9855b21bd2ea90edb","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"6d94c96d9b41d92efb792aceb3eff19c222ddfedcf737eb9855b21bd2ea90edb","op":"connect","outcome":"connected","time":"2026-10-23T05:00:04.753334Z"},"type":"record"}
{"cursor":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"deny","details":{"args":{"argv":["ls","Projects"],"network":false,"secret_env":[],"workdir":null,"writes":[]},"gate":null,"started":"2026-10-23T05:00:04.784896Z"},"id":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","op":"exec","outcome":"exec_denied","time":"2026-10-23T05:00:04.785546Z"},"type":"record"}
{"cursor":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","has_older":false,"type":"caught_up"}
"""#
    static let kitchenAudit = #"""
{"schema":1,"type":"hello"}
{"cursor":"0000000000000000000000000000000000000000000000000000000000000000","has_older":false,"type":"snapshot"}
{"cursor":"3847ada0b814d138df5111d0558eb8398095a55d302dbaa0003bdfe62b6af1a8","record":{"agent":"edad6227281f8c39c81b4b514030c0d6","caller":"c14846ee052eaabf0df311c7bd255449dfa1c7176cc889807be13aa0f5f094bb","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"3847ada0b814d138df5111d0558eb8398095a55d302dbaa0003bdfe62b6af1a8","op":"connect","outcome":"connected","time":"2026-10-23T05:00:22.20038Z"},"type":"record"}
{"cursor":"6caf882d14b2d942a136cb133de5270980e5f27adb7a4dcb533f469ce4e4cf57","record":{"agent":"edad6227281f8c39c81b4b514030c0d6","caller":"c14846ee052eaabf0df311c7bd255449dfa1c7176cc889807be13aa0f5f094bb","caller_name":null,"decision":"deny","details":{"args":{"argv":["ls","recipes"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","latency_ms":13,"rule":"allowed when within instructions >= 0.50","verdict":"no_intent"},"started":"2026-10-23T05:00:22.234276Z"},"id":"6caf882d14b2d942a136cb133de5270980e5f27adb7a4dcb533f469ce4e4cf57","op":"exec","outcome":"no_intent","time":"2026-10-23T05:00:22.248376Z"},"type":"record"}
{"cursor":"71aab5d260d64b64e45df118744f0c87e0aabb467e624082e8d89f3310ad1d41","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"71aab5d260d64b64e45df118744f0c87e0aabb467e624082e8d89f3310ad1d41","op":"connect","outcome":"connected","time":"2026-10-23T05:00:22.628448Z"},"type":"record"}
{"cursor":"246ce126dd89a3787078679404920a14da2ca46049539627a62a7a6149964d08","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","recipes"],"duration_ms":25,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":0.000011,"latency_ms":336,"model":"typesafe/jev-1.14-20261014","model_request":"req_pmhxsyrhlbigqtal","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","source":"owner_request","verdict":"allow"},"started":"2026-10-23T05:00:22.657485Z"},"id":"246ce126dd89a3787078679404920a14da2ca46049539627a62a7a6149964d08","op":"exec","outcome":"ok","time":"2026-10-23T05:00:23.019881Z"},"type":"record"}
{"cursor":"3f939488fb20e4e23cc4865e9bf21b646e7425c09724a5e1585930a71e64af37","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"3f939488fb20e4e23cc4865e9bf21b646e7425c09724a5e1585930a71e64af37","op":"connect","outcome":"connected","time":"2026-10-23T05:00:35.861887Z"},"type":"record"}
{"cursor":"3f939488fb20e4e23cc4865e9bf21b646e7425c09724a5e1585930a71e64af37","has_older":false,"type":"caught_up"}
"""#
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
            ("02-login-name-taken", .login, Fixtures.loggedOut, Progress(), { $0.login = .nameTaken(existing: "studio", lastSeen: nil); $0.loginName = "studio-2" }),
            ("02-login-verified", .login, Fixtures.serving, Progress(), { $0.login = .verified(name: "studio") }),
            ("03-privacy", .privacy, Fixtures.stopped, Progress(), { _ in }),
            ("04-rules-first-run", .rules, Fixtures.stopped, Progress(), { _ in }),
            ("04-rules-rerun-prefilled", .rules, Fixtures.stopped, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules
            }),
            ("04-rules-checking", .rules, Fixtures.serving, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules; $0.reviewing = true
                for example in $0.deck.examples { $0.previews[example.id] = .checking }
            }),
            ("04-rules-verdicts-fake-latch", .rules, Fixtures.serving, Progress(), {
                $0.savedRules = rules; $0.draftRules = rules
                let answers = [Fixtures.allow, Fixtures.allow, Fixtures.allow, Fixtures.hostRefused, Fixtures.saidNo]
                for (example, answer) in zip($0.deck.examples, answers) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.macLit = true
            }),
            ("04-rules-executive-open-fake-latch", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[1]
                $0.deck = deck; $0.savedRules = deck.rules; $0.draftRules = deck.rules
                let answers = [Fixtures.allow, Fixtures.allow, Fixtures.allow, Fixtures.notCovered, Fixtures.saidNo]
                for (example, answer) in zip(deck.examples, answers) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.openExamples = [deck.examples[4].id]
            }),
            ("04-rules-save-failed", .rules, Fixtures.stopped, Progress(), {
                $0.draftRules = rules
                $0.rulesError = "rules are at most 8 KB together; shorten them, then save again"
            }),
            ("05-google-not-checked", .google, Fixtures.stopped, Progress(), { $0.google = .unknown }),
            ("05-google-not-connected", .google, Fixtures.stopped, Progress(), {
                $0.google = .needed("Plow has no Google account this host can use; connect one in Plow, then run `latch plugins enable google` again")
            }),
            ("05-google-not-connected-with-link", .google, Fixtures.stopped, Progress(), {
                $0.google = .needed("connect a Google account in Plow"); $0.googleConnectURL = URL(string: "https://plow.co/connect/google")
            }),
            ("05-google-off-connected-in-plow", .google, Fixtures.stopped, Progress(), {
                $0.google = .unknown; $0.googleAccounts = ["owner@example.com"]
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
                app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
                app.stage(rulesSaved: rulesSaved, setup: try on.map { try Fixtures.decode(Fixtures.setup(on: $0)) })
                _ = app.stage(.done, progress: progress)
                try await renderWindow(MainView().environment(app), size: mainSize, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // The main window's host tier: each kind of host, and switching.
            let plowDown = LatchError.failed(LatchFailure(code: "plow_unreachable", message: "could not reach Plow at http://127.0.0.1:18934: error sending request for url (http://127.0.0.1:18934/v1/latch/peers)"))
            let homes: [(String, String, String?, String?, SidebarItem)] = [
                ("10-home-this-mac-serving", Fixtures.serving, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-terminal", Fixtures.servingAttached, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-stopped-update", Fixtures.stopped, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-other-version", Fixtures.otherVersion, Fixtures.studioHosts, nil, .home),
                ("10-home-this-mac-plow-unreachable", Fixtures.serving, nil, nil, .home),
                ("10-home-this-mac-stopped-plow-unreachable", Fixtures.stopped, nil, nil, .home),
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
                stageActivity(app)
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: mainSize, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // Activity: real records of each kind, a filter, the inspector,
            // another host's, and one Plow counts offline.
            let activities: [(String, String, String, String?, ActivityView.Filter, (AuditRecord) -> Bool)] = [
                ("14-activity-studio-agent-allowed", Fixtures.serving, Fixtures.studioHosts, nil, .all,
                 { $0.callerName == "scout" && $0.decision == "allow" && $0.op == "exec" }),
                ("14-activity-studio-refused-you-said-no", Fixtures.serving, Fixtures.studioHosts, nil, .refused,
                 { $0.callerName == "scout" && $0.gate?.verdict == "owner_said_no" }),
                ("14-activity-studio-exec-off", Fixtures.serving, Fixtures.studioHosts, nil, .refused, { $0.outcome == "exec_denied" }),
                ("14-activity-pi-kitchen-no-instructions", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", .all,
                 { $0.outcome == "no_intent" }),
                ("14-activity-pi-kitchen-offline", Fixtures.laptop, Fixtures.laptopHostsPiOffline, "pi-kitchen", .all, { _ in false }),
            ]
            for (name, status, account, chosen, filter, pick) in activities {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(status))
                app.stage(account: try Fixtures.decode(account))
                app.chosenHost = chosen
                app.section = .activity
                app.activityFilter = filter
                stageActivity(app)
                app.selectedRecord = app.host.flatMap { app.feed(for: $0).records.last(where: pick)?.id }
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: CGSize(width: 1180, height: 640), dark: dark,
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

    /// Each host's activity as `latch audit` printed it.
    private static func stageActivity(_ app: AppModel) {
        for host in app.hosts where host.name == "studio" || host.name == "pi-kitchen" {
            app.feed(for: host).stage(host.name == "studio" ? Fixtures.studioAudit : Fixtures.kitchenAudit)
        }
    }

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

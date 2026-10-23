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
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws {}
    func agents(host: String?) async throws -> AgentsList {
        switch host {
        case nil: try Fixtures.decode(Fixtures.agents)
        case "pi-kitchen"?: try Fixtures.decode(Fixtures.agentsKitchen)
        default: try Fixtures.decode(Fixtures.agentsStudio)
        }
    }
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        try Fixtures.decode(Fixtures.tries["not-covered"]!)
    }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { try Fixtures.decode(Fixtures.vaultShows[item]!) }
    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item { throw LatchError.unreachable("a still") }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        throw LatchError.unreachable("a still")
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws {}
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
    func disable(_ plugin: String) async throws {}
    func devices() async throws -> DeviceList { try Fixtures.decode(Fixtures.devices) }
    func removeDevice(_ name: String) async throws {}
    func updateCheck() async throws -> UpdateCheck { try Fixtures.decode(Fixtures.updateCheck) }
    func logout() async throws -> LogoutResult { LogoutResult(device: nil) }
    func agentCatalog() async throws -> AgentCatalog { try Fixtures.decode(Fixtures.catalog) }
    func agentLines() async throws -> AgentLines { try Fixtures.decode(Fixtures.lines) }
    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated {
        throw LatchError.unreachable("a still")
    }
    func agentShow(_ uid: String) async throws -> AgentsList.Agent { throw LatchError.unreachable("a still") }
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
    static let stopped = #"{"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2,"peer":{"name":"studio","key":"6b55a78f","kind":"device"},"serve":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null},"last_error":null,"update":{"available":true,"build":1342,"checked_at":"2026-10-22T22:07:54.186472Z"}}"#
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
    // `vault list` and `vault show` on vstudio: real runs.
    static let vaultItems = #"{"audit_id":"f9283602fbe14976983de000e647f02d9fc24b58c0cede0c0929370005928e51","items":[{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"},{"alias":false,"custom":false,"hidden":true,"label":"totp"}],"item":"v1","kind":"login","revision":2,"sites":["shop.example"],"title":"Shop","totp":true,"username":"ada"},{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"notes"}],"item":"v2","kind":"note","revision":2,"sites":[],"title":"Building notes","totp":false},{"fields":[{"alias":false,"custom":false,"hidden":true,"label":"number"},{"alias":false,"custom":false,"hidden":true,"label":"code"},{"alias":false,"custom":false,"hidden":false,"label":"expiry month"},{"alias":false,"custom":false,"hidden":false,"label":"expiry year"},{"alias":false,"custom":false,"hidden":false,"label":"cardholder name"},{"alias":false,"custom":false,"hidden":false,"label":"brand"},{"alias":false,"custom":false,"hidden":false,"label":"expiry"}],"item":"v3","kind":"card","revision":1,"sites":[],"title":"Visa (test)","totp":false},{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"title"},{"alias":false,"custom":false,"hidden":false,"label":"first name"},{"alias":false,"custom":false,"hidden":false,"label":"middle name"},{"alias":false,"custom":false,"hidden":false,"label":"last name"},{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":false,"label":"company"},{"alias":false,"custom":false,"hidden":true,"label":"ssn"},{"alias":false,"custom":false,"hidden":true,"label":"passport number"},{"alias":false,"custom":false,"hidden":false,"label":"license number"},{"alias":false,"custom":false,"hidden":false,"label":"email"},{"alias":false,"custom":false,"hidden":false,"label":"phone"},{"alias":false,"custom":false,"hidden":false,"label":"address1"},{"alias":false,"custom":false,"hidden":false,"label":"city"},{"alias":false,"custom":false,"hidden":false,"label":"state"},{"alias":false,"custom":false,"hidden":false,"label":"postal code"},{"alias":false,"custom":false,"hidden":false,"label":"country"},{"alias":false,"custom":false,"hidden":false,"label":"date of birth"}],"item":"v4","kind":"identity","revision":2,"sites":[],"title":"Me (test)","totp":false},{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v6","kind":"login","other_top_sites":["grocer-login.example"],"revision":1,"sites":["grocer.example"],"title":"Grocer","totp":false,"username":"ada@example.com"}]}"#
    static let vaultShows: [String: String] = ["v1": #"{"audit_id":"4717b21309079ff522a8e928d3a3568bce6153501a8c88a113bf7f5f22c964a4","item":{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"},{"alias":false,"custom":false,"hidden":true,"label":"totp"}],"item":"v1","kind":"login","revision":2,"sites":["shop.example"],"title":"Shop","totp":true,"username":"ada"},"values":{"username":"ada"}}"#, "v2": #"{"audit_id":"6b10d127b072ef1aa4029d6fcf3f3d9e4a12ad9ff1ad255208e11e48fbb4dccd","item":{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"notes"}],"item":"v2","kind":"note","revision":2,"sites":[],"title":"Building notes","totp":false},"values":{"notes":"Gate code is in the binder."}}"#, "v3": #"{"audit_id":"413bffa87e6bbae0a5168ddbaa4175a861b2783f8fa0c4a93d26e9fd4d66ee0b","item":{"fields":[{"alias":false,"custom":false,"hidden":true,"label":"number"},{"alias":false,"custom":false,"hidden":true,"label":"code"},{"alias":false,"custom":false,"hidden":false,"label":"expiry month"},{"alias":false,"custom":false,"hidden":false,"label":"expiry year"},{"alias":false,"custom":false,"hidden":false,"label":"cardholder name"},{"alias":false,"custom":false,"hidden":false,"label":"brand"},{"alias":false,"custom":false,"hidden":false,"label":"expiry"}],"item":"v3","kind":"card","revision":1,"sites":[],"title":"Visa (test)","totp":false},"values":{"brand":"Visa","cardholder name":"Ada L","expiry":"2031-04","expiry month":"04","expiry year":"2031"}}"#, "v4": #"{"audit_id":"7d4a4fff30229a51f4efc6cfe33f086b4c2b22906f611c91e06e24e4b9b41739","item":{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"title"},{"alias":false,"custom":false,"hidden":false,"label":"first name"},{"alias":false,"custom":false,"hidden":false,"label":"middle name"},{"alias":false,"custom":false,"hidden":false,"label":"last name"},{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":false,"label":"company"},{"alias":false,"custom":false,"hidden":true,"label":"ssn"},{"alias":false,"custom":false,"hidden":true,"label":"passport number"},{"alias":false,"custom":false,"hidden":false,"label":"license number"},{"alias":false,"custom":false,"hidden":false,"label":"email"},{"alias":false,"custom":false,"hidden":false,"label":"phone"},{"alias":false,"custom":false,"hidden":false,"label":"address1"},{"alias":false,"custom":false,"hidden":false,"label":"city"},{"alias":false,"custom":false,"hidden":false,"label":"state"},{"alias":false,"custom":false,"hidden":false,"label":"postal code"},{"alias":false,"custom":false,"hidden":false,"label":"country"},{"alias":false,"custom":false,"hidden":false,"label":"date of birth"}],"item":"v4","kind":"identity","revision":2,"sites":[],"title":"Me (test)","totp":false},"values":{"address1":"1 St","city":"London","company":"Co","country":"UK","date of birth":"1815-12-10","email":"ada@example.com","first name":"Ada","last name":"Lovelace","license number":"L1","middle name":"K","phone":"555","postal code":"N1","state":"LDN","title":"Dr","username":"ada2"}}"#, "v6": #"{"audit_id":"e20c23c3a649234daae8e584c7b9621856cf6d253b4871e7bc2ab8c6c7ef2125","item":{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v6","kind":"login","other_top_sites":["grocer-login.example"],"revision":1,"sites":["grocer.example"],"title":"Grocer","totp":false,"username":"ada@example.com"},"values":{"username":"ada@example.com"}}"#]
    // Another host's, read with `--host`.
    static let remoteVaultItems = #"{"audit_id":"7eff2d3d62fc8f3d16f422e2e19ea204fdf62d80e9d9ed0eee14bf1f6e9400d7","items":[{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v1","kind":"login","revision":1,"sites":["router.example"],"title":"Router (test)","totp":false,"username":"admin"}]}"#
    static let remoteVaultShow = #"{"audit_id":"004c3cca9d220ff417c05d233f61fe3741a745079edd629be94432757a376f70","item":{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v1","kind":"login","revision":1,"sites":["router.example"],"title":"Router (test)","totp":false,"username":"admin"},"values":{"username":"admin"}}"#
    // `latch devices` (the studio account's, laptop logged in without hosting),
    // `update --check`, `agents catalog` and `agents lines`: real runs.
    static let devices = #"{"devices":[{"key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","kind":"device","name":"laptop","relay_url":null,"last_seen_at":"2026-10-22T22:05:50.582906Z","online":true},{"key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","kind":"device","name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:30.037004Z","online":true},{"key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","kind":"device","name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:33.384336Z","online":true},{"key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","kind":"device","name":"studio","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:26.724881Z","online":true}]}"#
    static let updateCheck = #"{"available":false,"current":null,"latest":{"build":138,"tag":"v0.1.0-138"}}"#
    static let catalog = #"{"kinds":[{"id":"exe:checkpoint","name":"Checkpoint"},{"id":"exe:clip-warden","name":"Clip Warden"},{"id":"exe:dailies","name":"Video Cuts"},{"id":"exe:danedelattre-x-manager","name":"X Manager"},{"id":"exe:fandom","name":"Fandom"},{"id":"exe:fipe","name":"Fipe"},{"id":"exe:founder-agent","name":"Founder Agent"},{"id":"exe:galahad-hackathon","name":"Joust"},{"id":"exe:hermes","name":"Hermes"},{"id":"exe:hermes-cat-paw","name":"Hermes Cat Paw"},{"id":"exe:instagram-social-selling","name":"Instagram Social Selling"},{"id":"exe:life","name":"Life Assistant"},{"id":"exe:mako","name":"Mako"},{"id":"exe:meals-planner","name":"Meals Planner"},{"id":"exe:memory-vault","name":"Memory Vault"},{"id":"exe:metabolix","name":"Metabolix"},{"id":"exe:nutrilatch","name":"NutriLatch"},{"id":"exe:parley","name":"Parley"},{"id":"exe:phoneflow-agent","name":"PhoneFlow"},{"id":"exe:pm-second-brain","name":"PM Agent"},{"id":"exe:pruce","name":"Prucê"},{"id":"exe:radar","name":"RadaR"},{"id":"exe:receiptsnap","name":"ReceiptSnap"},{"id":"exe:repro-relay","name":"Repro Relay"},{"id":"exe:saved","name":"Saved"},{"id":"exe:text-me","name":"text-me"},{"id":"exe:the-commish","name":"The Commish"},{"id":"exe:theplowtimes","name":"The Plow Times"},{"id":"exe:vela","name":"Vela"},{"id":"exe:vigia","name":"Vigia"},{"id":"exe:your-mother","name":"Your Mother"},{"id":"exe:zoen","name":"Zoen"}]}"#
    static let lines = #"{"lines":[{"agent_uid":null,"available":true,"name":null,"number":"+15550000002","uid":"ln_p1"},{"agent_uid":null,"available":true,"name":null,"number":"+15550000003","uid":"ln_p2"},{"agent_uid":null,"available":true,"name":null,"number":"+15550000004","uid":"ln_p3"},{"agent_uid":null,"available":true,"name":null,"number":"+15550000005","uid":"ln_p4"},{"agent_uid":null,"available":true,"name":null,"number":"+15550000006","uid":"ln_p5"}]}"#
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
    // `latch agents [--host]`, `rules show --host` and `rules try` from laptop,
    // real runs on two hosts (pi-kitchen refusing helper exec), names and
    // home folders shortened.
    static let agents = #"{"agents":[{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.397426","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.523892","line":{"name":null,"number":"+15550000002"},"name":"scout","standing":null,"status":null,"uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.569323","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.604826","line":{"name":null,"number":"+15550000003"},"name":"helper","standing":null,"status":null,"uid":"3971582e59baad83250141c8dcc95855"}],"host":null}"#
    static let agentsStudio = #"{"agents":[{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.397426","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.523892","line":{"name":null,"number":"+15550000002"},"name":"scout","standing":{"exec_blocked":false,"own_rules":true},"status":null,"uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.569323","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.604826","line":{"name":null,"number":"+15550000003"},"name":"helper","standing":{"exec_blocked":false,"own_rules":false},"status":null,"uid":"3971582e59baad83250141c8dcc95855"}],"host":{"name":"studio","online":true}}"#
    static let agentsKitchen = #"{"agents":[{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.397426","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.523892","line":{"name":null,"number":"+15550000002"},"name":"scout","standing":{"exec_blocked":false,"own_rules":false},"status":null,"uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.569323","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45.604826","line":{"name":null,"number":"+15550000003"},"name":"helper","standing":{"exec_blocked":true,"own_rules":false},"status":null,"uid":"3971582e59baad83250141c8dcc95855"}],"host":{"name":"pi-kitchen","online":true}}"#
    static let agentsKitchenOffline = #"{"error":{"code":"host_offline","message":"host \"pi-kitchen\" is offline; nothing was sent. Start `latch serve` there"}}"#
    static let rulesStudio = #"{"agents":[{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd","revision":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","text":"scout may run builds and tests in ~/Projects/latch-demo."}],"audit_id":"94b02a5438b53063f41edeb8a2babaaefb3ffaefee225dbfe5e94a2ddff078bf","deny_exec":{"agent_uids":[],"every_agent":false},"every_agent":{"revision":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c","text":"Agents may list and read files in ~/Projects. Never delete anything, and never touch ~/Finance.\n"},"host":"studio","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    static let rulesKitchen = #"{"agents":[],"audit_id":"d5776f75782524ce226a62a46607b95c3382c957b8b44a342456171d2345fd49","deny_exec":{"agent_uids":["3971582e59baad83250141c8dcc95855"],"every_agent":false},"every_agent":null,"host":"pi-kitchen","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    static let tries: [String: String] = [
        "allow-owner": #"{"as":"owner","gate":{"caller":"owner_device","cost":0.000011,"latency_ms":268,"model":"typesafe/jev-1.14-20261014","model_request":"req_nzfeeqvhlqnslxsf","p":{"COVERED":0.9,"FORBIDDEN":0.08,"NOT_COVERED":0.02},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"host":"studio","request":{"action":{"argv":["ls","Projects"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"allow"}"#,
        "allow-scout": #"{"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":0.00001,"latency_ms":230,"model":"typesafe/jev-1.14-20261014","model_request":"req_xcxovwucwqgsvcis","p":{"COVERED":0.56,"FORBIDDEN":0.01,"NOT_COVERED":0.43},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["make","test"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me/Projects/latch-demo","writes":[]},"op":"exec"},"verdict":"allow"}"#,
        "not-covered": #"{"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":0.00001,"latency_ms":251,"model":"typesafe/jev-1.14-20261014","model_request":"req_ikrytewoewrfscag","p":{"COVERED":0.0,"FORBIDDEN":0.0,"NOT_COVERED":1.0},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"not_covered"},"host":"studio","request":{"action":{"argv":["curl","--version"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"}"#,
        "said-no": #"{"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","cost":0.00001,"latency_ms":213,"model":"typesafe/jev-1.14-20261014","model_request":"req_dvpscpwflpqagaja","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"rule":"allowed when within instructions >= 0.50","rules":{"agent":"a33189e9c51bcecdd839cfc226d392a7ce6c00eb700b08ce25fa325e2c29f780","host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"owner_said_no"},"host":"studio","request":{"action":{"argv":["cat","Finance/2026.csv"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"owner_said_no"}"#,
        "no-intent": #"{"as":{"agent_name":"scout","agent_uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"},"gate":{"caller":"agent","latency_ms":11,"rule":"allowed when within instructions >= 0.50","rules":null,"verdict":"no_intent"},"host":"pi-kitchen","request":{"action":{"argv":["ls"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"no_intent"}"#,
        "host-refused": #"{"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":null,"host":"pi-kitchen","refused":{"code":"exec_denied","message":"You turned exec off for this agent on this host when you started it with `latch serve --deny-exec=3971582e59baad83250141c8dcc95855`. Start it again without that flag to turn exec back on."},"request":{"action":{"argv":["ls"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"host_refused"}"#,
        "new-agent": #"{"as":"new_agent","gate":{"caller":"new_agent","cost":0.00001,"latency_ms":212,"model":"typesafe/jev-1.14-20261014","model_request":"req_nnlzesajgklnyqbg","p":{"COVERED":0.58,"FORBIDDEN":0.01,"NOT_COVERED":0.41},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["ls","Projects"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"allow"}"#,
        "draft": #"{"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":{"caller":"agent","cost":0.00001,"latency_ms":242,"model":"typesafe/jev-1.14-20261014","model_request":"req_hplzksgpinmlheze","p":{"COVERED":0.21,"FORBIDDEN":0.0,"NOT_COVERED":0.79},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"7d8f456802c7a988d09fc874e41d23543e9b8e2d810ab5c7d110757f193a90ff"},"source":"rules","verdict":"not_covered"},"host":"studio","request":{"action":{"argv":["touch","Projects/x"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"}"#,
    ]
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
        // The Full Disk Access helper, as wide as it floats under a
        // 715-point System Settings window: waiting, mid-drag, granted.
        let icon = NSImage(contentsOf: URL(filePath: #filePath).appending(path: "../../../../../macos/AppIcon.icns").standardized)
        for dark in [false, true] {
            for (name, granted, dragging) in [("waiting", false, false), ("dragging", false, true), ("granted", true, false)] {
                let helper = GrantHelper(bundle: URL(filePath: "/Applications/Plow Latch.app"), icon: icon)
                helper.stage(granted: granted, dragging: dragging)
                let width = 715 - SettingsTracker.sidebarWidth
                let height = NSHostingController(rootView: GrantPanelView(helper: helper))
                    .sizeThatFits(in: CGSize(width: width, height: 400)).height.rounded(.up)
                try await render(GrantPanelView(helper: helper).opacity(dragging ? 0.72 : 1), size: CGSize(width: width, height: height),
                                 dark: dark, background: false, file: output.appending(path: "10-fda-helper-\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // Agents and Rules: real agents and standings (sidecar offline), an
        // agent's page, a host's rules, and Try a request's verdicts.
        for dark in [false, true] {
            let sidecarOffline = Fixtures.studioHosts.replacingOccurrences(
                of: #""name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:33.384336Z","online":true"#,
                with: #""name":"sidecar","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":null,"online":false"#)
            let scout = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd", helper = "3971582e59baad83250141c8dcc95855"
            let screens: [(String, SidebarItem, String?, [String], (AppModel) throws -> Void)] = [
                ("15-agents-pills", .agents, nil, [], { _ in }),
                ("15-agent-page-helper", .agents, nil, [helper], { _ in }),
                ("16-rules-studio-scout-try-allowed", .rules, nil, [], { app in
                    app.rulesFocus = scout
                    app.trial.stage(try Fixtures.decode(Fixtures.tries["allow-scout"]!), request: "exec --workdir ~/Projects/latch-demo -- make test",
                                    caller: .agent(scout), on: "studio")
                }),
                ("16-rules-studio-try-you-said-no", .rules, nil, [], { app in
                    app.trial.stage(try Fixtures.decode(Fixtures.tries["said-no"]!), request: "exec -- cat Finance/2026.csv",
                                    caller: .agent(scout), on: "studio")
                }),
                ("16-rules-pi-kitchen-helper-exec-refused", .rules, "pi-kitchen", [], { app in
                    app.rulesFocus = helper
                    app.trial.stage(try Fixtures.decode(Fixtures.tries["host-refused"]!), request: "exec -- ls",
                                    caller: .agent(helper), on: "pi-kitchen")
                }),
            ]
            for (name, section, chosen, path, stage) in screens {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(Fixtures.serving))
                app.stage(account: try Fixtures.decode(sidecarOffline))
                app.stage(agents: try Fixtures.decode(Fixtures.agents), standings: [
                    "studio": .success(try Fixtures.decode(Fixtures.agentsStudio)),
                    "pi-kitchen": .success(try Fixtures.decode(Fixtures.agentsKitchen)),
                ])
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.rulesStudio))), for: "studio")
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.rulesKitchen))), for: "pi-kitchen")
                app.chosenHost = chosen
                app.section = section
                app.agentsPath = path
                try stage(app)
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: CGSize(width: 1120, height: 700), dark: dark,
                                       file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // Vault: this Mac's items with each kind in the inspector, the empty
        // state, another host's, one offline, and the sheets.
        for dark in [false, true] {
            let piOffline = Fixtures.studioHosts.replacingOccurrences(
                of: #""name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:30.037004Z","online":true"#,
                with: #""name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":null,"online":false"#)
            let screens: [(String, String?, String, String?, String)] = [
                ("17-vault-login-2fa", nil, Fixtures.vaultItems, "v1", Fixtures.studioHosts),
                ("17-vault-card", nil, Fixtures.vaultItems, "v3", Fixtures.studioHosts),
                ("17-vault-identity", nil, Fixtures.vaultItems, "v4", Fixtures.studioHosts),
                ("17-vault-login-also-inside", nil, Fixtures.vaultItems, "v6", Fixtures.studioHosts),
                ("17-vault-empty", nil, #"{"items":[]}"#, nil, Fixtures.studioHosts),
                ("17-vault-remote-pi-kitchen", "pi-kitchen", Fixtures.remoteVaultItems, "v1", Fixtures.studioHosts),
                ("17-vault-remote-offline", "pi-kitchen", #"{"items":[]}"#, nil, piOffline),
            ]
            for (name, chosen, items, selected, account) in screens {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(Fixtures.serving))
                app.stage(account: try Fixtures.decode(account))
                app.stage(summary: HostSummary(vault: .success(try Fixtures.decode(items))), for: chosen ?? "studio")
                let shows = chosen == nil ? Array(Fixtures.vaultShows.values) : [Fixtures.remoteVaultShow]
                for show in shows { app.stage(vaultShow: try Fixtures.decode(show), on: chosen ?? "studio") }
                app.chosenHost = chosen
                app.section = .vault
                app.selectedVaultItem = selected
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: CGSize(width: 1020, height: 620), dark: dark,
                                       file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            let app = AppModel(client: FakeLatchClient(), still: true)
            app.stage(status: try Fixtures.decode(Fixtures.serving))
            app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
            guard let studio = app.host else { return }
            let newLogin = VaultEditor(on: studio)
            newLogin.title = "Grocer"; newLogin.sites = "grocer.example"; newLogin.values = ["username": "ada@example.com", "password": "secret"]
            let editCard = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v3"]!), on: studio)
            let stale = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!), on: studio)
            stale.values["username"] = "ada2"; stale.error = AppModel.itemChangedElsewhere
            let sheets: [(String, AnyView)] = [
                ("18-vault-new-login", AnyView(VaultItemSheet(editor: newLogin))),
                ("18-vault-new-identity", AnyView(VaultItemSheet(editor: VaultEditor(kind: .identity, on: studio)))),
                ("18-vault-edit-card", AnyView(VaultItemSheet(editor: editCard))),
                ("18-vault-edit-changed-elsewhere", AnyView(VaultItemSheet(editor: stale))),
            ]
            for (name, sheet) in sheets {
                let size = name.contains("identity") ? CGSize(width: 480, height: 640) : CGSize(width: 480, height: 520)
                try await render(sheet.environment(app), size: size, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // Host Settings: this Mac serving, stopped with an update, another
        // host online and offline, viewer-only; and the New Agent sheet.
        for dark in [false, true] {
            let piOffline = Fixtures.studioHosts.replacingOccurrences(
                of: #""name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":"2026-10-22T22:06:30.037004Z","online":true"#,
                with: #""name":"pi-kitchen","relay_url":"https://usw1-1.relay.n0.iroh.link./","last_seen_at":null,"online":false"#)
            let screens: [(String, String, String, String?, Bool)] = [
                ("19-settings-this-mac-serving", Fixtures.serving, Fixtures.studioHosts, nil, false),
                ("19-settings-this-mac-stopped-update", Fixtures.stopped, Fixtures.studioHosts, nil, true),
                ("19-settings-remote-online", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", false),
                ("19-settings-remote-offline", Fixtures.serving, piOffline, "pi-kitchen", false),
                ("19-settings-viewer-only", Fixtures.laptop, Fixtures.laptopHosts, nil, false),
            ]
            for (name, status, account, chosen, checked) in screens {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(status))
                app.stage(account: try Fixtures.decode(account))
                app.stage(agents: try Fixtures.decode(Fixtures.agents), standings: [:])
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.rulesStudio)),
                                               plugins: .success(try Fixtures.decode(Fixtures.studioPlugins))), for: "studio")
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.rulesKitchen)),
                                               plugins: .success(try Fixtures.decode(Fixtures.piPlugins))), for: "pi-kitchen")
                app.stage(devices: try Fixtures.decode(Fixtures.devices), update: checked ? try Fixtures.decode(Fixtures.updateCheck) : nil)
                app.chosenHost = chosen
                app.section = .settings
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: CGSize(width: 900, height: 900), dark: dark,
                                       file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            let app = AppModel(client: FakeLatchClient(), still: true)
            app.stage(status: try Fixtures.decode(Fixtures.serving))
            app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
            let catalog: AgentCatalog = try Fixtures.decode(Fixtures.catalog)
            let lines: AgentLines = try Fixtures.decode(Fixtures.lines)
            let agent = #"{"cloud_kind":null,"created_at":"2026-10-23T05:37:45.397426","failure_code":null,"kind":"own","last_seen_at":null,"line":{"name":null,"number":"+15550000002"},"name":"scout","standing":null,"status":null,"uid":"ba3c0b3ad1eba7a9a1eb04b67c9db0dd"}"#
            let cloud = NewAgent()
            cloud.name = "Meals"; cloud.cloudKind = "exe:meals-planner"
            cloud.stage(catalog: catalog, lines: lines)
            let own = NewAgent()
            own.kind = .own; own.name = "scout"; own.tokenFile = URL(filePath: "/Users/me/Documents/scout.latch-key")
            own.stage(catalog: catalog, lines: lines)
            let ownDone = NewAgent()
            ownDone.stage(catalog: catalog, lines: lines, created: try Fixtures.decode(
                #"{"agent":\#(agent),"api_url":"http://127.0.0.1:18934","token_file":"/Users/me/Documents/scout.latch-key"}"#))
            let cloudAgent = #"{"cloud_kind":"exe:meals-planner","created_at":"2026-10-23T05:37:45.397426","failure_code":null,"kind":"cloud","last_seen_at":null,"line":{"name":null,"number":"+15550000003"},"name":"Meals","standing":null,"status":"provisioning","uid":"3971582e59baad83250141c8dcc95855"}"#
            let cloudStarting = NewAgent()
            cloudStarting.stage(catalog: catalog, lines: lines, created: try Fixtures.decode(#"{"agent":\#(cloudAgent),"api_url":null,"token_file":null}"#))
            for (name, flow) in [("20-new-agent-cloud", cloud), ("20-new-agent-own", own), ("20-new-agent-own-created", ownDone),
                                 ("20-new-agent-cloud-starting", cloudStarting)] {
                try await render(NewAgentSheet(flow: flow).environment(app), size: CGSize(width: 520, height: 500), dark: dark,
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

    private static func render(_ view: some View, size: CGSize, dark: Bool, background: Bool = true, file: URL) async throws {
        let host = NSHostingView(rootView: view)
        // The panel is sized as the helper sizes it, not to its content.
        if !background { host.sizingOptions = [] }
        // A floating panel has no title bar, nor its minimum height.
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: background ? [.titled, .fullSizeContentView] : [.borderless],
                              backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentView = host
        window.alphaValue = 0.01
        window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(400))
        try Render.png(host, to: file, background: background)
        window.orderOut(nil)
        print(file.path)
    }
}
#endif

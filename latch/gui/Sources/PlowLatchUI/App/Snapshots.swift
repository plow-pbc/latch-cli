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
    func hostStatus(_ host: String) async throws -> LocalStatus { try Fixtures.decode(Fixtures.piStatus) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func rulesShow(host: String?) async throws -> RulesShow {
        try Fixtures.decode(rules.map { #"{"agents":[],"every_agent":{"revision":"r1","text":"\#($0)"},"host":"studio"}"# }
            ?? #"{"agents":[],"every_agent":null,"host":"studio"}"#)
    }
    func rulesSet(_ text: String) async throws {}
    var connectorsOn: [String] = []
    /// Google's step in `setup --status`, when not the default.
    var google: String?
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: connectorsOn, google: google)) }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func signInGoogle(addAccount: Bool) -> AsyncThrowingStream<GoogleSignInEvent, Error> { AsyncThrowingStream { _ in } }
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
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview {
        (try Fixtures.decode(Fixtures.importPreview) as ImportPreviewFixture).preview
    }
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult { try Fixtures.decode(Fixtures.importResult) }
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

/// `vault import --preview`'s answer, around the preview.
nonisolated struct ImportPreviewFixture: Decodable { var preview: ImportPreview }

nonisolated enum Fixtures {
    /// Google's step in `latch setup --status` on local Plow: no account
    /// connected; one connected (through Plow's OAuth stand-in) after an
    /// enable that found none; turned on; and Plow not answering.
    static let googleNotConnected = #"{"step":"google","state":"needed","reason":"Google is not connected in Plow: connect it at http://localhost:3000/connections, then run `latch plugins enable google`.","args":null,"enabled":false,"ready":false,"accounts":[],"connect_url":"http://localhost:3000/connections"}"#
    static let googleConnectedOff = #"{"step":"google","state":"needed","reason":"Google is connected in Plow (testuser@plow.test); the Google connector needs setup: Plow has no Google account this host can use; connect one at http://localhost:3000/connections, then run `latch plugins enable google` again","args":["plugins","enable","google"],"enabled":false,"ready":false,"accounts":["testuser@plow.test"]}"#
    static let googleOn = #"{"step":"google","state":"done","reason":"Google is connected in Plow (testuser@plow.test); the Google connector is on; agents can use it once latch is running.","args":null,"enabled":true,"ready":false,"accounts":["testuser@plow.test"]}"#
    static let googleUnreachable = #"{"step":"google","state":"blocked","reason":"Plow could not say whether Google is connected: could not reach Plow at http://127.0.0.1:9: error sending request for url (http://127.0.0.1:9/v1/connectors)","args":null,"enabled":true,"ready":false}"#

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

    /// A sign-in link as Plow mints one, its code made up.
    static let signInLink = URL(string: "http://127.0.0.1:18934/v1/connectors/gmail/connect/EXAMPLE_ONLY")!

    /// `setup --status`'s Google step with these accounts connected in Plow
    /// (the plugin off here).
    static let threeAccounts = ["alex@example.com", "alex.work@example.com", "family@example.com"]
    static func googleConnected(_ accounts: [String], needsSignIn: [String] = []) -> String {
        func list(_ names: [String]) -> String { "[" + names.map { "\"\($0)\"" }.joined(separator: ",") + "]" }
        let again = needsSignIn.isEmpty ? "" : #","needs_sign_in":\#(list(needsSignIn))"#
        return #"{"step":"google","state":"needed","reason":"Google is connected in Plow (\#(accounts.joined(separator: ", "))); the Google connector is off.","args":["plugins","enable","google"],"enabled":false,"ready":false,"accounts":\#(list(accounts))\#(again)}"#
    }

    static let serving = #"{"login":{"kind":"device"},"this_machine":{"name":"studio","key":"6b55a78f","hosting":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":32250,"blocked":null}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2}"#
    static let stopped = #"{"login":{"kind":"device"},"this_machine":{"name":"studio","key":"6b55a78f","hosting":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null}},"update":{"available":true,"build":1342,"checked_at":"2026-10-22T22:07:54.186472Z"},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2}"#
    static let loggedOut = #"{"login":null,"this_machine":{"name":null,"key":null,"hosting":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2}"#
    static let servingAttached = #"{"login":{"kind":"device"},"this_machine":{"name":"studio","key":"6b55a78f","hosting":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":true,"state":"serving","pid":32250,"blocked":null}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2}"#
    static let otherVersion = #"{"login":{"kind":"device"},"this_machine":{"name":"studio","key":"6b55a78f","hosting":{"running":true,"ready":false,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":4242,"blocked":{"code":"other_version","message":"a host from another latch version (pid 4242) holds this state folder; stop it with `kill 4242`, then start latch again"}}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0","protocol":2}"#
    static let laptop = #"{"login":{"kind":"device"},"this_machine":{"name":"laptop","key":"0cdaf3c7601ab886e37b9835cfe9902617aeed88ffd3d066b11bc77eef548b29","hosting":{"running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null,"blocked":null}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"/Users/me/.latch","version":"0.1.0 (commit 6031e5a50, built 2026-10-22 22:03 UTC, local build, protocol plow/latch/2)","protocol":2}"#
    // `latch status` on studio (a host) and on laptop (never served), and
    // laptop's once pi-kitchen had stopped.
    static let studioHosts = #"{"account":{"owner_uid":"usr_5f2c9a7e","display_name":"Morgan"},"hosts":[{"name":"studio","key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","self":true,"online":true,"last_seen_at":"2026-10-22T22:06:26.724881Z"},{"name":"pi-kitchen","key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z"},{"name":"sidecar","key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z"}],"default_host":"studio","default_host_reason":null,"api_url":"http://127.0.0.1:18934"}"#
    static let laptopHosts = #"{"account":{"owner_uid":"usr_5f2c9a7e","display_name":"Morgan"},"hosts":[{"name":"pi-kitchen","key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z"},{"name":"sidecar","key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z"},{"name":"studio","key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:26.724881Z"}],"default_host":null,"default_host_reason":"host_required","api_url":"http://127.0.0.1:18934"}"#
    static let laptopHostsPiOffline = #"{"account":{"owner_uid":"usr_5f2c9a7e","display_name":"Morgan"},"hosts":[{"name":"pi-kitchen","key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","self":false,"online":false,"last_seen_at":null},{"name":"sidecar","key":"b58ac41605d91211b1d5c052addea8d6e7b81702d4902f746e6d639e38909364","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z"},{"name":"studio","key":"6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f","self":false,"online":true,"last_seen_at":"2026-10-22T22:06:26.724881Z"}],"default_host":null,"default_host_reason":"host_required","api_url":"http://127.0.0.1:18934"}"#
    static let noHosts = #"{"account":{"owner_uid":"usr_5f2c9a7e","display_name":"Morgan"},"hosts":[],"default_host":null,"default_host_reason":"no_host","api_url":"http://127.0.0.1:18934"}"#
    // `rules show`, `vault list` and `status --local`'s `plugins` on studio,
    // and on pi-kitchen (read from laptop with `--host`).
    static let studioRules = #"{"agents":[{"agent_name":"scout","agent_uid":"0432eb0ee1e99e24c40c54fc2bada229","note":"no latch registered when set","revision":"8be9ed94de19f40ae8a8f06b98bb6ee54da22900b8191b70dd2f2486edf1a1f0","text":"scout may run builds and tests in ~/Projects/latch-demo with --write there, and may use the network for package downloads."}],"audit_id":"81c7dce2aa3758db17a746b2284596fcfb4e2773a99d34cc49cee9e6071b2e3f","every_agent":{"revision":"9b1e8ee3911c42472937f948983a24e6c087c4f70d5bc5b924b84c799f0bff05","text":"Agents may read and summarize anything in ~/Projects. Ask me before sending any email or deleting files. Never touch ~/Finance."},"host":"studio","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    static let studioVault = #"{"audit_id":"bcaf28dedfe777a60303c3ccd802a8a2380237b6f02988bc33dabb307431e0a6","items":[{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v1","kind":"login","revision":1,"sites":["shop.example"],"title":"Shop (test)","totp":false,"username":"ada"},{"fields":[{"alias":false,"custom":false,"hidden":false,"label":"username"},{"alias":false,"custom":false,"hidden":true,"label":"password"}],"item":"v2","kind":"login","other_top_sites":["grocer-login.example"],"revision":1,"sites":["grocer.example"],"title":"Grocer (test)","totp":false,"username":"ada@example.com"},{"fields":[],"item":"v3","kind":"note","revision":1,"sites":[],"title":"Building notes (test)","totp":false},{"fields":[{"alias":false,"custom":false,"hidden":true,"label":"number"},{"alias":false,"custom":false,"hidden":true,"label":"code"},{"alias":false,"custom":false,"hidden":false,"label":"cardholder name"}],"item":"v4","kind":"card","revision":1,"sites":["shop.example"],"title":"Visa (test)","totp":false}]}"#
    static let studioPlugins = #"[{"enable":"latch plugins enable google","name":"google","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable messages","name":"messages","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable contacts","name":"contacts","reason":"its owner has not enabled it on this host","state":"disabled"}]"#
    /// studio's after turning Google on, with Messages lacking Full Disk Access.
    static let studioPluginsSetUp = #"[{"enable":null,"name":"google","reason":"enabled; agents can use it once latch is running","state":"enabled"},{"enable":"latch plugins enable messages","name":"messages","reason":"this host has no Full Disk Access; turn it on for /Applications/Plow Latch.app in System Settings (x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles), then run `latch plugins enable messages` again","state":"needs_setup"},{"enable":"latch plugins enable contacts","name":"contacts","reason":"its owner has not enabled it on this host","state":"disabled"}]"#
    /// pi-kitchen's own `status --local`, as `status --host pi-kitchen` gives it.
    static let piStatus = #"{"login":{"kind":"device"},"this_machine":{"name":"pi-kitchen","key":"78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc","hosting":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":811,"blocked":null}},"update":{"available":false,"build":138,"checked_at":"2026-10-23T08:00:00Z"},"last_error":null,"state_dir":"/home/pi/.latch","version":"0.1.0 (138): commit 4c1e0a2b9, built 2026-10-22 18:04 UTC, standalone, protocol plow/latch/4","protocol":4}"#
    static let piRules = #"{"agents":[],"audit_id":"c57f1209db34c3767c262960f596b34ec61e8b932a2b3011b3677db297f74bea","every_agent":{"revision":"8419fd077d6e4e3adc59789beedadb9864635746da5e22ae63b727c9b42c8715","text":"This Pi runs the kitchen display. Agents may read ~/recipes and write only to ~/recipes/drafts."},"host":"pi-kitchen","warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}"#
    /// `status` with `plugins` in it, as `status --local` lists them.
    static func status(_ status: String, plugins: String) -> String {
        "{\"plugins\":\(plugins)," + status.dropFirst()
    }
    static let piPlugins = #"[{"enable":"latch plugins enable google","name":"google","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable messages","name":"messages","reason":"its owner has not enabled it on this host","state":"disabled"},{"enable":"latch plugins enable contacts","name":"contacts","reason":"its owner has not enabled it on this host","state":"disabled"}]"#
    static let piVault = #"{"audit_id":"1e059a4e60011ac425fc08b23c8c43e6de7a4db7a571fe8fcce7881388c9f00e","items":[]}"#
    // `vault list`, `vault show` and `vault import --preview` on vstudio, and
    // the import of a Chrome export's first row: real runs.
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
    // An import's preview of a Chrome export (one row updating a password)
    // and that import, at the preview's `expect`: real runs.
    static let importPreview = #"{"preview":{"expect":"20b372fbab51d631bac7849e16bd8e97fa8545e1df8a297e3f458f995437038a","items":[{"changed":["password"],"duplicate":false,"has_password":true,"has_totp":false,"kind":"login","pick":1,"title":"shop.example","url":"https://shop.example/login","username":"ada","warnings":[]},{"changed":[],"duplicate":false,"has_password":true,"has_totp":false,"kind":"login","pick":2,"title":"grocer.example","url":"https://grocer.example","username":"ada@example.com","warnings":[]}],"skipped":[{"reason":"has no website address; add it by hand as a new item if you still use it","title":"nosite"}],"source":"Chrome","vaults":[]}}"#
    static let importResult = #"{"audit_id":"cef0158ca075e6aa3d62b3072bab64a7c7c6e04d82e251ea471306f3929ee12d","duplicates":0,"failed":[],"picked":[1,2],"saved":1,"source":"Chrome","updated":1}"#
    // `latch audit` on studio and, from laptop, `latch audit --host pi-kitchen`:
    // real runs, with names and home folders shortened, and each record's
    // `command` as latch now prints it.
    static let studioAudit = #"""
{"schema":1,"type":"hello"}
{"cursor":"0000000000000000000000000000000000000000000000000000000000000000","has_older":false,"type":"snapshot"}
{"cursor":"c30c5f19c96e7339175054f21735bff913330ad55e3a59820062ea3031e5ce86","record":{"agent":null,"caller":"6b55a78f","caller_name":"studio","decision":"allow","details":{"args":{"after":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c","agent_uid":null,"before":null,"command":"set","scope":"every agent"},"gate":null,"started":null},"id":"c30c5f19c96e7339175054f21735bff913330ad55e3a59820062ea3031e5ce86","op":"rules","outcome":"ok","time":"2026-10-23T04:57:17.443592Z","command":"latch rules set"},"type":"record"}
{"cursor":"c979a3eeb29cc2d8a2b265b030b35e2d1b87455b2fcf098c0345c8618f6944dc","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"c979a3eeb29cc2d8a2b265b030b35e2d1b87455b2fcf098c0345c8618f6944dc","op":"connect","outcome":"connected","time":"2026-10-23T04:57:25.011137Z","command":null},"type":"record"}
{"cursor":"6340de838b0f5a8037872c6c931e34bffb3925247f970736b518b7b6546ef6db","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":24,"exit_code":1,"network":false,"secret_env":[],"signal":null,"stderr_bytes":40,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":313,"model":"typesafe/jev-1.14-20261014","model_request":"req_clvmzkwubekyhohm","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:25.059946Z"},"id":"6340de838b0f5a8037872c6c931e34bffb3925247f970736b518b7b6546ef6db","op":"exec","outcome":"ok","time":"2026-10-23T04:57:25.406595Z","command":"latch exec ls Projects"},"type":"record"}
{"cursor":"bc5c3f4e741ea9dbc17f448c97957a9b73d7bc60df9dfcbb688ee75a629badc3","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"bc5c3f4e741ea9dbc17f448c97957a9b73d7bc60df9dfcbb688ee75a629badc3","op":"connect","outcome":"connected","time":"2026-10-23T04:57:25.82139Z","command":null},"type":"record"}
{"cursor":"26ce20c4e91c5ecac785db00f672f07e6e631b547677cae5c382ba9820cbe54c","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":{"argv":["rm","-rf","Finance"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":278,"model":"typesafe/jev-1.14-20261014","model_request":"req_bexwvhrmekgdazuq","p":{"COVERED":0.18,"FORBIDDEN":0.82,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"owner_said_no"},"started":"2026-10-23T04:57:25.83365Z"},"id":"26ce20c4e91c5ecac785db00f672f07e6e631b547677cae5c382ba9820cbe54c","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:57:26.112307Z","command":"latch exec rm -rf Finance"},"type":"record"}
{"cursor":"4e23cb5b48d536704e495b9d29d4cb9b162330ec60dbfaf236a31667bd9be033","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"4e23cb5b48d536704e495b9d29d4cb9b162330ec60dbfaf236a31667bd9be033","op":"connect","outcome":"connected","time":"2026-10-23T04:57:26.50193Z","command":null},"type":"record"}
{"cursor":"7f7c8b8278358cab8fc81abc4d14ae90d8c9cefa599574d411a65184f4b3b59f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["curl","https://example.com"],"duration_ms":45,"exit_code":6,"network":false,"secret_env":[],"signal":null,"stderr_bytes":283,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":164,"model":"typesafe/jev-1.14-20261014","model_request":"req_pcprjkoskeurcceu","p":{"COVERED":0.87,"FORBIDDEN":0.09,"NOT_COVERED":0.04},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:26.533038Z"},"id":"7f7c8b8278358cab8fc81abc4d14ae90d8c9cefa599574d411a65184f4b3b59f","op":"exec","outcome":"ok","time":"2026-10-23T04:57:26.743828Z","command":"latch exec curl https://example.com"},"type":"record"}
{"cursor":"d44d2a3920ab63c74ae9f70dc9b00491556992ef712ff66239a57cacdfa7cf6f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"d44d2a3920ab63c74ae9f70dc9b00491556992ef712ff66239a57cacdfa7cf6f","op":"connect","outcome":"connected","time":"2026-10-23T04:57:51.903676Z","command":null},"type":"record"}
{"cursor":"bdc143c43361be60435e26ebf7b9eca7c5774335a27152279f049d39aa5b24f8","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":18,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":303,"model":"typesafe/jev-1.14-20261014","model_request":"req_mhkhqxhakqjfnmqr","p":{"COVERED":0.91,"FORBIDDEN":0.07,"NOT_COVERED":0.02},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:51.956162Z"},"id":"bdc143c43361be60435e26ebf7b9eca7c5774335a27152279f049d39aa5b24f8","op":"exec","outcome":"ok","time":"2026-10-23T04:57:52.278366Z","command":"latch exec ls Projects"},"type":"record"}
{"cursor":"e38f38d8a451677c09e82eb84c6e8494f5b0e33a9d2c4636696393489b6caab8","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"e38f38d8a451677c09e82eb84c6e8494f5b0e33a9d2c4636696393489b6caab8","op":"connect","outcome":"connected","time":"2026-10-23T04:57:52.732852Z","command":null},"type":"record"}
{"cursor":"e2c90eb59636a3339cd0df42518405c3610ea5f89ffff272591830bb249ad5c0","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["cat","Projects/latch-demo/README.md"],"duration_ms":19,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.2e-05,"latency_ms":180,"model":"typesafe/jev-1.14-20261014","model_request":"req_zkxautxbszsppfdz","p":{"COVERED":0.97,"FORBIDDEN":0.03,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:52.781233Z"},"id":"e2c90eb59636a3339cd0df42518405c3610ea5f89ffff272591830bb249ad5c0","op":"exec","outcome":"ok","time":"2026-10-23T04:57:52.981672Z","command":"latch exec cat Projects/latch-demo/README.md"},"type":"record"}
{"cursor":"d40383d218b037fd86c7f97184cbc5fc6420355acfbb0a14d5583847a1a3161c","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"d40383d218b037fd86c7f97184cbc5fc6420355acfbb0a14d5583847a1a3161c","op":"connect","outcome":"connected","time":"2026-10-23T04:57:53.426433Z","command":null},"type":"record"}
{"cursor":"93a9614ca6028415ac52f3cee6f959daebd09ba0b10abeef00989ef7c394997f","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":{"argv":["cat","Finance/2026.csv"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":214,"model":"typesafe/jev-1.14-20261014","model_request":"req_mzzdawsrcgoaluii","p":{"COVERED":0.34,"FORBIDDEN":0.66,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"owner_said_no"},"started":"2026-10-23T04:57:53.466317Z"},"id":"93a9614ca6028415ac52f3cee6f959daebd09ba0b10abeef00989ef7c394997f","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:57:53.681028Z","command":"latch exec cat Finance/2026.csv"},"type":"record"}
{"cursor":"8dfb79662f4b3aefa6bf291451ef0dd37d5ed38a5206c990b85caf0610475fde","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"8dfb79662f4b3aefa6bf291451ef0dd37d5ed38a5206c990b85caf0610475fde","op":"connect","outcome":"connected","time":"2026-10-23T04:57:54.097018Z","command":null},"type":"record"}
{"cursor":"84c65cf8b063d5adfdc7e4ef045d0c204723ac0145342781702700a82917431b","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["uname","-a"],"duration_ms":18,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":186,"model":"typesafe/jev-1.14-20261014","model_request":"req_hdxckffgexqhxskg","p":{"COVERED":0.99,"FORBIDDEN":0.01,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"owner_request","verdict":"allow"},"started":"2026-10-23T04:57:54.122272Z"},"id":"84c65cf8b063d5adfdc7e4ef045d0c204723ac0145342781702700a82917431b","op":"exec","outcome":"ok","time":"2026-10-23T04:57:54.328652Z","command":"latch exec uname -a"},"type":"record"}
{"cursor":"7c3c1c7e6558c03a3c4de0d5efff3ad2a0afbcfd994481968ac2ad6859b7cd7e","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"7c3c1c7e6558c03a3c4de0d5efff3ad2a0afbcfd994481968ac2ad6859b7cd7e","op":"connect","outcome":"connected","time":"2026-10-23T04:57:54.695349Z","command":null},"type":"record"}
{"cursor":"4dd3f640612fb52093107aadafa86c014a39f00c77b386041d0f88d4a31ab814","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"deny","details":{"args":null,"gate":null,"started":"2026-10-23T04:57:54.719856Z"},"id":"4dd3f640612fb52093107aadafa86c014a39f00c77b386041d0f88d4a31ab814","op":"google","outcome":"not_enabled","time":"2026-10-23T04:57:54.71992Z","command":"latch google unrecognized"},"type":"record"}
{"cursor":"fa1629671e2b00ff12cd7a6c97b386dd6df5caec50ed83e22c03fccb0fc710b8","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"fa1629671e2b00ff12cd7a6c97b386dd6df5caec50ed83e22c03fccb0fc710b8","op":"connect","outcome":"connected","time":"2026-10-23T04:58:04.519463Z","command":null},"type":"record"}
{"cursor":"7b22c2ecbeb978b5ae0802dc6dd6c56d87d14c7d891c53d121dccb85d929a669","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"deny","details":{"args":{"argv":["ls","Projects"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","cost":1e-05,"latency_ms":416,"model":"typesafe/jev-1.14-20261014","model_request":"req_pjacfdrynfkuwigb","p":{"COVERED":0.44,"FORBIDDEN":0.0,"NOT_COVERED":0.56},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"not_covered"},"started":"2026-10-23T04:58:04.541515Z"},"id":"7b22c2ecbeb978b5ae0802dc6dd6c56d87d14c7d891c53d121dccb85d929a669","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:58:04.967315Z","command":"latch exec ls Projects"},"type":"record"}
{"cursor":"1f8656f30d7ad55e5bbe4ffc9ad1e915a6848c4ba0b15f613951794f9c4f5456","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"1f8656f30d7ad55e5bbe4ffc9ad1e915a6848c4ba0b15f613951794f9c4f5456","op":"connect","outcome":"connected","time":"2026-10-23T04:58:05.362647Z","command":null},"type":"record"}
{"cursor":"0cb70259b986c3f4a06dad3e850c70bb3791f2e5ab4371517fd2658b2b8573a2","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"deny","details":{"args":{"argv":["cat","Finance/2026.csv"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","cost":1e-05,"latency_ms":286,"model":"typesafe/jev-1.14-20261014","model_request":"req_xcprcsejhfkehxgp","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"owner_said_no"},"started":"2026-10-23T04:58:05.400589Z"},"id":"0cb70259b986c3f4a06dad3e850c70bb3791f2e5ab4371517fd2658b2b8573a2","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T04:58:05.688109Z","command":"latch exec cat Finance/2026.csv"},"type":"record"}
{"cursor":"8f79bb19999f991d95fa4d5f40a8105d5a781460010c3338db102f95a60928fc","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"8f79bb19999f991d95fa4d5f40a8105d5a781460010c3338db102f95a60928fc","op":"connect","outcome":"connected","time":"2026-10-23T04:58:17.143652Z","command":null},"type":"record"}
{"cursor":"f85509e49fcc611c3f36f66b4cbabaf6a396e6aa4e332ea0c51c951aaae3635f","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":{"argv":["ls","Projects"],"duration_ms":28,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","chat":"cht_RlfbPCRJOw5jubw4FJ4nqw","cost":1.1e-05,"latency_ms":505,"model":"typesafe/jev-1.14-20261014","model_request":"req_hzjunxllqsenomkr","owner_messages":["msg_pzYDNrGQn0K2KzEIbRNvIg"],"p":{"COVERED":0.87,"FORBIDDEN":0.0,"NOT_COVERED":0.13},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"chat_and_rules","verdict":"allow","window":{"count":1,"first":"msg_pzYDNrGQn0K2KzEIbRNvIg","last":"msg_pzYDNrGQn0K2KzEIbRNvIg","truncated":false}},"started":"2026-10-23T04:58:17.190603Z"},"id":"f85509e49fcc611c3f36f66b4cbabaf6a396e6aa4e332ea0c51c951aaae3635f","op":"exec","outcome":"ok","time":"2026-10-23T04:58:17.738563Z","command":"latch exec ls Projects"},"type":"record"}
{"cursor":"2b5df2550f8a2afbf4abf629b92bdaf2fd6985313989e80be3464f8cdf8dbd3a","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"2b5df2550f8a2afbf4abf629b92bdaf2fd6985313989e80be3464f8cdf8dbd3a","op":"connect","outcome":"connected","time":"2026-10-23T04:58:18.208986Z","command":null},"type":"record"}
{"cursor":"5e49f93c5f5f60d2ab1b2e49d4552403bd143e1e8bc95917096631b8ca00cab7","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":"scout","decision":"allow","details":{"args":{"argv":["cat","Projects/latch-demo/README.md"],"duration_ms":95,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","chat":"cht_RlfbPCRJOw5jubw4FJ4nqw","cost":1.1e-05,"latency_ms":416,"model":"typesafe/jev-1.14-20261014","model_request":"req_lcjvkoufkvslpbuy","owner_messages":["msg_pzYDNrGQn0K2KzEIbRNvIg"],"p":{"COVERED":0.97,"FORBIDDEN":0.0,"NOT_COVERED":0.03},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"chat_and_rules","verdict":"allow","window":{"count":1,"first":"msg_pzYDNrGQn0K2KzEIbRNvIg","last":"msg_pzYDNrGQn0K2KzEIbRNvIg","truncated":false}},"started":"2026-10-23T04:58:18.273433Z"},"id":"5e49f93c5f5f60d2ab1b2e49d4552403bd143e1e8bc95917096631b8ca00cab7","op":"exec","outcome":"ok","time":"2026-10-23T04:58:18.787025Z","command":"latch exec cat Projects/latch-demo/README.md"},"type":"record"}
{"cursor":"6d94c96d9b41d92efb792aceb3eff19c222ddfedcf737eb9855b21bd2ea90edb","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"6d94c96d9b41d92efb792aceb3eff19c222ddfedcf737eb9855b21bd2ea90edb","op":"connect","outcome":"connected","time":"2026-10-23T05:00:04.753334Z","command":null},"type":"record"}
{"cursor":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","record":{"agent":"a74f38668570a45ddc4b762ba0465d74","caller":"9f4d06180a5fa0824b2bc4fc4f056b0395e90914cedf7709621e6559047263f2","caller_name":null,"decision":"deny","details":{"args":{"argv":["ls","Projects"],"network":false,"secret_env":[],"workdir":null,"writes":[]},"gate":null,"started":"2026-10-23T05:00:04.784896Z"},"id":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","op":"exec","outcome":"exec_denied","time":"2026-10-23T05:00:04.785546Z","command":"latch exec ls Projects"},"type":"record"}
{"cursor":"afe395811c3842b422aba81f8926d00eeb808b7f11362e5ffc949b2ed9c27717","has_older":false,"type":"caught_up"}
"""#
    static let kitchenAudit = #"""
{"schema":1,"type":"hello"}
{"cursor":"0000000000000000000000000000000000000000000000000000000000000000","has_older":false,"type":"snapshot"}
{"cursor":"3847ada0b814d138df5111d0558eb8398095a55d302dbaa0003bdfe62b6af1a8","record":{"agent":"edad6227281f8c39c81b4b514030c0d6","caller":"c14846ee052eaabf0df311c7bd255449dfa1c7176cc889807be13aa0f5f094bb","caller_name":null,"decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"3847ada0b814d138df5111d0558eb8398095a55d302dbaa0003bdfe62b6af1a8","op":"connect","outcome":"connected","time":"2026-10-23T05:00:22.20038Z","command":null},"type":"record"}
{"cursor":"6caf882d14b2d942a136cb133de5270980e5f27adb7a4dcb533f469ce4e4cf57","record":{"agent":"edad6227281f8c39c81b4b514030c0d6","caller":"c14846ee052eaabf0df311c7bd255449dfa1c7176cc889807be13aa0f5f094bb","caller_name":null,"decision":"deny","details":{"args":{"argv":["rm","recipes/old.txt"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]},"gate":{"caller":"agent","cost":1.1e-05,"decided_by":"rules","latency_ms":197,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_acdevaoxexjochck","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"prompt":"7f3a7dc2c0d23b04","says":"neither"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":null,"source":"defaults","verdict":"not_covered"},"started":"2026-10-23T05:00:22.234276Z"},"id":"6caf882d14b2d942a136cb133de5270980e5f27adb7a4dcb533f469ce4e4cf57","op":"exec","outcome":"gatekeeper_denied","time":"2026-10-23T05:00:22.248376Z","command":"latch exec rm recipes/old.txt"},"type":"record"}
{"cursor":"71aab5d260d64b64e45df118744f0c87e0aabb467e624082e8d89f3310ad1d41","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"71aab5d260d64b64e45df118744f0c87e0aabb467e624082e8d89f3310ad1d41","op":"connect","outcome":"connected","time":"2026-10-23T05:00:22.628448Z","command":null},"type":"record"}
{"cursor":"246ce126dd89a3787078679404920a14da2ca46049539627a62a7a6149964d08","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":{"argv":["ls","recipes"],"duration_ms":25,"exit_code":0,"network":false,"secret_env":[],"signal":null,"stderr_bytes":0,"timed_out":false,"timeout":60,"workdir":"/Users/me","writes":[]},"gate":{"caller":"owner_device","cost":1.1e-05,"latency_ms":336,"model":"typesafe/jev-1.14-20261014","model_request":"req_pmhxsyrhlbigqtal","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"rule":"refused when forbidden >= within instructions","source":"owner_request","verdict":"allow"},"started":"2026-10-23T05:00:22.657485Z"},"id":"246ce126dd89a3787078679404920a14da2ca46049539627a62a7a6149964d08","op":"exec","outcome":"ok","time":"2026-10-23T05:00:23.019881Z","command":"latch exec ls recipes"},"type":"record"}
{"cursor":"3f939488fb20e4e23cc4865e9bf21b646e7425c09724a5e1585930a71e64af37","record":{"agent":null,"caller":"beded256c96e63e4c9af3aef238e3c4aad0c6bfa88312f8b77f107f2449788d9","caller_name":"laptop","decision":"allow","details":{"args":null,"gate":null,"started":null},"id":"3f939488fb20e4e23cc4865e9bf21b646e7425c09724a5e1585930a71e64af37","op":"connect","outcome":"connected","time":"2026-10-23T05:00:35.861887Z","command":null},"type":"record"}
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
        "defaults": #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":197,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_acdevaoxexjochck","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"prompt":"7f3a7dc2c0d23b04","says":"neither"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.05,"FORBIDDEN":0.03,"NOT_COVERED":0.92},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":null,"source":"defaults","verdict":"not_covered"},"host":"pi-kitchen","request":{"action":{"argv":["rm","recipes/old.txt"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"}"#,
        "host-refused": #"{"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":null,"host":"pi-kitchen","refused":{"code":"exec_denied","message":"You turned exec off for this agent on this host when you started it with `latch serve --deny-exec=3971582e59baad83250141c8dcc95855`. Start it again without that flag to turn exec back on."},"request":{"action":{"argv":["ls"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"host_refused"}"#,
        "new-agent": #"{"as":"new_agent","gate":{"caller":"new_agent","cost":0.00001,"latency_ms":212,"model":"typesafe/jev-1.14-20261014","model_request":"req_nnlzesajgklnyqbg","p":{"COVERED":0.58,"FORBIDDEN":0.01,"NOT_COVERED":0.41},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"ea4ac6c6bd9184d7479f8312f4ebca18db1804f21bba03212ac8f0dc80f35c7c"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["ls","Projects"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"allow"}"#,
        "draft": #"{"as":{"agent_name":"helper","agent_uid":"3971582e59baad83250141c8dcc95855"},"gate":{"caller":"agent","cost":0.00001,"latency_ms":242,"model":"typesafe/jev-1.14-20261014","model_request":"req_hplzksgpinmlheze","p":{"COVERED":0.21,"FORBIDDEN":0.0,"NOT_COVERED":0.79},"rule":"allowed when within instructions >= 0.50","rules":{"agent":null,"host":"7d8f456802c7a988d09fc874e41d23543e9b8e2d810ab5c7d110757f193a90ff"},"source":"rules","verdict":"not_covered"},"host":"studio","request":{"action":{"argv":["touch","Projects/x"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me","writes":[]},"op":"exec"},"verdict":"not_covered"}"#,
    ]
    // `rules try --new-agent --draft -` with each default's rules, on its
    // five examples; the host renamed and the home folder shortened.
    static let personalTries = [
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":293,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_jurayjfejrkgatyn","p":{"COVERED":0.99,"FORBIDDEN":0.01,"NOT_COVERED":0.0},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.99,"FORBIDDEN":0.01,"NOT_COVERED":0.0},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"e045533eebf3b2483cbe70c84e1653f0f3713325e1ec6b827aace8c2fe9a5140"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"accounts":[],"command":["calendar","events","primary","--from","2026-10-26","--to","2026-11-02"],"operation":"google"},"op":"google"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":177,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_fjhinappbkvbzujr","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":1.0,"FORBIDDEN":0.0,"NOT_COVERED":0.0},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"e045533eebf3b2483cbe70c84e1653f0f3713325e1ec6b827aace8c2fe9a5140"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"arguments":{"command":"search","limit":50,"oldest_first":false,"phrase":"dinner"},"operation":"messages.search"},"op":"messages"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.2e-05,"decided_by":"rules","latency_ms":189,"layers":{"rules":{"cost":1.2e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_jpxxuucgfzscbyeu","p":{"COVERED":0.89,"FORBIDDEN":0.09,"NOT_COVERED":0.02},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.89,"FORBIDDEN":0.09,"NOT_COVERED":0.02},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"e045533eebf3b2483cbe70c84e1653f0f3713325e1ec6b827aace8c2fe9a5140"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["touch","shopping-list.txt"],"network":false,"operation":"exec","secret_env":[],"working_folder":"/Users/me/Documents","writes":["/Users/me/Documents"]},"op":"exec"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":null,"host":"studio","refused":{"code":"bad_path","message":"the path must be inside the host's home and outside hidden state; home itself cannot be a write target"},"request":{"action":{"argv":["npm","install"],"network":true,"operation":"exec","secret_env":[],"writes":["/"]},"op":"exec"},"verdict":"host_refused"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":202,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_uyabmrjhiduvthmx","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"prompt":"7f3a7dc2c0d23b04","says":"veto"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.0,"FORBIDDEN":0.99,"NOT_COVERED":0.01},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"e045533eebf3b2483cbe70c84e1653f0f3713325e1ec6b827aace8c2fe9a5140"},"source":"rules","verdict":"owner_said_no"},"host":"studio","request":{"action":{"accounts":[],"command":["gmail","send","--to","jordan@example.com","--subject","Update","--body","Ready for review."],"operation":"google"},"op":"google"},"verdict":"owner_said_no"}"#,
    ]
    static let executiveTries = [
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1e-05,"decided_by":"rules","latency_ms":169,"layers":{"rules":{"cost":1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_tnrxggbeogdfselj","p":{"COVERED":0.95,"FORBIDDEN":0.04,"NOT_COVERED":0.01},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.95,"FORBIDDEN":0.04,"NOT_COVERED":0.01},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"ef4a3f2d7ea60c863e5fcd69f75bce1be0c68e0d4ce4b070ff83137005ef1eb1"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"accounts":[],"command":["gmail","search","is:unread"],"operation":"google"},"op":"google"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":170,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_wccbygutiiuaxnvs","p":{"COVERED":0.89,"FORBIDDEN":0.01,"NOT_COVERED":0.1},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.89,"FORBIDDEN":0.01,"NOT_COVERED":0.1},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"ef4a3f2d7ea60c863e5fcd69f75bce1be0c68e0d4ce4b070ff83137005ef1eb1"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"accounts":[],"command":["gmail","send","--to","jordan@example.com","--subject","Update","--body","Ready for review."],"operation":"google"},"op":"google"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":176,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_npqgsrjnpbogocbl","p":{"COVERED":0.98,"FORBIDDEN":0.01,"NOT_COVERED":0.01},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.98,"FORBIDDEN":0.01,"NOT_COVERED":0.01},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"ef4a3f2d7ea60c863e5fcd69f75bce1be0c68e0d4ce4b070ff83137005ef1eb1"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"accounts":[],"command":["calendar","freebusy","primary","--from","2026-10-26","--to","2026-11-02"],"operation":"google"},"op":"google"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.2e-05,"decided_by":"rules","latency_ms":185,"layers":{"rules":{"cost":1.2e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_amqgxvktyrvcnmnl","p":{"COVERED":0.96,"FORBIDDEN":0.01,"NOT_COVERED":0.03},"prompt":"7f3a7dc2c0d23b04","says":"allow"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.96,"FORBIDDEN":0.01,"NOT_COVERED":0.03},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"ef4a3f2d7ea60c863e5fcd69f75bce1be0c68e0d4ce4b070ff83137005ef1eb1"},"source":"rules","verdict":"allow"},"host":"studio","request":{"action":{"argv":["npm","install"],"network":true,"operation":"exec","secret_env":[],"working_folder":"/Users/me/work","writes":["/Users/me/work"]},"op":"exec"},"verdict":"allow"}"#,
        #"{"as":"new_agent","gate":{"caller":"new_agent","cost":1.1e-05,"decided_by":"rules","latency_ms":312,"layers":{"rules":{"cost":1.1e-05,"model":"typesafe/jev-1.14-20261014","model_request":"req_lujqesxgiqtmowdy","p":{"COVERED":0.0,"FORBIDDEN":1.0,"NOT_COVERED":0.0},"prompt":"7f3a7dc2c0d23b04","says":"veto"}},"model":"typesafe/jev-1.14-20261014","p":{"COVERED":0.0,"FORBIDDEN":1.0,"NOT_COVERED":0.0},"rule":"refused when a layer forbids; else allowed when a layer is within instructions >= 0.50","rules":{"agent":null,"host":"ef4a3f2d7ea60c863e5fcd69f75bce1be0c68e0d4ce4b070ff83137005ef1eb1"},"source":"rules","verdict":"owner_said_no"},"host":"studio","request":{"action":{"arguments":{"command":"search","limit":50,"oldest_first":false,"phrase":"dinner"},"operation":"messages.search"},"op":"messages"},"verdict":"owner_said_no"}"#,
    ]
    static let code = #"{"type":"code","code":"JFO3S","send_to":"+15550000001","send_to_display":"+1 555-000-0001","smsto":"SMSTO:+15550000001:JFO3S","sms_link":"sms:+15550000001?&body=JFO3S","expires_at":null}"#
    static let hostRefused = #"{"verdict":"host_refused","host":"studio","as":"owner","request":{"op":"messages","action":{}},"gate":null,"refused":{"code":"plugin_disabled","message":"the messages connector is off on this host"}}"#

    static func decode<T: Decodable>(_ json: String) throws -> T {
        try LatchJSON.decoder.decode(T.self, from: Data(json.utf8))
    }
}

/// Renders every onboarding screen and state, and the main window, in
/// light and dark, on the fake latch: `swift run Snapshots OUTPUT_DIR`.
/// Each view draws its own layers, so no screen-recording permission.
package enum SnapshotRunner {
    /// Name prefixes of the stills to draw; empty draws them all.
    private static var only: [String] = []

    private static func wanted(_ file: URL) -> Bool {
        only.isEmpty || only.contains { file.lastPathComponent.hasPrefix($0) }
    }

    package static func run(into output: URL, only: [String] = []) async throws {
        self.only = only
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let code: LoginCode = try Fixtures.decode(Fixtures.code)
        let rules = "Agents may read and summarize anything in ~/Projects. Ask me before sending any email or deleting files. Never touch ~/Finance."
        let skipped = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .skipped,
                                        .access: .skipped, .start: .continued])
        let full = Progress(passed: [.welcome: .continued, .login: .continued, .privacy: .continued, .rules: .continued,
                                     .access: .continued, .start: .continued])

        typealias Stage = (Onboarding) async -> Void
        let states: [(String, Step, String, Progress, Stage)] = [
            ("01-welcome", .welcome, Fixtures.loggedOut, Progress(), { _ in }),
            ("02-login-starting", .login, Fixtures.loggedOut, Progress(), { $0.login = .starting }),
            ("02-login-waiting", .login, Fixtures.loggedOut, Progress(), { $0.login = .waiting(code) }),
            ("02-login-expired", .login, Fixtures.loggedOut, Progress(), { $0.login = .expired(code) }),
            ("02-login-error", .login, Fixtures.loggedOut, Progress(), {
                $0.login = .failed(LatchError.failed(LatchFailure(code: "plow_unreachable", message: "could not reach Plow at http://127.0.0.1:18934: error sending request for url (http://127.0.0.1:18934/v1/latch/login)")))
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
            ("04-rules-personal-verdicts", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[0]
                $0.deck = deck; $0.draftRules = deck.rules
                for (example, answer) in zip(deck.examples, Fixtures.personalTries) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.macLit = true
            }),
            ("04-rules-executive-verdicts-open", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[1]
                $0.deck = deck; $0.draftRules = deck.rules
                for (example, answer) in zip(deck.examples, Fixtures.executiveTries) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.openExamples = [deck.examples[4].id]
            }),
            ("04-rules-personal-host-refused-open", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[0]
                $0.deck = deck; $0.draftRules = deck.rules
                for (example, answer) in zip(deck.examples, Fixtures.personalTries) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                $0.openExamples = [deck.examples[3].id]
            }),
            ("04-rules-no-answer-open", .rules, Fixtures.serving, Progress(), {
                let deck = Onboarding.decks[1]
                $0.deck = deck; $0.draftRules = deck.rules
                for (example, answer) in zip(deck.examples, Fixtures.executiveTries).prefix(2) { $0.previews[example.id] = .verdict(try! Fixtures.decode(answer)) }
                for example in deck.examples.dropFirst(2) { $0.previews[example.id] = .noAnswer }
                $0.openExamples = [deck.examples[3].id]
            }),
            ("04-rules-save-failed", .rules, Fixtures.stopped, Progress(), {
                $0.draftRules = rules
                $0.rulesError = LatchError.failed(LatchFailure(code: "rules_too_long", message: "rules are at most 8 KB together; shorten them, then save again"))
            }),
            ("05-grant-google-checking", .access, Fixtures.stopped, Progress(), { _ in }),
            ("05-grant-google-not-connected", .access, Fixtures.stopped, Progress(), { await $0.checkGoogle() }),
            ("05-grant-google-waiting", .access, Fixtures.stopped, Progress(), {
                await $0.checkGoogle(); $0.google = .waiting; $0.stage(googleSignInLink: Fixtures.signInLink)
            }),
            ("05-grant-google-wait-ended", .access, Fixtures.stopped, Progress(), {
                await $0.checkGoogle(); $0.googleWaitEnded = GoogleSignIn.problem(.failed(LatchFailure(code: "sign_in_timeout", message: "")), adding: false)
            }),
            ("05-grant-google-connected-turned-on", .access, Fixtures.stopped, Progress(), { await $0.checkGoogle() }),
            ("05-grant-google-three-accounts", .access, Fixtures.stopped, Progress(), { await $0.checkGoogle() }),
            ("05-grant-google-adding-waiting", .access, Fixtures.stopped, Progress(), {
                await $0.checkGoogle(); $0.google = .waiting; $0.stage(googleSignInLink: Fixtures.signInLink)
            }),
            ("05-grant-google-adding-timed-out", .access, Fixtures.stopped, Progress(), {
                await $0.checkGoogle(); $0.googleWaitEnded = GoogleSignIn.problem(.failed(LatchFailure(code: "sign_in_timeout", message: "")), adding: true)
            }),
            ("05-grant-google-some-need-sign-in", .access, Fixtures.stopped, Progress(), { await $0.checkGoogle() }),
            ("05-grant-google-all-need-sign-in", .access, Fixtures.stopped, Progress(), {
                $0.googleAccounts = GoogleAccounts(accounts: Fixtures.threeAccounts, needsSignIn: Fixtures.threeAccounts); $0.google = .notConnected
            }),
            ("05-grant-google-rerun-connected", .access, Fixtures.serving, Progress(rerun: true), { await $0.checkGoogle() }),
            ("05-grant-google-plow-unreachable", .access, Fixtures.stopped, Progress(), { await $0.checkGoogle() }),
            ("05-grant-google-tools-missing", .access, Fixtures.stopped, Progress(), { $0.google = .failed(Onboarding.toolsMissing) }),
            ("05-grant-access-not-checked", .access, Fixtures.stopped, Progress(), { _ in }),
            ("05-grant-access-waiting", .access, Fixtures.stopped, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access"); $0.waitingForAccess = true
            }),
            ("05-grant-access-allowed", .access, Fixtures.stopped, Progress(), {
                $0.messages = .done; $0.contacts = .done
            }),
            ("05-grant-access-restart-latch", .access, Fixtures.serving, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
            }),
            ("05-grant-access-restart-failed", .access, Fixtures.stopped, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
                $0.restartError = LatchError.failed(LatchFailure(code: "serve_failed", message: "the host did not answer within 30 seconds (`latch logs` has its log)"))
            }),
            ("05-grant-access-terminal-host", .access, Fixtures.servingAttached, Progress(), {
                $0.messages = .needed("Waiting for access"); $0.contacts = .needed("Waiting for access")
            }),
            ("05-grant-access-unavailable", .access, Fixtures.stopped, Progress(), {
                $0.messages = .failed(LatchError.failed(LatchFailure(code: "messages_not_installed", message: "the Messages reader is not installed at /Users/me/.latch/plugins/messages/0.1.0/latch-messages; run install.sh")))
                $0.contacts = .done
            }),
            ("07-start", .start, Fixtures.stopped, Progress(), { _ in }),
            ("07-start-background-off", .start, Fixtures.stopped, Progress(), { $0.runInBackground = false }),
            ("07-start-already-running", .start, Fixtures.serving, Progress(), { _ in }),
            ("07-start-other-version", .start, Fixtures.otherVersion, Progress(), { _ in }),
            ("07-start-error", .start, Fixtures.stopped, Progress(), {
                $0.startError = LatchError.failed(LatchFailure(code: "serve_failed", message: "the host did not answer within 30 seconds (`latch logs` has its log)"))
            }),
            ("07-start-login-item-needs-approval", .start, Fixtures.serving, Progress(), { $0.startError = LoginItem.needsApproval }),
            ("08-done-all-set", .done, Fixtures.serving, full, {
                $0.app.stage(rulesSaved: true, setup: try! Fixtures.decode(Fixtures.setup(on: ["google", "messages", "contacts"])))
            }),
            ("08-done-text-agent", .done, Fixtures.serving, full, {
                $0.app.stage(rulesSaved: true, setup: try! Fixtures.decode(Fixtures.setup(on: ["google", "messages", "contacts"])))
                $0.app.stage(agents: try! Fixtures.decode(Fixtures.agents), standings: [:])
            }),
            ("08-done-google-skipped", .done, Fixtures.serving,
             Progress(passed: full.passed.merging([.access: .skipped]) { $1 }), {
                $0.app.stage(rulesSaved: true, setup: try! Fixtures.decode(Fixtures.setup(on: ["messages", "contacts"], google: Fixtures.googleNotConnected)))
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
                let app = AppModel(client: FakeLatchClient(status: status, google: Self.google[name]), still: true)
                app.stage(status: try Fixtures.decode(status))
                let flow = app.stage(step, progress: progress)
                await stage(flow)
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
                          error: status == nil ? .unreachable("Latch couldn't start.", detail: "The file “latch” doesn't exist.") : nil)
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
                app.stage(status: try Fixtures.decode(Fixtures.status(status, plugins: Fixtures.studioPlugins)))
                app.stage(account: try account.map { try Fixtures.decode($0) }, error: account == nil ? plowDown : nil)
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.studioRules)),
                                               vault: .success(try Fixtures.decode(Fixtures.studioVault))),
                          for: "studio")
                app.stage(summary: HostSummary(status: .success(try Fixtures.decode(Fixtures.status(Fixtures.piStatus, plugins: Fixtures.piPlugins))),
                                               rules: .success(try Fixtures.decode(Fixtures.piRules)),
                                               vault: .success(try Fixtures.decode(Fixtures.piVault))),
                          for: "pi-kitchen")
                app.chosenHost = chosen
                app.section = section
                stageActivity(app)
                _ = app.stage(.done, progress: full)
                try await renderWindow(MainView().environment(app), size: mainSize, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // latch's errors in the app's words, latch's own under Details:
            // a host out of reach, one stopped while Activity follows it,
            // and a code the app has no sentence for.
            let unreachable = LatchError.failed(LatchFailure(code: "host_unreachable",
                                                             message: #"could not connect to host "pi-kitchen"; nothing was sent"#))
            let stopped = LatchError.failed(LatchFailure(code: "host_stopped", message: #"host "pi-kitchen" stopped serving"#))
            let busy = LatchError.failed(LatchFailure(code: "state_busy", message: "another latch is changing /Users/me/.latch; try again"))
            let errors: [(String, String?, SidebarItem, HostSummary)] = [
                ("23-error-rules-pi-kitchen-unreachable", "pi-kitchen", .rules, HostSummary(rules: .failure(unreachable))),
                ("23-error-activity-pi-kitchen-stopped", "pi-kitchen", .activity, HostSummary()),
                ("23-error-rules-unknown-code", nil, .rules, HostSummary(rules: .failure(busy))),
            ]
            for (name, chosen, section, summary) in errors {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(Fixtures.serving))
                app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
                app.stage(summary: summary, for: chosen ?? "studio")
                app.chosenHost = chosen
                app.section = section
                stageActivity(app)
                if let kitchen = app.hosts.first(where: { $0.name == "pi-kitchen" }) {
                    app.feed(for: kitchen).stage("", error: stopped)
                }
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
                ("14-activity-pi-kitchen-needed-your-ok", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", .all,
                 { $0.gate?.source == "defaults" }),
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
                of: #""self":false,"online":true,"last_seen_at":"2026-10-22T22:06:33.384336Z""#,
                with: #""self":false,"online":false,"last_seen_at":null"#)
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
                ("16-rules-studio-unsaved-draft", .rules, nil, [], { app in
                    let key = AppModel.RulesKey(host: "studio", agent: nil)
                    let saved = app.savedRules(key)
                    app.rulesDrafts[key] = .init(text: (saved?.text ?? "") + "Never send email without asking me.", base: saved?.revision)
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
                of: #""self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z""#,
                with: #""self":false,"online":false,"last_seen_at":null"#)
            let screens: [(String, String?, String, String?, String)] = [
                ("17-vault-login-2fa", nil, Fixtures.vaultItems, "v1", Fixtures.studioHosts),
                ("17-vault-card", nil, Fixtures.vaultItems, "v3", Fixtures.studioHosts),
                ("17-vault-identity", nil, Fixtures.vaultItems, "v4", Fixtures.studioHosts),
                ("17-vault-login-also-inside", nil, Fixtures.vaultItems, "v6", Fixtures.studioHosts),
                ("17-vault-empty", nil, #"{"items":[]}"#, nil, Fixtures.studioHosts),
                ("17-vault-remote-pi-kitchen", "pi-kitchen", Fixtures.remoteVaultItems, "v1", Fixtures.studioHosts),
                ("17-vault-remote-offline", "pi-kitchen", #"{"items":[]}"#, nil, piOffline),
                ("17-vault-remove-failed", nil, Fixtures.vaultItems, "v1", Fixtures.studioHosts),
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
                if name.hasSuffix("remove-failed") {
                    app.vaultRemoveError = .failed(LatchFailure(code: "state_busy", message: "another latch is changing /Users/me/.latch; try again"))
                }
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
            stale.values["username"] = "ada2"; stale.error = .unreachable(AppModel.itemChangedElsewhere)
            let preview = (try Fixtures.decode(Fixtures.importPreview) as ImportPreviewFixture).preview
            let previewing = VaultImport(file: URL(filePath: "/Users/me/Downloads/Chrome Passwords.csv"), on: studio)
            previewing.stage(preview: preview)
            let imported = VaultImport(file: URL(filePath: "/Users/me/Downloads/Chrome Passwords.csv"), on: studio)
            imported.stage(preview: preview, result: try Fixtures.decode(Fixtures.importResult))
            let stalePreview = VaultImport(file: URL(filePath: "/Users/me/Downloads/Chrome Passwords.csv"), on: studio)
            stalePreview.stage(preview: preview, notice: VaultImport.changedSincePreview)
            let sheets: [(String, AnyView)] = [
                ("18-vault-new-login", AnyView(VaultItemSheet(editor: newLogin))),
                ("18-vault-new-identity", AnyView(VaultItemSheet(editor: VaultEditor(kind: .identity, on: studio)))),
                ("18-vault-edit-card", AnyView(VaultItemSheet(editor: editCard))),
                ("18-vault-edit-changed-elsewhere", AnyView(VaultItemSheet(editor: stale))),
                ("18-vault-import-preview", AnyView(ImportSheet(importer: previewing))),
                ("18-vault-import-done", AnyView(ImportSheet(importer: imported))),
                ("18-vault-import-changed-since-preview", AnyView(ImportSheet(importer: stalePreview))),
            ]
            for (name, sheet) in sheets {
                let size = name.contains("import") ? CGSize(width: 560, height: 460)
                    : name.contains("identity") ? CGSize(width: 480, height: 640) : CGSize(width: 480, height: 520)
                try await render(sheet.environment(app), size: size, dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
        }
        // Host Settings: this Mac serving, stopped with an update, another
        // host online and offline, viewer-only; and the New Agent sheet.
        for dark in [false, true] {
            let piOffline = Fixtures.studioHosts.replacingOccurrences(
                of: #""self":false,"online":true,"last_seen_at":"2026-10-22T22:06:30.037004Z""#,
                with: #""self":false,"online":false,"last_seen_at":null"#)
            let screens: [(String, String, String, String?, Bool)] = [
                ("19-settings-this-mac-serving", Fixtures.serving, Fixtures.studioHosts, nil, false),
                ("19-settings-this-mac-stopped-update", Fixtures.stopped, Fixtures.studioHosts, nil, true),
                ("19-settings-remote-online", Fixtures.serving, Fixtures.studioHosts, "pi-kitchen", false),
                ("19-settings-remote-offline", Fixtures.serving, piOffline, "pi-kitchen", false),
                ("19-settings-viewer-only", Fixtures.laptop, Fixtures.laptopHosts, nil, false),
            ]
            func settingsApp(status: String, account: String, chosen: String?, checked: Bool, plugins: String = Fixtures.studioPlugins,
                             google: String = Fixtures.googleNotConnected) throws -> AppModel {
                let app = AppModel(client: FakeLatchClient(), still: true)
                app.stage(status: try Fixtures.decode(Fixtures.status(status, plugins: plugins)))
                app.stage(account: try Fixtures.decode(account))
                app.stage(agents: try Fixtures.decode(Fixtures.agents), standings: [:])
                app.stage(summary: HostSummary(rules: .success(try Fixtures.decode(Fixtures.rulesStudio))), for: "studio")
                app.stage(summary: HostSummary(status: .success(try Fixtures.decode(Fixtures.status(Fixtures.piStatus, plugins: Fixtures.piPlugins))),
                                               rules: .success(try Fixtures.decode(Fixtures.rulesKitchen))), for: "pi-kitchen")
                app.stage(devices: try Fixtures.decode(Fixtures.devices), update: checked ? try Fixtures.decode(Fixtures.updateCheck) : nil)
                app.stage(connections: Connections(try Fixtures.decode(Fixtures.setup(on: [], google: google)) as SetupStatus))
                app.chosenHost = chosen
                app.section = .settings
                _ = app.stage(.done, progress: full)
                return app
            }
            for (name, status, account, chosen, checked) in screens {
                let app = try settingsApp(status: status, account: account, chosen: chosen, checked: checked)
                try await renderWindow(MainView().environment(app), size: CGSize(width: 900, height: 900), dark: dark,
                                       file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // This Mac with Google on and Messages needing Full Disk Access.
            let setUp = try settingsApp(status: Fixtures.serving, account: Fixtures.studioHosts, chosen: nil, checked: false,
                                        plugins: Fixtures.studioPluginsSetUp)
            try await renderWindow(MainView().environment(setUp), size: CGSize(width: 900, height: 900), dark: dark,
                                   file: output.appending(path: "19-settings-this-mac-plugins-\(dark ? "dark" : "light").png"))
            // Your account › Connections: no Google account, one, two, Plow
            // out of reach; and where a plugin's Manage Accounts… lands.
            let connections: [(String, String)] = [
                ("21-connections-none", Fixtures.googleNotConnected),
                ("21-connections-one", Fixtures.googleConnected(["ada@example.com"])),
                ("21-connections-two", Fixtures.googleConnected(["ada@example.com", "ada.work@example.com"])),
                ("21-connections-three", Fixtures.googleConnected(Fixtures.threeAccounts)),
                ("21-connections-some-need-sign-in", Fixtures.googleConnected(Fixtures.threeAccounts, needsSignIn: ["family@example.com"])),
                ("21-connections-all-need-sign-in", Fixtures.googleConnected(Fixtures.threeAccounts, needsSignIn: Fixtures.threeAccounts)),
                ("21-connections-unknown", Fixtures.googleUnreachable),
            ]
            for (name, google) in connections {
                let app = try settingsApp(status: Fixtures.serving, account: Fixtures.studioHosts, chosen: nil, checked: false, google: google)
                app.section = .connections
                try await renderWindow(MainView().environment(app), size: CGSize(width: 900, height: 700), dark: dark,
                                       file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            // Its sign-in sheet: adding an account, connecting a first, and
            // a wait that saw no new account.
            let timedOut = GoogleSignIn.problem(.failed(LatchFailure(code: "sign_in_timeout", message: "")), adding: true)
            for (name, sheet) in [("21-connections-sign-in-adding", GoogleSignIn(adding: true)),
                                  ("21-connections-sign-in-first", GoogleSignIn(adding: false)),
                                  ("21-connections-sign-in-timed-out", GoogleSignIn(adding: true, failure: timedOut))] {
                let app = try settingsApp(status: Fixtures.serving, account: Fixtures.studioHosts, chosen: nil, checked: false,
                                          google: Fixtures.googleConnected(["ada@example.com"]))
                app.stage(googleSignIn: sheet, link: Fixtures.signInLink)
                try await render(GoogleSignInSheet().environment(app), size: CGSize(width: 420, height: 170), dark: dark,
                                 file: output.appending(path: "\(name)-\(dark ? "dark" : "light").png"))
            }
            let managing = try settingsApp(status: Fixtures.serving, account: Fixtures.studioHosts, chosen: "pi-kitchen", checked: false,
                                           google: Fixtures.googleConnected(["ada@example.com"]))
            managing.manageAccounts()
            try await renderWindow(MainView().environment(managing), size: CGSize(width: 900, height: 700), dark: dark,
                                   file: output.appending(path: "21-connections-from-manage-accounts-\(dark ? "dark" : "light").png"))
            // The Settings window: up to date, and a build available.
            for (name, status, checked) in [("22-app-settings", Fixtures.serving, true), ("22-app-settings-update", Fixtures.stopped, false)] {
                let app = try settingsApp(status: status, account: Fixtures.studioHosts, chosen: nil, checked: checked)
                try await renderWindow(AppSettingsView().environment(app), size: CGSize(width: 520, height: 300), dark: dark,
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

    /// What `setup --status` says of Google for each Connect Google still.
    private static let google: [String: String] = [
        "05-grant-google-not-connected": Fixtures.googleNotConnected,
        "05-grant-google-waiting": Fixtures.googleNotConnected,
        "05-grant-google-wait-ended": Fixtures.googleNotConnected,
        "05-grant-google-connected-turned-on": Fixtures.googleConnectedOff,
        "05-grant-google-three-accounts": Fixtures.googleConnected(Fixtures.threeAccounts),
        "05-grant-google-adding-waiting": Fixtures.googleConnected(Fixtures.threeAccounts),
        "05-grant-google-adding-timed-out": Fixtures.googleConnected(Fixtures.threeAccounts),
        "05-grant-google-some-need-sign-in": Fixtures.googleConnected(Fixtures.threeAccounts, needsSignIn: ["family@example.com"]),
        "05-grant-google-rerun-connected": Fixtures.googleOn,
        "05-grant-google-plow-unreachable": Fixtures.googleUnreachable,
    ]

    /// Each host's activity as `latch audit` printed it.
    private static func stageActivity(_ app: AppModel) {
        for host in app.hosts where host.name == "studio" || host.name == "pi-kitchen" {
            app.feed(for: host).stage(host.name == "studio" ? Fixtures.studioAudit : Fixtures.kitchenAudit)
        }
    }

    /// The main window, title bar included, drawn off every display from its
    /// own views: the window server composites nothing there, so it has no
    /// image of it (nor of the sidebar's materials) to give.
    private static func renderWindow(_ view: some View, size: CGSize, dark: Bool, file: URL) async throws {
        guard wanted(file) else { return }
        let window = Offscreen(contentRect: CGRect(origin: CGPoint(x: -4000, y: -4000), size: size),
                               styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                               backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentViewController = NSHostingController(rootView: view)
        window.setFrame(CGRect(origin: CGPoint(x: -4000, y: -4000), size: size), display: true)
        window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(600))
        try Render.frame(window, to: file)
        window.orderOut(nil)
        print(file.path)
    }

    private static func render(_ view: some View, size: CGSize, dark: Bool, background: Bool = true, file: URL) async throws {
        guard wanted(file) else { return }
        let host = NSHostingView(rootView: view)
        // The panel is sized as the helper sizes it, not to its content.
        if !background { host.sizingOptions = [] }
        // A floating panel has no title bar, nor its minimum height.
        // Off every display: nothing shows, and no click lands on it.
        let window = Offscreen(contentRect: CGRect(origin: CGPoint(x: -4000, y: -4000), size: size),
                               styleMask: background ? [.titled, .fullSizeContentView] : [.borderless],
                               backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentView = host
        window.orderFrontRegardless()
        try await Task.sleep(for: .milliseconds(400))
        try Render.png(host, to: file, background: background)
        window.orderOut(nil)
        print(file.path)
    }
}
/// A window for a still, which never shows: kept off every display (a
/// titled window is otherwise moved onto one, such as a display left of
/// the main one) and transparent. The stills draw its views, not what the
/// window server shows.
private final class Offscreen: NSWindow {
    override init(contentRect: NSRect, styleMask: NSWindow.StyleMask, backing: NSWindow.BackingStoreType, defer flag: Bool) {
        super.init(contentRect: contentRect, styleMask: styleMask, backing: backing, defer: flag)
        alphaValue = 0
        ignoresMouseEvents = true
    }

    override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect { frameRect }
}
#endif

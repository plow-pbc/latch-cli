#if DEBUG || LATCH_TEST_HOOKS
import Foundation
import Synchronization

/// A latch with made-up data at the sizes the main window is measured at,
/// for a test-hooks app opened with `hooks.sh open --fixture SPEC`: this Mac
/// serving and logged in, its hosts, each host's records, vault items and
/// rules, and the account's agents. It never runs latch, reaches Plow, or
/// holds a secret; anything it can't make up is `FakeLatchClient`'s. With
/// `connectors`, this Mac's plugins and the account's Google accounts change
/// as they're changed, and stay so.
nonisolated struct SyntheticLatchClient: LatchClient {
    /// How much there is: "hosts=8,records=10000,vault=1000,agents=5,rate=10,delay=40,connectors=1500".
    struct Spec: Equatable {
        /// The account's hosts, this Mac first (3 to 8 of `names`).
        var hosts = 3
        /// Records on each host's audit, all in the first page.
        var records = 50
        /// Items in each host's vault.
        var vault = 100
        /// The account's agents.
        var agents = 2
        /// New records per second on each followed host, once caught up.
        var rate = 0
        /// Milliseconds every read takes, as a latch run would.
        var delay = 0
        /// Milliseconds a plugin change or a Google sign-in takes; with it,
        /// changes hold and Google has an account that works.
        var connectors = 0

        init() {}

        /// nil for a word it doesn't know or a number out of range.
        init?(_ text: String) {
            for pair in text.split(separator: ",") {
                let parts = pair.split(separator: "=")
                guard parts.count == 2, let value = Int(parts[1]), value >= 0 else { return nil }
                switch parts[0] {
                case "hosts": hosts = value
                case "records": records = value
                case "vault": vault = value
                case "agents": agents = value
                case "rate": rate = value
                case "delay": delay = value
                case "connectors": connectors = value
                default: return nil
                }
            }
            guard (1...Self.names.count).contains(hosts), (1...Self.agentNames.count).contains(agents) else { return nil }
        }

        /// This Mac, one online, one Plow counts offline, then one with a
        /// long name and one Plow can't say of.
        static let names = ["studio", "pi-kitchen", "sidecar", "build-server-in-the-garage-closet", "nas", "mini", "travel-air", "lab-linux"]
        static let agentNames = ["scout", "helper", "courier", "ledger", "sentinel", "archivist", "porter", "tinker"]
    }

    let spec: Spec
    let stateDir: String
    private let base = FakeLatchClient()
    private let connectors = Connectors()

    /// This Mac's plugins that are on and the account's Google accounts, as
    /// the last change left them.
    private final class Connectors: Sendable {
        let state = Mutex((on: Set<String>(), accounts: ["morgan@example.com"]))
    }

    init(_ spec: Spec, stateDir: String) {
        self.spec = spec
        self.stateDir = stateDir
    }

    private var hostNames: [String] { Array(Spec.names.prefix(spec.hosts)) }

    private func key(_ host: String) -> String { Self.hex(Spec.names.firstIndex(of: host) ?? 0, salt: 0x6b55, width: 64) }
    private func agentUid(_ index: Int) -> String { Self.hex(index, salt: 0xa9e7, width: 32) }

    /// A made-up id `width` hex digits long, distinct for each salt and n.
    private static func hex(_ n: Int, salt: Int, width: Int) -> String {
        String(repeating: "0", count: width - 12) + String(format: "%04x%08x", salt, n)
    }

    /// The host a read names: this Mac's for nil.
    private func host(_ name: String?) -> String { name ?? hostNames[0] }

    private func read<T: Decodable>(_ json: String) async throws -> T {
        if spec.delay > 0 { try await Task.sleep(for: .milliseconds(spec.delay)) }
        return try Fixtures.decode(json)
    }

    func localStatus() async throws -> LocalStatus {
        try await read(Fixtures.status(#"{"login":{"kind":"device"},"this_machine":{"name":"\#(hostNames[0])","key":"\#(key(hostNames[0]))","hosting":{"running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":4100,"blocked":null,"keep_awake":{"enabled":false,"held":false,"reason":"disabled"}}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"\#(stateDir)","version":"0.1.0","protocol":2}"#, plugins: plugins))
    }

    /// This Mac's plugins: off, or, with `connectors`, as changed.
    private var plugins: String {
        guard spec.connectors > 0 else { return Fixtures.studioPlugins }
        let on = connectors.state.withLock { $0.on }
        let plugins = ["google", "messages", "contacts"].map { name in
            on.contains(name)
                ? #"{"enable":null,"name":"\#(name)","reason":"enabled; agents can use it once latch is running","state":"enabled"}"#
                : #"{"enable":"latch plugins enable \#(name)","name":"\#(name)","reason":"its owner has not enabled it on this host","state":"disabled"}"#
        }
        return "[" + plugins.joined(separator: ",") + "]"
    }

    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> {
        AsyncThrowingStream { continuation in
            Task {
                if let status = try? await localStatus() { continuation.yield(status) }
            }
        }
    }

    func accountStatus() async throws -> AccountStatus {
        let hosts = hostNames.enumerated().map { index, name in
            let online = switch name {
            case "sidecar": #""online":false,"last_seen_at":null"#
            case "nas": #""online":null,"last_seen_at":null"#
            default: #""online":true,"last_seen_at":"2026-10-23T09:00:0\#(index)Z""#
            }
            return #"{"name":"\#(name)","key":"\#(key(name))","self":\#(index == 0),\#(online)}"#
        }
        return try await read(#"{"account":{"owner_uid":"usr_synthetic","display_name":"Morgan"},"hosts":[\#(hosts.joined(separator: ","))],"default_host":"\#(hostNames[0])","default_host_reason":null,"api_url":"http://127.0.0.1:9"}"#)
    }

    func hostStatus(_ host: String) async throws -> LocalStatus {
        try await read(Fixtures.status(Fixtures.piStatus.replacingOccurrences(of: "pi-kitchen", with: host), plugins: Fixtures.studioPlugins))
    }

    func agents(host: String?) async throws -> AgentsList {
        let agents = Spec.agentNames.prefix(spec.agents).enumerated().map { index, name in
            let standing = host == nil ? "null" : #"{"exec_blocked":\#(index % 3 == 2),"own_rules":\#(index % 2 == 0)}"#
            return #"{"cloud_kind":null,"created_at":"2026-10-23T05:37:45","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45","line":{"name":null,"number":"+1555000\#(String(format: "%04d", index + 2))"},"name":"\#(name)","standing":\#(standing),"status":null,"uid":"\#(agentUid(index))"}"#
        }
        return try await read(#"{"agents":[\#(agents.joined(separator: ","))]}"#)
    }

    func rulesShow(host: String?) async throws -> RulesShow {
        let own = Spec.agentNames.prefix(spec.agents).enumerated().filter { $0.offset % 2 == 0 }.map { index, name in
            #"{"agent_name":"\#(name)","agent_uid":"\#(agentUid(index))","revision":"\#(Self.hex(index, salt: 0x7e11, width: 64))","text":"\#(name) may run builds and tests in ~/Projects.\n\#(name) may read ~/Documents/Reports but never write there.\nAsk me before \#(name) sends anything outside this host."}"#
        }
        return try await read(#"{"agents":[\#(own.joined(separator: ","))],"deny_exec":{"agent_uids":[],"every_agent":false},"every_agent":{"revision":"\#(Self.hex(0, salt: 0x5a7e, width: 64))","text":"Agents may list and read files in ~/Projects.\nNever delete anything.\nNever touch ~/Finance.\n"},"host":"\#(self.host(host))"}"#)
    }

    func vaultList(host: String?) async throws -> VaultList {
        try await read(#"{"items":[\#((0..<spec.vault).map(Self.vaultItem).joined(separator: ","))]}"#)
    }

    func vaultShow(_ item: String, host: String?) async throws -> VaultShow {
        guard let index = Int(item.dropFirst()), (0..<spec.vault).contains(index) else { throw LatchError.unreachable("no such item") }
        return try await read(#"{"item":\#(Self.vaultItem(index)),"values":{"username":"user\#(index)@example.com"}}"#)
    }

    /// Logins, cards and notes, in turn; no values, so no secrets.
    private static func vaultItem(_ index: Int) -> String {
        switch index % 3 {
        case 0: #"{"fields":[{"custom":false,"hidden":false,"label":"username"},{"custom":false,"hidden":true,"label":"password"}],"item":"v\#(index)","kind":"login","revision":1,"sites":["site\#(index).example"],"title":"Site \#(index) (synthetic)","totp":\#(index % 6 == 0),"username":"user\#(index)@example.com"}"#
        case 1: #"{"fields":[{"custom":false,"hidden":true,"label":"number"},{"custom":false,"hidden":false,"label":"cardholder name"}],"item":"v\#(index)","kind":"card","revision":1,"sites":[],"title":"Card \#(index) (synthetic)","totp":false}"#
        default: #"{"fields":[],"item":"v\#(index)","kind":"note","revision":1,"sites":[],"title":"Note \#(index) (synthetic)","totp":false}"#
        }
    }

    /// The `n`th record on a host, oldest first: agents' requests allowed
    /// and refused, their connections, and the owner's own reads.
    private func record(_ n: Int, on host: String) -> String {
        // Each kind of record for every agent in turn.
        let agent = n / 5 % spec.agents, name = Spec.agentNames[agent], id = recordID(n, on: host)
        let time = Date(timeIntervalSince1970: 1_792_700_000 + Double(n)).formatted(.iso8601)
        let line: String = switch n % 5 {
        case 0: #""agent":"\#(agentUid(agent))","caller":"\#(Self.hex(agent, salt: 0xca11, width: 64))","caller_name":"\#(name)","decision":"allow","details":null,"op":"connect","outcome":"ok""#
        case 1, 2: #""agent":"\#(agentUid(agent))","caller":"\#(Self.hex(agent, salt: 0xca11, width: 64))","caller_name":"\#(name)","decision":"allow","details":{"args":{"argv":["ls","Projects/\#(n)"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]}},"op":"exec","outcome":"ok","command":"latch exec ls Projects/\#(n)""#
        case 3: #""agent":"\#(agentUid(agent))","caller":"\#(Self.hex(agent, salt: 0xca11, width: 64))","caller_name":"\#(name)","decision":"deny","details":{"args":{"argv":["cat","Finance/\#(n).csv"],"network":false,"secret_env":[],"workdir":"/Users/me","writes":[]}},"op":"exec","outcome":"gatekeeper_denied","command":"latch exec cat Finance/\#(n).csv""#
        default: #""agent":null,"caller":"\#(key(hostNames[0]))","caller_name":"\#(hostNames[0])","decision":"allow","details":null,"op":"rules","outcome":"ok","command":"latch rules show""#
        }
        return #"{"cursor":"\#(id)","record":{"id":"\#(id)","time":"\#(time)",\#(line)},"type":"record"}"#
    }

    /// A record's id, and its cursor: its last eight digits are its number.
    private func recordID(_ n: Int, on host: String) -> String {
        Self.hex(n, salt: 0xc0de + (Spec.names.firstIndex(of: host) ?? 0), width: 64)
    }

    func audit(host: String?, before: String?) async throws -> AuditPage {
        if spec.delay > 0 { try await Task.sleep(for: .milliseconds(spec.delay)) }
        return AuditPage(records: [], hasOlder: false)
    }

    /// Every record at once, or those after `after`, then, with a rate, new
    /// ones until stopped.
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> {
        let host = self.host(host), client = self
        return AsyncThrowingStream { continuation in
            let task = Task {
                func send(_ line: String) {
                    if let line = try? LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)) { continuation.yield(line) }
                }
                if client.spec.delay > 0 { try? await Task.sleep(for: .milliseconds(client.spec.delay)) }
                let start = String(repeating: "0", count: 64)
                var cursor = after ?? start
                var n = after.flatMap { $0 == start ? nil : Int($0.suffix(8), radix: 16) }.map { $0 + 1 } ?? 0
                send(#"{"cursor":"\#(cursor)","has_older":false,"type":"snapshot"}"#)
                while n < client.spec.records {
                    send(client.record(n, on: host))
                    cursor = client.recordID(n, on: host)
                    n += 1
                }
                send(#"{"cursor":"\#(cursor)","has_older":false,"type":"caught_up"}"#)
                guard client.spec.rate > 0 else { return }
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1) / client.spec.rate)
                    send(client.record(n, on: host))
                    n += 1
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func setupStatus() async throws -> SetupStatus {
        let (on, accounts) = connectors.state.withLock { (Array($0.on), $0.accounts) }
        let google = spec.connectors > 0 ? Fixtures.googleConnected(accounts) : nil
        return try await read(Fixtures.setup(on: spec.connectors > 0 ? on : [], google: google,
                                             host: #"{"step":"host","state":"done","reason":"latch is running.","args":null}"#))
    }

    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { base.login(name: name, replace: replace) }
    func rulesSet(_ text: String) async throws {}
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws {}
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        try await base.rulesTry(request, host: host, agent: agent, newAgent: newAgent, draft: draft)
    }
    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item { try await base.vaultAdd(draft, host: host) }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        try await base.vaultEdit(item, revision: revision, draft, host: host)
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws {}
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview { try await base.vaultImportPreview(file, host: host) }
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult {
        try await base.vaultImport(file, picks: picks, expect: expect, host: host)
    }
    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        guard spec.connectors > 0 else { return nil }
        try await Task.sleep(for: .milliseconds(spec.connectors))
        let accounts = connectors.state.withLock { $0.on.insert(plugin); return $0.accounts }
        return plugin == "google" ? GoogleEnabled(accounts: accounts) : nil
    }

    /// With `connectors`, a link, then, after a sign-in's time, one more
    /// account and Google on.
    func signInGoogle(addAccount: Bool) -> AsyncThrowingStream<GoogleSignInEvent, Error> {
        guard spec.connectors > 0 else { return base.signInGoogle(addAccount: addAccount) }
        let client = self
        return AsyncThrowingStream { continuation in
            let task = Task {
                continuation.yield(.link(Fixtures.signInLink))
                do {
                    try await Task.sleep(for: .milliseconds(client.spec.connectors))
                } catch {
                    return continuation.finish(throwing: error)
                }
                let accounts = client.connectors.state.withLock {
                    $0.accounts.append("morgan\($0.accounts.count + 1)@example.com")
                    $0.on.insert("google")
                    return $0.accounts
                }
                continuation.yield(.enabled(GoogleEnabled(accounts: accounts)))
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
    func disable(_ plugin: String) async throws {
        guard spec.connectors > 0 else { return }
        try await Task.sleep(for: .milliseconds(spec.connectors))
        _ = connectors.state.withLock { $0.on.remove(plugin) }
    }
    func devices() async throws -> DeviceList { try await read(Fixtures.devices) }
    func removeDevice(_ name: String) async throws {}
    func updateCheck() async throws -> UpdateCheck { try await read(Fixtures.updateCheck) }
    func logout() async throws -> LogoutResult { LogoutResult(device: nil) }
    func agentCatalog() async throws -> AgentCatalog { try await read(Fixtures.catalog) }
    func agentLines() async throws -> AgentLines { try await read(Fixtures.lines) }
    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated {
        try await base.newAgent(name: name, cloud: cloud, tokenFile: tokenFile, line: line)
    }
    func removeAgent(_ uid: String) async throws {}
    func agentShow(_ uid: String) async throws -> AgentsList.Agent {
        guard let agent = try await agents(host: nil).agents.first(where: { $0.uid == uid }) else { throw LatchError.unreachable("no such agent") }
        return agent
    }
}
#endif

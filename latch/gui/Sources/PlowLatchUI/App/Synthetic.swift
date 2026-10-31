#if DEBUG || LATCH_TEST_HOOKS
import Foundation
import Synchronization

/// A latch with made-up data at the sizes the main window is measured at,
/// for a test-hooks app opened with `hooks.sh open --fixture SPEC`: this Mac
/// serving and logged in, its hosts, each host's records and vault items,
/// the account's rules and its agents. It never runs latch, reaches Plow, or
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
        /// New records sent at once on each followed host, as fast as they
        /// go, once caught up (before `rate`'s): a burst of arrivals.
        var burst = 0
        /// Milliseconds every read takes, as a latch run would.
        var delay = 0
        /// Up to this many milliseconds more on each read, at random.
        var jitter = 0
        /// Milliseconds a plugin change or a Google sign-in takes; with it,
        /// changes hold and Google has an account that works.
        var connectors = 0
        /// 1: this Mac only views the hosts, not hosting (Plow lists the
        /// others), so it offers to start hosting.
        var viewer = 0

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
                case "burst": burst = value
                case "delay": delay = value
                case "jitter": jitter = value
                case "connectors": connectors = value
                case "viewer": viewer = value
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
    /// Whether this Mac is logged in: `logout` logs it out, `login` back in.
    private let session = Session()
    private final class Session: Sendable { let loggedIn = Mutex(true) }

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

    /// Waits as long as a read takes.
    private func wait() async throws {
        let ms = spec.delay + Int.random(in: 0...spec.jitter)
        if ms > 0 { try await Task.sleep(for: .milliseconds(ms)) }
    }

    private func read<T: Decodable>(_ json: String) async throws -> T {
        try await wait()
        return try Fixtures.decode(json)
    }

    func localStatus() async throws -> LocalStatus {
        let hosting = spec.viewer > 0
            ? #""running":false,"ready":false,"configuring":false,"managed":false,"attached":false,"state":"stopped","pid":null"#
            : #""running":true,"ready":true,"configuring":false,"managed":true,"attached":false,"state":"serving","pid":4100"#
        let login = session.loggedIn.withLock { $0 } ? #"{"kind":"device"}"# : "null"
        return try await read(Fixtures.status(#"{"login":\#(login),"this_machine":{"name":"\#(hostNames[0])","key":"\#(key(hostNames[0]))","hosting":{\#(hosting),"blocked":null,"keep_awake":{"enabled":false,"held":false,"reason":"disabled"}}},"update":{"available":false,"build":null,"checked_at":null},"last_error":null,"state_dir":"\#(stateDir)","version":"0.1.0","protocol":2}"#, plugins: plugins))
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
        let hosts = hostNames.enumerated().filter { $0.offset > 0 || spec.viewer == 0 }.map { index, name in
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
            let standing = host == nil ? "null" : #"{"exec_blocked":\#(index % 3 == 2)}"#
            return #"{"cloud_kind":null,"created_at":"2026-10-23T05:37:45","failure_code":null,"kind":"own","last_seen_at":"2026-10-23T05:37:45","line":{"name":null,"number":"+1555000\#(String(format: "%04d", index + 2))"},"name":"\#(name)","standing":\#(standing),"status":null,"uid":"\#(agentUid(index))"}"#
        }
        return try await read(#"{"agents":[\#(agents.joined(separator: ","))]}"#)
    }

    /// A rule for every agent on every host, and one of each agent's own
    /// on this Mac, every host holding them all.
    func rulesList() async throws -> RulesList {
        let own = Spec.agentNames.prefix(spec.agents).enumerated().map { index, name in
            #"{"id":"rl_\#(Self.hex(index, salt: 0x7e11, width: 16))","text":"\#(name) may run builds and tests in ~/Projects, and may read ~/Documents/Reports but never write there.","agents":[{"uid":"\#(agentUid(index))"}],"hosts":[{"key":"\#(key(hostNames[0]))"}],"behind_hosts":[]}"#
        }
        let every = #"{"id":"rl_\#(Self.hex(0, salt: 0x5a7e, width: 16))","text":"Agents may list and read files in ~/Projects. Never delete anything, and never touch ~/Finance.","agents":"all","hosts":"all","behind_hosts":[]}"#
        let hosts = hostNames.enumerated().map { index, name in
            #"{"key":"\#(key(name))","name":"\#(name)","self":\#(index == 0),"delivery":"\#(name == "sidecar" ? "unknown" : "current")","reason":\#(name == "sidecar" ? #""host_offline""# : "null"),"budget":{"over_budget":false}}"#
        }
        return try await read(#"{"rules":[\#(([every] + own).joined(separator: ","))],"hosts":[\#(hosts.joined(separator: ","))]}"#)
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
        default: #""agent":null,"caller":"\#(key(hostNames[0]))","caller_name":"\#(hostNames[0])","decision":"allow","details":{"args":{"command":"list"}},"op":"rules","outcome":"ok","command":"latch rules list""#
        }
        return #"{"cursor":"\#(id)","record":{"id":"\#(id)","time":"\#(time)",\#(line)},"type":"record"}"#
    }

    /// A record's id, and its cursor: its last eight digits are its number.
    private func recordID(_ n: Int, on host: String) -> String {
        Self.hex(n, salt: 0xc0de + (Spec.names.firstIndex(of: host) ?? 0), width: 64)
    }

    func audit(host: String?, before: String?) async throws -> AuditPage {
        try await wait()
        return AuditPage(records: [], hasOlder: false)
    }
    func auditShot(_ record: String, shot: String, host: String?) async throws -> Data {
        try await base.auditShot(record, shot: shot, host: host)
    }
    func auditJudge(_ id: String, host: String?) async throws -> String {
        try await base.auditJudge(id, host: host)
    }

    /// The newest `last` records (all without it), or those after `after`,
    /// then a burst, then, with a rate, new ones until stopped.
    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
        let host = self.host(host), client = self
        return AsyncThrowingStream { continuation in
            let task = Task {
                func send(_ line: String) {
                    if let line = try? LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)) { continuation.yield(line) }
                }
                try? await client.wait()
                let start = String(repeating: "0", count: 64)
                var n = after.flatMap { $0 == start ? nil : Int($0.suffix(8), radix: 16) }.map { $0 + 1 }
                    ?? max(0, client.spec.records - (last ?? client.spec.records))
                var cursor = after ?? (n == 0 ? start : client.recordID(n - 1, on: host))
                let older = after == nil && n > 0
                send(#"{"cursor":"\#(cursor)","has_older":\#(older),"type":"snapshot"}"#)
                while n < client.spec.records {
                    send(client.record(n, on: host))
                    cursor = client.recordID(n, on: host)
                    n += 1
                }
                send(#"{"cursor":"\#(cursor)","has_older":\#(older),"type":"caught_up"}"#)
                for _ in 0..<client.spec.burst where !Task.isCancelled {
                    send(client.record(n, on: host))
                    n += 1
                }
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
    /// Logged in already, as latch says at once.
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> {
        let name = hostNames[0]
        session.loggedIn.withLock { $0 = true }
        return AsyncThrowingStream { continuation in
            continuation.yield(.verified(name: name))
            continuation.finish()
        }
    }
    func rulesAdd(_ text: String, agents: RulesList.Scope, hosts: RulesList.Scope) async throws -> RulesWrite {
        try await base.rulesAdd(text, agents: agents, hosts: hosts)
    }
    func rulesAddIfEmpty(_ text: String) async throws -> RulesWrite { try await base.rulesAddIfEmpty(text) }
    func rulesEdit(_ id: String, text: String?, agents: RulesList.Scope?, hosts: RulesList.Scope?) async throws -> RulesWrite {
        try await base.rulesEdit(id, text: text, agents: agents, hosts: hosts)
    }
    func rulesRemove(_ id: String) async throws -> RulesWrite { try await base.rulesRemove(id) }
    func rulesTry(_ request: TryRequest, host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
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
    func update() async throws -> UpdateResult { UpdateResult(updated: false) }
    func logout() async throws -> LogoutResult {
        session.loggedIn.withLock { $0 = false }
        return LogoutResult(device: nil)
    }
    func agentCatalog() async throws -> AgentCatalog { try await read(Fixtures.catalog) }
    func agentCard(_ uid: String) async throws -> Data { throw LatchError.unreachable("no card on the synthetic latch") }
    func agentStartByText() async throws -> AgentStartByText { try await read(Fixtures.startByText) }
    func removeAgent(_ uid: String) async throws {}
}
#endif

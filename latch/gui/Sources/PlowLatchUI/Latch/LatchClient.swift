import Foundation

/// Everything the app asks of latch: the only type that knows a CLI exists.
nonisolated protocol LatchClient: Sendable {
    /// The state folder every run is given.
    var stateDir: String { get }
    func localStatus() async throws -> LocalStatus
    /// `latch status --host NAME`: another host's own `status --local`.
    func hostStatus(_ host: String) async throws -> LocalStatus
    /// `latch status`: the account's hosts, from Plow.
    func accountStatus() async throws -> AccountStatus
    /// `latch login --events [--name NAME] [--replace]`: the code to text,
    /// then how it ends. Stopping the iteration cancels the login.
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error>
    /// `latch rules show`, on this Mac's host or, with `host`, another.
    func rulesShow(host: String?) async throws -> RulesShow
    /// `latch rules set --file -`, the text on stdin.
    func rulesSet(_ text: String) async throws
    /// `latch setup --status`: what's set up, read from the state folder.
    func setupStatus() async throws -> SetupStatus
    /// The same, with `--check-access`: whether Plow Latch has Full Disk
    /// Access, which takes a moment.
    func setupStatusCheckingAccess() async throws -> SetupStatus
    /// `latch rules set --file -` (or `rules clear` for no text) on a host,
    /// for every agent or, with `agent` (a uid), that agent's own, only if
    /// they are still at `revision` (nil: there were none); otherwise latch
    /// refuses with `stale_revision` and nothing changes.
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws
    /// `latch agents [--host NAME]`.
    func agents(host: String?) async throws -> AgentsList
    /// `latch rules try`: the verdict `request` (as an agent would send it)
    /// would get on a host now, as `agent` (a uid), a new agent, or you;
    /// `draft` judges against unsaved rules for every agent.
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry
    /// `latch plugins list`, on this Mac's host or, with `host`, another.
    func pluginsList(host: String?) async throws -> PluginsList
    /// `latch vault list`, on this Mac's host or, with `host`, another.
    func vaultList(host: String?) async throws -> VaultList
    /// `latch vault show ITEM`: its non-secret values.
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow
    /// `latch vault add --stdin`, every value on stdin, never in argv.
    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item
    /// `latch vault edit ITEM --revision R --stdin`: the title, the sites,
    /// and the values that changed (an empty one clears it), on stdin.
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item
    /// `latch vault remove ITEM --revision R`.
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws
    /// `latch vault import FILE --preview`: what it holds; nothing is saved.
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview
    /// `latch vault import FILE --pick ROWS --expect TOKEN`: saves those rows
    /// of the preview, only if they still mean what it showed.
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult
    /// `latch plugins enable NAME`; Google answers with its accounts.
    func enable(_ plugin: String) async throws -> GoogleEnabled?
    /// `latch status --local --follow`: this Mac's state, again whenever it
    /// changes. Stopping the iteration ends it.
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error>
    /// `latch audit [--before ID]`: a page of a host's audit, this Mac's or,
    /// with `host`, another's.
    func audit(host: String?, before: String?) async throws -> AuditPage
    /// `latch audit --follow`: the newest page, then each new record;
    /// `after` picks up after that record instead. Stopping the iteration
    /// ends it.
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error>
    /// A setup step, as `setup --status` gives its words after `latch`.
    func perform(_ args: [String]) async throws
    /// `latch stop`: stops the host on this state folder.
    func stop() async throws
    /// `latch plugins disable NAME` on this Mac's host.
    func disable(_ plugin: String) async throws
    /// `latch devices`: the account's logged-in machines.
    func devices() async throws -> DeviceList
    /// `latch devices remove NAME --yes`.
    func removeDevice(_ name: String) async throws
    /// `latch update --check`.
    func updateCheck() async throws -> UpdateCheck
    /// `latch logout`: this Mac leaves the account; its vault stays.
    func logout() async throws -> LogoutResult
    /// `latch agents catalog`, `latch agents lines`.
    func agentCatalog() async throws -> AgentCatalog
    func agentLines() async throws -> AgentLines
    /// `latch agents new`: a cloud agent of `cloud`'s kind, or (nil) your
    /// own, its credential written to `tokenFile`.
    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated
    /// `latch agents show UID`.
    func agentShow(_ uid: String) async throws -> AgentsList.Agent
}

extension LatchClient {
    func rulesShow() async throws -> RulesShow { try await rulesShow(host: nil) }
}

/// The bundle's own latch, by absolute path, always with `--state-dir`.
nonisolated struct LiveLatchClient: LatchClient {
    let executable: URL
    let stateDir: String

    func localStatus() async throws -> LocalStatus {
        try decode(LocalStatus.self, try await run(["status", "--local"]))
    }

    func accountStatus() async throws -> AccountStatus {
        try decode(AccountStatus.self, try await run(["status"]))
    }

    func hostStatus(_ host: String) async throws -> LocalStatus {
        try decode(LocalStatus.self, try await run(["status", "--host", host]))
    }

    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> {
        let arguments = ["login", "--events"] + (name.map { ["--name", $0] } ?? []) + (replace ? ["--replace"] : [])
        return stream(arguments) { try decode(LoginLine.self, $0).event }
    }

    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> {
        stream(["status", "--local", "--follow"]) { data -> LocalStatus? in
            guard case let .status(status) = try decode(StatusLine.self, data) else { return nil }
            return status
        }
    }

    func audit(host: String?, before: String?) async throws -> AuditPage {
        let data = try await run(["audit"] + (before.map { ["--before", $0] } ?? []) + on(host))
        do {
            return try AuditPage.parse(data)
        } catch {
            throw LatchError.unreachable("Latch returned an unreadable response.")
        }
    }

    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> {
        stream(["audit", "--follow"] + (after.map { ["--after", $0] } ?? []) + on(host)) { try decode(AuditLine.self, $0) }
    }

    /// Runs latch and yields each JSON line it prints (skipping those
    /// `decodeLine` gives nil for), then latch's error if it failed.
    /// Stopping the iteration stops latch.
    private func stream<T: Sendable>(_ arguments: [String], _ decodeLine: @escaping @Sendable (Data) throws -> T?)
        -> AsyncThrowingStream<T, Error> {
        let process = LatchProcess(executable: executable, arguments: withState(arguments), environment: Self.environment)
        return AsyncThrowingStream { continuation in
            let reader = Task.detached {
                do {
                    for await line in try process.lines() {
                        guard let value = try decodeLine(Data(line.utf8)) else { continue }
                        continuation.yield(value)
                    }
                    if Task.isCancelled { return continuation.finish() }
                    continuation.finish(throwing: await process.failure())
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in
                reader.cancel()
                process.terminate()
            }
        }
    }

    func rulesShow(host: String?) async throws -> RulesShow {
        try decode(RulesShow.self, try await run(["rules", "show"] + on(host)))
    }

    func rulesSet(_ text: String) async throws {
        _ = try await run(["rules", "set", "--file", "-"], input: Data(text.utf8))
    }

    func setupStatus() async throws -> SetupStatus {
        try decode(SetupStatus.self, try await run(["setup", "--status"]))
    }

    func setupStatusCheckingAccess() async throws -> SetupStatus {
        try decode(SetupStatus.self, try await run(["setup", "--status", "--check-access"]))
    }

    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws {
        let whose = on(host) + (agent.map { ["--agent", $0] } ?? [])
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            // No rules to clear when there were none.
            guard let revision else { return }
            _ = try await run(["rules", "clear", "--revision", revision] + whose)
        } else {
            _ = try await run(["rules", "set", "--revision", revision ?? "none", "--file", "-"] + whose, input: Data(text.utf8))
        }
    }

    func agents(host: String?) async throws -> AgentsList {
        try decode(AgentsList.self, try await run(["agents"] + on(host)))
    }

    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        var arguments = ["rules", "try"] + on(host)
        if let agent { arguments += ["--agent", agent] }
        if newAgent { arguments.append("--new-agent") }
        if draft != nil { arguments += ["--draft", "-"] }
        return try decode(RulesTry.self, try await run(arguments + ["--"] + request, input: draft.map { Data($0.utf8) }))
    }

    func pluginsList(host: String?) async throws -> PluginsList {
        try decode(PluginsList.self, try await run(["plugins", "list"] + on(host)))
    }

    func vaultList(host: String?) async throws -> VaultList {
        try decode(VaultList.self, try await run(["vault", "list"] + on(host)))
    }

    func vaultShow(_ item: String, host: String?) async throws -> VaultShow {
        try decode(VaultShow.self, try await run(["vault", "show", item] + on(host)))
    }

    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        let arguments = ["vault", "add", "--kind", draft.kind, "--title", draft.title]
            + draft.sites.flatMap { ["--site", $0] } + draft.otherTopSites.flatMap { ["--other-top-site", $0] }
        let given = draft.values.filter { !$0.value.isEmpty }
        return try decode(VaultItemAnswer.self, try await run(arguments + ["--stdin"] + on(host), input: try Self.values(given))).item
    }

    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        var arguments = ["vault", "edit", item, "--revision", String(revision), "--title", draft.title]
        arguments += draft.sites.isEmpty ? ["--no-sites"] : draft.sites.flatMap { ["--site", $0] }
        if draft.kind == "login" {
            arguments += draft.otherTopSites.isEmpty ? ["--no-other-top-sites"] : draft.otherTopSites.flatMap { ["--other-top-site", $0] }
        }
        return try decode(VaultItemAnswer.self, try await run(arguments + ["--stdin"] + on(host), input: try Self.values(draft.values))).item
    }

    func vaultRemove(_ item: String, revision: Int, host: String?) async throws {
        _ = try await run(["vault", "remove", item, "--revision", String(revision)] + on(host))
    }

    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview {
        try decode(ImportPreviewAnswer.self, try await run(["vault", "import", file.path, "--preview"] + on(host))).preview
    }

    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult {
        let rows = picks.map(String.init).joined(separator: ",")
        return try decode(ImportResult.self, try await run(["vault", "import", file.path, "--pick", rows, "--expect", expect] + on(host)))
    }

    /// Field values as `--stdin` takes them: one JSON object of strings.
    private static func values(_ values: [String: String]) throws -> Data {
        try JSONSerialization.data(withJSONObject: values)
    }

    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        let data = try await run(["plugins", "enable", plugin])
        return plugin == "google" ? try decode(GoogleEnabled.self, data) : nil
    }

    func perform(_ args: [String]) async throws {
        _ = try await run(args)
    }

    func stop() async throws {
        _ = try await run(["stop"])
    }

    func disable(_ plugin: String) async throws {
        _ = try await run(["plugins", "disable", plugin])
    }

    func devices() async throws -> DeviceList {
        try decode(DeviceList.self, try await run(["devices"]))
    }

    func removeDevice(_ name: String) async throws {
        _ = try await run(["devices", "remove", name, "--yes"])
    }

    func updateCheck() async throws -> UpdateCheck {
        try decode(UpdateCheck.self, try await run(["update", "--check"]))
    }

    func logout() async throws -> LogoutResult {
        try decode(LogoutResult.self, try await run(["logout"]))
    }

    func agentCatalog() async throws -> AgentCatalog {
        try decode(AgentCatalog.self, try await run(["agents", "catalog"]))
    }

    func agentLines() async throws -> AgentLines {
        try decode(AgentLines.self, try await run(["agents", "lines"]))
    }

    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated {
        let kind = cloud.map { ["--cloud", $0] } ?? ["--own", "--token-file", tokenFile?.path ?? ""]
        return try decode(AgentCreated.self, try await run(["agents", "new", "--name", name, "--line", line] + kind))
    }

    func agentShow(_ uid: String) async throws -> AgentsList.Agent {
        try decode(AgentAnswer.self, try await run(["agents", "show", uid])).agent
    }

    private func run(_ arguments: [String], input: Data? = nil) async throws -> Data {
        try await LatchProcess(executable: executable, arguments: withState(arguments), input: input, environment: Self.environment).output()
    }

    /// The app's environment, less what would pick a target for latch: the
    /// host and chat are always given as arguments, so an inherited
    /// `LATCH_HOST` (a terminal launch, `launchctl setenv`) can't redirect
    /// a read meant for this Mac.
    private static let environment = childEnvironment(ProcessInfo.processInfo.environment)

    static func childEnvironment(_ inherited: [String: String]) -> [String: String] {
        inherited.filter { !["LATCH_HOST", "LATCH_PLOW_CHAT"].contains($0.key) }
    }

    private func on(_ host: String?) -> [String] {
        host.map { ["--host", $0] } ?? []
    }

    private func withState(_ arguments: [String]) -> [String] {
        ["--state-dir", stateDir] + arguments
    }
}

/// `vault add`'s and `vault edit`'s answer: the item as saved.
nonisolated private struct VaultItemAnswer: Decodable { var item: VaultList.Item }
nonisolated private struct AgentAnswer: Decodable { var agent: AgentsList.Agent }
nonisolated private struct ImportPreviewAnswer: Decodable { var preview: ImportPreview }

nonisolated private func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
    do {
        return try LatchJSON.decoder.decode(type, from: data)
    } catch {
        throw LatchError.unreachable("Latch returned an unreadable response.")
    }
}

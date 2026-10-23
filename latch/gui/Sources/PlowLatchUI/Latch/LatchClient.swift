import Foundation

/// Everything the app asks of latch: the only type that knows a CLI exists.
nonisolated protocol LatchClient: Sendable {
    /// The state folder every run is given.
    var stateDir: String { get }
    func localStatus() async throws -> LocalStatus
    /// `latch status`: the account's hosts, from Plow.
    func accountStatus() async throws -> AccountStatus
    /// `latch login --events`: the code to text, then how it ends. Stopping
    /// the iteration cancels the login.
    func login() -> AsyncThrowingStream<LoginEvent, Error>
    /// `latch rules show`, on this Mac's host or, with `host`, another.
    func rulesShow(host: String?) async throws -> RulesShow
    /// `latch rules set --file -`, the text on stdin.
    func rulesSet(_ text: String) async throws
    /// `latch plugins list`, on this Mac's host (it must be running) or,
    /// with `host`, another.
    func pluginsList(host: String?) async throws -> PluginsList
    /// `latch vault list`, on this Mac's host or, with `host`, another.
    func vaultList(host: String?) async throws -> VaultList
    /// `latch plugins enable NAME`; Google answers with its accounts.
    func enable(_ plugin: String) async throws -> GoogleEnabled?
    /// `latch serve -d`, `--again` to start with the last serve's flags.
    func serve(again: Bool) async throws
    /// `latch stop`: stops the host on this state folder.
    func stop() async throws
}

extension LatchClient {
    func rulesShow() async throws -> RulesShow { try await rulesShow(host: nil) }
    func pluginsList() async throws -> PluginsList { try await pluginsList(host: nil) }
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

    func login() -> AsyncThrowingStream<LoginEvent, Error> {
        let process = LatchProcess(executable: executable, arguments: withState(["login", "--events"]), environment: Self.environment)
        return AsyncThrowingStream { continuation in
            let reader = Task.detached {
                do {
                    for await line in try process.lines() {
                        continuation.yield(try decode(LoginEvent.self, Data(line.utf8)))
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

    func pluginsList(host: String?) async throws -> PluginsList {
        try decode(PluginsList.self, try await run(["plugins", "list"] + on(host)))
    }

    func vaultList(host: String?) async throws -> VaultList {
        try decode(VaultList.self, try await run(["vault", "list"] + on(host)))
    }

    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        let data = try await run(["plugins", "enable", plugin])
        return plugin == "google" ? try decode(GoogleEnabled.self, data) : nil
    }

    func serve(again: Bool) async throws {
        _ = try await run(["serve", "-d"] + (again ? ["--again"] : []))
    }

    func stop() async throws {
        _ = try await run(["stop"])
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

nonisolated private func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
    do {
        return try LatchJSON.decoder.decode(type, from: data)
    } catch {
        throw LatchError.unreachable("Latch returned an unreadable response.")
    }
}

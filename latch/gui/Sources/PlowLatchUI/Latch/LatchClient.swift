import Foundation

/// Everything the app asks of latch: the only type that knows a CLI exists.
nonisolated protocol LatchClient: Sendable {
    func localStatus() async throws -> LocalStatus
    /// `latch login --events`: the code to text, then how it ends. Stopping
    /// the iteration cancels the login.
    func login() -> AsyncThrowingStream<LoginEvent, Error>
    func rulesShow() async throws -> RulesShow
    /// `latch rules set --file -`, the text on stdin.
    func rulesSet(_ text: String) async throws
    /// `latch rules try -- REQUEST`, as the owner.
    func rulesTry(_ request: [String]) async throws -> RulesTry
    /// `latch plugins list`, on this Mac's host (it must be running).
    func pluginsList() async throws -> PluginsList
    /// `latch plugins enable NAME`; Google answers with its accounts.
    func enable(_ plugin: String) async throws -> GoogleEnabled?
    /// `latch serve -d`, `--again` to start with the last serve's flags.
    func serve(again: Bool) async throws
}

/// The bundle's own latch, by absolute path, always with `--state-dir`.
nonisolated struct LiveLatchClient: LatchClient {
    let executable: URL
    let stateDir: String

    func localStatus() async throws -> LocalStatus {
        try decode(LocalStatus.self, try await run(["status", "--local"]))
    }

    func login() -> AsyncThrowingStream<LoginEvent, Error> {
        let process = LatchProcess(executable: executable, arguments: withState(["login", "--events"]))
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

    func rulesShow() async throws -> RulesShow {
        try decode(RulesShow.self, try await run(["rules", "show"]))
    }

    func rulesSet(_ text: String) async throws {
        _ = try await run(["rules", "set", "--file", "-"], input: Data(text.utf8))
    }

    func rulesTry(_ request: [String]) async throws -> RulesTry {
        try decode(RulesTry.self, try await run(["rules", "try", "--"] + request))
    }

    func pluginsList() async throws -> PluginsList {
        try decode(PluginsList.self, try await run(["plugins", "list"]))
    }

    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        let data = try await run(["plugins", "enable", plugin])
        return plugin == "google" ? try decode(GoogleEnabled.self, data) : nil
    }

    func serve(again: Bool) async throws {
        _ = try await run(["serve", "-d"] + (again ? ["--again"] : []))
    }

    private func run(_ arguments: [String], input: Data? = nil) async throws -> Data {
        try await LatchProcess(executable: executable, arguments: withState(arguments), input: input).output()
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

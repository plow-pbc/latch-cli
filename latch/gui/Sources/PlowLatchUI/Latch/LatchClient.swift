import Foundation

/// Everything the app asks of latch: the only type that knows a CLI exists.
nonisolated protocol LatchClient: Sendable {
    /// The state folder every run is given.
    var stateDir: String { get }
    func localStatus() async throws -> LocalStatus
    /// `latch login --events [--name NAME]`: the code to text, then how it
    /// ends. Stopping the iteration cancels the login.
    func login(name: String?) -> AsyncThrowingStream<LoginEvent, Error>
    func rulesShow() async throws -> RulesShow
    /// `latch rules set --file -`, the text on stdin.
    func rulesSet(_ text: String) async throws
    /// `latch setup --status`: what's set up, read from the state folder.
    func setupStatus() async throws -> SetupStatus
    /// `latch plugins enable NAME`; Google answers with its accounts.
    func enable(_ plugin: String) async throws -> GoogleEnabled?
    /// A setup step, as `setup --status` gives its words after `latch`.
    func perform(_ args: [String]) async throws
    /// `latch stop`: stops the host on this state folder.
    func stop() async throws
}

/// The bundle's own latch, by absolute path, always with `--state-dir`.
nonisolated struct LiveLatchClient: LatchClient {
    let executable: URL
    let stateDir: String

    func localStatus() async throws -> LocalStatus {
        try decode(LocalStatus.self, try await run(["status", "--local"]))
    }

    func login(name: String?) -> AsyncThrowingStream<LoginEvent, Error> {
        let process = LatchProcess(executable: executable, arguments: withState(["login", "--events"] + (name.map { ["--name", $0] } ?? [])))
        return AsyncThrowingStream { continuation in
            let reader = Task.detached {
                do {
                    for await line in try process.lines() {
                        guard let event = try decode(LoginLine.self, Data(line.utf8)).event else { continue }
                        continuation.yield(event)
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

    func setupStatus() async throws -> SetupStatus {
        try decode(SetupStatus.self, try await run(["setup", "--status"]))
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

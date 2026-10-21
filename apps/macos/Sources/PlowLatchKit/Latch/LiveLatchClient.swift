import Foundation
import Synchronization

/// Runs the bundled `latch` by absolute path, with an argument array (never a
/// shell, never PATH), and decodes what it prints.
public struct LiveLatchClient: LatchClient {
    public let executable: URL
    /// The state folder passed as `--state-dir`; nil uses latch's default.
    public let stateDir: String?

    public init(executable: URL, stateDir: String?) {
        self.executable = executable
        self.stateDir = stateDir
    }

    /// The `latch` inside this app, unless the `LatchExecutable` default
    /// points elsewhere; the state folder comes from the `LatchStateDir`
    /// default. Both can be set per launch: `-LatchStateDir /path/.dev/x`.
    public static func bundled(defaults: UserDefaults = .standard) -> LiveLatchClient {
        let executable = defaults.string(forKey: "LatchExecutable").map { URL(filePath: $0) }
            ?? Bundle.main.bundleURL.appending(path: "Contents/Helpers/latch")
        return LiveLatchClient(executable: executable, stateDir: defaults.string(forKey: "LatchStateDir"))
    }

    public func localStatus() async throws -> LocalStatus {
        try await decode(LocalStatus.self, from: run(["status", "--local"]))
    }

    public func rules() async throws -> RulesShow {
        try await decode(RulesShow.self, from: run(["rules", "show"]))
    }

    public func events(after cursor: String?, lifecycleOnly: Bool) -> AsyncThrowingStream<LatchEvent, Error> {
        var arguments = ["events", "--follow"]
        if let cursor { arguments += ["--after", cursor] }
        if lifecycleOnly { arguments.append("--lifecycle-only") }
        return stream(LatchEvent.self, arguments)
    }

    public func login() -> AsyncThrowingStream<LoginEvent, Error> {
        stream(LoginEvent.self, ["login", "--events"])
    }

    public func setShares(_ folders: [String]) async throws -> Shares {
        try await decode(Shares.self, from: run(["shares", "set"] + folders))
    }

    /// Runs latch and decodes each line it prints, until the caller stops.
    private func stream<T: Decodable & Sendable>(_ type: T.Type, _ arguments: [String]) -> AsyncThrowingStream<T, Error> {
        let process = LatchProcess(executable: executable, arguments: withState(arguments))
        return AsyncThrowingStream { continuation in
            let reader = Task {
                do {
                    for await line in try process.lines() {
                        continuation.yield(try decode(type, from: Data(line.utf8)))
                    }
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

    private func run(_ arguments: [String]) async throws -> Data {
        let process = LatchProcess(executable: executable, arguments: withState(arguments))
        return try await process.output()
    }

    private func withState(_ arguments: [String]) -> [String] {
        guard let stateDir else { return arguments }
        return ["--state-dir", stateDir] + arguments
    }

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do {
            return try LatchJSON.decoder.decode(type, from: data)
        } catch {
            let text = String(decoding: data.prefix(200), as: UTF8.self)
            throw LatchError.unreachable("latch printed something unexpected: \(text)")
        }
    }
}

/// One run of latch: stdout for answers, stderr for its error JSON.
public final class LatchProcess: @unchecked Sendable {
    /// Every latch this app has started and not yet seen exit.
    private static let running = Mutex<[ObjectIdentifier: Process]>([:])

    /// Stops every latch still running, so none outlives the app.
    public static func terminateAll() {
        running.withLock { $0.values.forEach { $0.terminate() } }
    }

    private let process = Process()
    private let stdout = Pipe()
    private let stderr = Pipe()
    private let exited = AsyncStream<Int32>.makeStream()

    init(executable: URL, arguments: [String]) {
        process.executableURL = executable
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = stdout
        process.standardError = stderr
        process.terminationHandler = { [exited] process in
            Self.running.withLock { _ = $0.removeValue(forKey: ObjectIdentifier(process)) }
            exited.continuation.yield(process.terminationStatus)
            exited.continuation.finish()
        }
    }

    /// Runs to the end and returns stdout, or throws latch's error.
    func output() async throws -> Data {
        try start()
        async let out = Self.readAll(stdout)
        async let err = Self.readAll(stderr)
        let (data, errData) = await (out, err)
        let status = await exitStatus()
        guard status == 0 else { throw Self.failure(status: status, stderr: errData) }
        return data
    }

    /// Starts latch and yields stdout line by line as it prints. Read with a
    /// readability handler, not `FileHandle.bytes`: those reads share one
    /// serial queue, so one quiet latch stream would hold up every other.
    func lines() throws -> AsyncStream<String> {
        try start()
        let (lines, continuation) = AsyncStream<String>.makeStream()
        final class Pending: @unchecked Sendable { var data = Data() }
        let pending = Pending()
        stdout.fileHandleForReading.readabilityHandler = { handle in
            let chunk = handle.availableData
            if chunk.isEmpty {
                handle.readabilityHandler = nil
                if !pending.data.isEmpty { continuation.yield(String(decoding: pending.data, as: UTF8.self)) }
                continuation.finish()
                return
            }
            pending.data.append(chunk)
            while let newline = pending.data.firstIndex(of: UInt8(ascii: "\n")) {
                let line = pending.data[pending.data.startIndex..<newline]
                if !line.isEmpty { continuation.yield(String(decoding: line, as: UTF8.self)) }
                pending.data.removeSubrange(pending.data.startIndex...newline)
            }
        }
        return lines
    }

    /// After stdout ends: latch's error if it exited non-zero, else nil.
    func failure() async -> Error? {
        let errData = await Self.readAll(stderr)
        let status = await exitStatus()
        return status == 0 ? nil : Self.failure(status: status, stderr: errData)
    }

    func terminate() {
        if process.isRunning { process.terminate() }
    }

    private func start() throws {
        do {
            try Self.running.withLock { running in
                try process.run()
                running[ObjectIdentifier(process)] = process
            }
        } catch {
            throw LatchError.unreachable("Couldn't run latch at \(process.executableURL?.path ?? "?"): \(error.localizedDescription)")
        }
    }

    private func exitStatus() async -> Int32 {
        for await status in exited.stream { return status }
        return process.terminationStatus
    }

    private static func readAll(_ pipe: Pipe) async -> Data {
        await pipe.fileHandleForReading.readUntilEnd()
    }

    private struct ErrorLine: Decodable { var error: LatchFailure }

    private static func failure(status: Int32, stderr: Data) -> LatchError {
        // latch prints its error as the last line of stderr.
        let last = stderr.split(separator: UInt8(ascii: "\n")).last.map { Data($0) } ?? Data()
        if let line = try? JSONDecoder().decode(ErrorLine.self, from: last) {
            return .failed(line.error)
        }
        let text = String(decoding: stderr.suffix(300), as: UTF8.self)
        return .unreachable("latch exited with status \(status): \(text)")
    }
}

extension FileHandle {
    /// Everything up to end of file, read as it arrives. Never blocks a
    /// thread: blocked reads would starve Swift's few concurrency threads,
    /// which the app's other latch processes need.
    func readUntilEnd() async -> Data {
        final class Buffer: @unchecked Sendable { var data = Data() }
        let buffer = Buffer()
        return await withCheckedContinuation { continuation in
            readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    handle.readabilityHandler = nil
                    continuation.resume(returning: buffer.data)
                } else {
                    buffer.data.append(chunk)
                }
            }
        }
    }
}

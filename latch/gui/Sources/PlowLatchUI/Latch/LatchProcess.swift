import Foundation
import Synchronization

/// Why a call to latch produced no answer.
nonisolated enum LatchError: Error, Hashable {
    /// latch ran and refused, with its own error.
    case failed(LatchFailure)
    /// latch could not be run, or printed something that isn't its JSON:
    /// the app's sentence, and what latch or the system said, if anything.
    case unreachable(String, detail: String? = nil)

    var message: String {
        switch self {
        case let .failed(failure): failure.message
        case let .unreachable(message, _): message
        }
    }

    /// What goes under Details: latch's own words, never shown as the
    /// sentence.
    var details: String? {
        switch self {
        case let .failed(failure): failure.message
        case let .unreachable(_, detail): detail
        }
    }

    /// latch's error code; the app decides on this, never on the message.
    var code: String? {
        if case let .failed(failure) = self { failure.code } else { nil }
    }
}

extension LatchError: LocalizedError {
    var errorDescription: String? { message }
}

/// latch's error JSON: `{"error": {"code", "message", …}}`, with the
/// fields the app acts on.
nonisolated struct LatchFailure: Decodable, Hashable {
    var code: String
    var message: String
    /// Full Disk Access not granted: what to grant it to, and where.
    var grant: String?
    var settings: String?
    /// `not_enabled`: the connector that is off.
    var plugin: String?
    /// `google_not_connected`: where to connect Google in Plow.
    var connectUrl: URL?
    /// `conflict` at login: the machine that has the name, and when Plow
    /// last saw it.
    var existingName: String?
    var lastSeenAt: Date?
}

/// One run of latch: argv only (never a shell), stdout for answers, stderr
/// for its error JSON.
nonisolated final class LatchProcess: @unchecked Sendable {
    /// Every latch this app has started and not yet seen exit.
    private static let running = Mutex<[ObjectIdentifier: Process]>([:])

    /// Stops every latch still running, so none outlives the app.
    nonisolated static func terminateAll() {
        running.withLock { $0.values.forEach { $0.terminate() } }
    }

    private let process = Process()
    private let stdout = Pipe()
    private let stderr = Pipe()
    private let stdin = Pipe()
    /// What goes to latch's stdin, closed after; for a stream, nothing is
    /// written and the app holds it open: if the app dies, latch sees it
    /// close and its stream ends.
    private let input: Data?
    private let exit = Mutex<(status: Int32?, waiting: [CheckedContinuation<Int32, Never>])>((nil, []))

    nonisolated init(executable: URL, arguments: [String], input: Data? = nil, environment: [String: String]? = nil) {
        process.executableURL = executable
        process.arguments = arguments
        if let environment { process.environment = environment }
        self.input = input
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr
        process.terminationHandler = { [weak self] process in
            Self.running.withLock { _ = $0.removeValue(forKey: ObjectIdentifier(process)) }
            guard let self else { return }
            let status = process.terminationStatus
            let waiting = exit.withLock { now in
                now.status = status
                defer { now.waiting = [] }
                return now.waiting
            }
            for waiter in waiting { waiter.resume(returning: status) }
        }
    }

    /// Runs to the end and returns stdout, or throws latch's error. A
    /// cancelled caller stops latch (SIGTERM) and gets CancellationError.
    nonisolated func output() async throws -> Data {
        try await withTaskCancellationHandler {
            try await run()
        } onCancel: {
            terminate()
        }
    }

    private nonisolated func run() async throws -> Data {
        try start()
        // Cancelled before latch started: it ends at once.
        if Task.isCancelled { terminate() }
        if let input {
            try? stdin.fileHandleForWriting.write(contentsOf: input)
        }
        try? stdin.fileHandleForWriting.close()
        async let out = stdout.fileHandleForReading.readUntilEnd()
        async let err = stderr.fileHandleForReading.readUntilEnd()
        let (data, errData) = await (out, err)
        let status = await exitStatus()
        try Task.checkCancellation()
        guard status == 0 else { throw Self.failure(status: status, stderr: errData) }
        return data
    }

    /// Starts latch and yields stdout line by line as it prints. Read with a
    /// readability handler, not `FileHandle.bytes`: those reads share one
    /// serial queue, so one quiet stream would hold up every other.
    nonisolated func lines() throws -> AsyncStream<String> {
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
    nonisolated func failure() async -> LatchError? {
        let errData = await stderr.fileHandleForReading.readUntilEnd()
        let status = await exitStatus()
        return status == 0 ? nil : Self.failure(status: status, stderr: errData)
    }

    nonisolated func terminate() {
        try? stdin.fileHandleForWriting.close()
        if process.isRunning { process.terminate() }
    }

    private nonisolated func start() throws {
        do {
            try Self.running.withLock { running in
                try process.run()
                running[ObjectIdentifier(process)] = process
            }
        } catch {
            throw LatchError.unreachable("Latch couldn't start.", detail: error.localizedDescription)
        }
    }

    /// Waits until latch has exited, even in a cancelled task.
    private nonisolated func exitStatus() async -> Int32 {
        await withCheckedContinuation { continuation in
            let status = exit.withLock { now in
                if now.status == nil { now.waiting.append(continuation) }
                return now.status
            }
            if let status { continuation.resume(returning: status) }
        }
    }

    /// Runs latch as nothing the app holds: in a session of its own, stdin
    /// from /dev/null, stdout and stderr into `out` and `err` (never a pipe),
    /// none of the app's other descriptors, and not among the processes
    /// `terminateAll` stops. For `latch update`, whose installer quits the
    /// app partway through: the update goes on without it. Returns stdout,
    /// or throws latch's error, if the app is still here when it ends.
    nonisolated static func detached(executable: URL, arguments: [String], environment: [String: String],
                                     out: URL, err: URL) async throws -> Data {
        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_addopen(&actions, 1, out.path, O_WRONLY | O_CREAT | O_TRUNC, 0o600)
        posix_spawn_file_actions_addopen(&actions, 2, err.path, O_WRONLY | O_CREAT | O_TRUNC, 0o600)
        var attributes: posix_spawnattr_t?
        posix_spawnattr_init(&attributes)
        defer { posix_spawnattr_destroy(&attributes) }
        posix_spawnattr_setflags(&attributes, Int16(POSIX_SPAWN_SETSID | POSIX_SPAWN_CLOEXEC_DEFAULT))

        let argv = ([executable.path] + arguments).map { strdup($0) } + [nil]
        let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer { (argv + envp).forEach { free($0) } }
        var pid: pid_t = 0
        let spawned = posix_spawn(&pid, executable.path, &actions, &attributes, argv, envp)
        guard spawned == 0 else {
            throw LatchError.unreachable("Latch couldn't start.", detail: String(cString: strerror(spawned)))
        }
        // Waited for off Swift's threads; a cancelled caller leaves it running.
        let status = await withCheckedContinuation { continuation in
            DispatchQueue.global().async {
                var status: Int32 = 0
                while waitpid(pid, &status, 0) == -1 && errno == EINTR {}
                continuation.resume(returning: status)
            }
        }
        // An exit's code, or a signal's number as `Process` gives it.
        let code = status & 0x7f == 0 ? (status >> 8) & 0xff : status & 0x7f
        guard code == 0 else { throw failure(status: code, stderr: (try? Data(contentsOf: err)) ?? Data()) }
        return (try? Data(contentsOf: out)) ?? Data()
    }

    private struct ErrorLine: Decodable { var error: LatchFailure }

    private nonisolated static func failure(status: Int32, stderr: Data) -> LatchError {
        // latch prints its error as the last line of stderr.
        let last = stderr.split(separator: UInt8(ascii: "\n")).last.map { Data($0) } ?? Data()
        if let line = try? LatchJSON.decoder.decode(ErrorLine.self, from: last) {
            return .failed(line.error)
        }
        // Not latch's error JSON: the exit status and the end of what it
        // printed go along as detail.
        let text = String(decoding: stderr.suffix(300), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return .unreachable("Latch couldn't complete this step.", detail: "exit \(status)\(text.isEmpty ? "" : ": \(text)")")
    }
}

nonisolated extension FileHandle {
    /// Everything up to end of file, read as it arrives, never blocking a
    /// thread (blocked reads would starve Swift's few concurrency threads).
    nonisolated func readUntilEnd() async -> Data {
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

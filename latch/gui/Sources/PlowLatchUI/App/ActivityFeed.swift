import Observation
import SwiftUI
import Synchronization

/// One host's audit as `latch audit --follow` streams it: the newest 500
/// records, then each record as it's written, kept with no limit, so a
/// selected record never goes from under the person. It runs while a screen
/// showing it is on screen and for a grace after, so coming straight back
/// doesn't start latch again, and picks up after the last record it got
/// when it starts again: every record since, with none missed or twice.
/// Records arrive in batches, at most one change every ~100 ms, however
/// fast latch sends them.
@Observable
final class ActivityFeed {
    let client: any LatchClient
    /// nil for this Mac's own host, else the host `--host` names.
    let host: String?

    /// Oldest first.
    private(set) var records: [AuditRecord] = []
    /// The ids of `records`, kept with them: a record already held is
    /// found without going through them all.
    @ObservationIgnored private var ids: Set<AuditRecord.ID> = []
    /// Records older than the first held exist on the host.
    private(set) var hasOlder = false
    /// Every record up to now has arrived.
    private(set) var caughtUp = false
    /// Why the stream stopped, until it runs again.
    private(set) var error: LatchError?
    private(set) var loadingOlder = false

    private var cursor: String?
    private var task: Task<Void, Never>?
    private var watchers = 0
    /// The earlier pages Home reads on its own looking for five requests.
    static let homePages = 2
    /// How many of the newest records a first read (or one after latch
    /// forgot where the last left off) starts with.
    static let openingPage = 500
    /// The least time between two changes to the records.
    static let batchInterval: Duration = .milliseconds(100)
    /// How long to wait before following again after the stream dropped;
    /// twice as long each time after, up to a minute.
    var firstRetry: Duration = .seconds(2)

    /// How long the stream keeps running after the last screen showing it
    /// goes.
    var grace: Duration = .seconds(30)
    private var release: Task<Void, Never>?
    /// The stream is running (it ends on its own after an error that
    /// waiting won't fix).
    private(set) var running = false

    init(client: any LatchClient, host: String?) {
        self.client = client
        self.host = host
    }

    /// A screen showing this feed appeared; the first one starts the stream.
    func start() {
        release?.cancel()
        release = nil
        watchers += 1
        guard !running else { return }
        running = true
        task = Task { await run() }
    }

    /// This Mac serves: if the stream stopped because it had never hosted,
    /// and the feed is still watched, follow again. Safe to call on every
    /// status; it does nothing otherwise.
    func retry() {
        guard watchers > 0, !running, error?.code == "no_host_here" else { return }
        running = true
        task = Task { await run() }
    }

    /// A screen showing it went away; the last one stops the stream (and
    /// latch with it) once the grace has passed with none back.
    func stop() {
        watchers = max(0, watchers - 1)
        guard watchers == 0, task != nil else { return }
        release?.cancel()
        release = Task { [grace] in
            try? await Task.sleep(for: grace)
            guard !Task.isCancelled, watchers == 0 else { return }
            end()
        }
    }

    /// Stops the stream now (the window closed); the records and where they
    /// got to stay, for the next start.
    func end() {
        release?.cancel()
        release = nil
        watchers = 0
        task?.cancel()
        task = nil
        running = false
    }

    /// Follows until stopped. A dropped stream (the host stopped or out of
    /// reach) is picked up again after the last record, waiting longer each
    /// time up to a minute; an error that won't change by waiting stops it.
    private func run() async {
        defer { if !Task.isCancelled { running = false } }
        var wait = firstRetry
        while !Task.isCancelled {
            do {
                let lines = client.auditFollow(host: host, after: cursor, last: cursor == nil ? Self.openingPage : nil)
                for try await batch in LineBatches.of(lines, every: Self.batchInterval) {
                    apply(batch)
                    wait = firstRetry
                }
            } catch let failure as LatchError {
                if failure.code == "cursor_expired" {
                    // Starts again from the newest records.
                    cursor = nil
                    records = []
                    ids = []
                    hasOlder = false
                    caughtUp = false
                    continue
                }
                guard !Task.isCancelled else { return }
                error = failure
                caughtUp = false
                if Self.final.contains(failure.code ?? "") { return }
            } catch {
                return
            }
            guard !Task.isCancelled else { return }
            try? await Task.sleep(for: wait)
            wait = min(wait * 2, .seconds(60))
        }
    }

    /// Errors that waiting won't fix; `no_host_here` until this Mac starts
    /// hosting (`retry`). A record too large to send stops the stream at
    /// the same record every time.
    private static let final: Set<String> = ["no_host_here", "owner_only", "usage", "audit_broken", "unknown_host", "not_logged_in",
                                             "record_too_large"]

    /// Lines in the order sent, changing each of the feed's properties
    /// once, however many there are.
    func apply(_ lines: [AuditLine]) {
        var added: [AuditRecord] = []
        var cursor = cursor, hasOlder = hasOlder, caughtUp = caughtUp, cleared = false
        let empty = records.isEmpty
        for line in lines {
            switch line {
            case let .snapshot(_, older):
                if empty, added.isEmpty { hasOlder = older }
            case let .record(at, record):
                cursor = at
                cleared = true
                if ids.insert(record.id).inserted { added.append(record) }
            case let .caughtUp(at, older):
                cursor = at
                if empty, added.isEmpty { hasOlder = older }
                caughtUp = true
                cleared = true
            case .hello, .other:
                break
            }
        }
        if cursor != self.cursor { self.cursor = cursor }
        if hasOlder != self.hasOlder { self.hasOlder = hasOlder }
        if caughtUp != self.caughtUp { self.caughtUp = caughtUp }
        if cleared, error != nil { error = nil }
        if !added.isEmpty { records.append(contentsOf: added) }
    }

    func apply(_ line: AuditLine) {
        apply([line])
    }

    /// "Load Earlier": the page before the first record held.
    func loadOlder() async {
        guard let first = records.first, !loadingOlder else { return }
        loadingOlder = true
        defer { loadingOlder = false }
        do {
            let page = try await client.audit(host: host, before: first.id)
            let earlier = page.records.filter { ids.insert($0.id).inserted }
            records.insert(contentsOf: earlier, at: 0)
            hasOlder = page.hasOlder
        } catch {
            // Leaving the screen that asked is no failure to show.
            guard !(error is CancellationError), !Task.isCancelled else { return }
            self.error = .from(error)
        }
    }

    /// Earlier pages until `requests` records aren't connections or the
    /// owner's reads, there are no more, or `pages` were read (Home's
    /// Recent, which offers the rest a page at a time).
    func loadOlder(until requests: Int, pages: Int) async {
        for _ in 0..<pages {
            guard hasOlder, error == nil, records.filter({ !$0.isBackground }).count < requests, !Task.isCancelled else { return }
            let held = records.count
            await loadOlder()
            if records.count == held { return }
        }
    }

    /// For snapshots: as a stream left it.
    func stage(_ lines: String, error: LatchError? = nil) {
        for line in lines.split(separator: "\n") {
            if let line = try? LatchJSON.decoder.decode(AuditLine.self, from: Data(line.utf8)) { apply(line) }
        }
        self.error = error
    }
}

/// A follow's lines gathered off the main actor and handed over in
/// batches, so a busy main thread takes one hop a batch rather than one a
/// line: a batch goes once `interval` has passed since the last (the first
/// counting from the start), at once when the stream has caught up, and
/// before the stream's end or error. Stopping the iteration stops the lines.
nonisolated final class LineBatches: Sendable {
    private struct Held {
        var lines: [AuditLine] = []
        var sent = ContinuousClock.now
        /// Sends lines held early when no later line comes in time.
        var timer: Task<Void, Never>?
    }
    private let held = Mutex(Held())
    private let interval: Duration
    private let out: AsyncThrowingStream<[AuditLine], Error>.Continuation

    private init(interval: Duration, out: AsyncThrowingStream<[AuditLine], Error>.Continuation) {
        self.interval = interval
        self.out = out
    }

    static func of(_ lines: AsyncThrowingStream<AuditLine, Error>, every interval: Duration)
        -> AsyncThrowingStream<[AuditLine], Error> {
        let (batches, out) = AsyncThrowingStream<[AuditLine], Error>.makeStream()
        let batcher = LineBatches(interval: interval, out: out)
        let reading = Task {
            do {
                for try await line in lines { batcher.receive(line) }
                batcher.finish(throwing: nil)
            } catch {
                batcher.finish(throwing: error)
            }
        }
        out.onTermination = { _ in reading.cancel() }
        return batches
    }

    private func receive(_ line: AuditLine) {
        held.withLock { held in
            held.lines.append(line)
            let due = held.sent + interval
            if case .caughtUp = line { return send(&held) }
            if ContinuousClock.now >= due { return send(&held) }
            guard held.timer == nil else { return }
            held.timer = Task { [self] in
                try? await Task.sleep(until: due)
                self.held.withLock { held in
                    // Cancelled under the lock by a send that went first.
                    guard !Task.isCancelled else { return }
                    send(&held)
                }
            }
        }
    }

    private func send(_ held: inout Held) {
        held.timer?.cancel()
        held.timer = nil
        guard !held.lines.isEmpty else { return }
        out.yield(held.lines)
        held.lines = []
        held.sent = .now
    }

    private func finish(throwing error: Error?) {
        held.withLock { send(&$0) }
        out.finish(throwing: error)
    }
}

/// How a record reads in a list: its request, who asked and what became of it.
nonisolated extension AuditRecord {
    /// What became of it, in words.
    enum Status: Equatable {
        case connected, allowed(String?), refused(String), notCovered

        var title: String {
            switch self {
            case .connected: "Connected"
            case let .allowed(note): note.map { "Allowed · \($0)" } ?? "Allowed"
            case let .refused(why): "Refused · \(why)"
            case .notCovered: "Refused · needed your OK"
            }
        }
    }

    var status: Status {
        if op == "connect", decision == "allow" { return .connected }
        if decision == "allow" { return .allowed(outcome == "ok" ? nil : outcome.replacingOccurrences(of: "_", with: " ")) }
        switch outcome {
        case "exec_denied": return .refused("exec is off for this agent")
        case "not_enabled": return .refused("\(op.capitalized) is off")
        case "gatekeeper_unavailable": return .refused("judge unavailable")
        case "caller_denied": return .refused("caller not allowed")
        case "owner_only": return .refused("the owner's only")
        case "gatekeeper_denied":
            return gate?.verdict == "owner_said_no" ? .refused("you said no") : .notCovered
        default: return .refused(outcome.replacingOccurrences(of: "_", with: " "))
        }
    }

    /// Who asked: an agent by its name, else the owner, from which machine.
    /// `hostKey` is the key of the host the record is on.
    func callerLabel(hostKey: String?, hostName: String, isThisMac: Bool) -> (who: String, kind: String) {
        if let agent { return (callerName ?? "Agent \(agent.prefix(8))", "agent") }
        if let caller, caller == hostKey { return ("You", isThisMac ? "on this Mac" : "on \(hostName)") }
        return ("You", callerName ?? caller.map { String($0.prefix(8)) } ?? "unknown")
    }
}

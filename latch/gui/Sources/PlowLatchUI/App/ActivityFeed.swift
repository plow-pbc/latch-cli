import Observation
import SwiftUI

/// One host's audit as `latch audit --follow` streams it: the newest page,
/// then each record as it's written. It runs only while a screen showing
/// it is on screen, and picks up after the last record it got when it
/// starts again.
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
    /// The stream is running (it ends on its own after an error that
    /// waiting won't fix).
    private(set) var running = false

    init(client: any LatchClient, host: String?) {
        self.client = client
        self.host = host
    }

    /// A screen showing this feed appeared; the first one starts the stream.
    func start() {
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
    /// latch with it).
    func stop() {
        watchers = max(0, watchers - 1)
        guard watchers == 0 else { return }
        task?.cancel()
        task = nil
        running = false
    }

    /// Follows until stopped. A dropped stream (the host stopped or out of
    /// reach) is picked up again after the last record, waiting longer each
    /// time up to a minute; an error that won't change by waiting stops it.
    private func run() async {
        defer { if !Task.isCancelled { running = false } }
        var wait = 2
        while !Task.isCancelled {
            do {
                for try await line in client.auditFollow(host: host, after: cursor) {
                    apply(line)
                    wait = 2
                }
            } catch let failure as LatchError {
                if failure.code == "cursor_expired" {
                    cursor = nil
                    records = []
                    ids = []
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
            try? await Task.sleep(for: .seconds(wait))
            wait = min(wait * 2, 60)
        }
    }

    /// Errors that waiting won't fix; `no_host_here` until this Mac starts
    /// hosting (`retry`). A record too large to send stops the stream at
    /// the same record every time.
    private static let final: Set<String> = ["no_host_here", "owner_only", "usage", "audit_broken", "unknown_host", "not_logged_in",
                                             "record_too_large"]

    func apply(_ line: AuditLine) {
        switch line {
        case let .snapshot(_, hasOlder):
            if records.isEmpty { self.hasOlder = hasOlder }
        case let .record(cursor, record):
            self.cursor = cursor
            error = nil
            guard ids.insert(record.id).inserted else { return }
            records.append(record)
        case let .caughtUp(cursor, hasOlder):
            self.cursor = cursor
            if records.isEmpty { self.hasOlder = hasOlder }
            caughtUp = true
            error = nil
        case .hello, .other:
            break
        }
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
            self.error = .from(error)
        }
    }

    /// Earlier pages until `requests` records aren't connections or the
    /// owner's reads, or there are no more (Home's Recent).
    func loadOlder(until requests: Int) async {
        while hasOlder, error == nil, records.filter({ !$0.isBackground }).count < requests {
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

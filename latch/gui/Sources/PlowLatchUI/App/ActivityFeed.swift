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

    /// Oldest first, at most `limit`.
    private(set) var records: [AuditRecord] = []
    /// Records older than the first held exist on the host.
    private(set) var hasOlder = false
    /// Every record up to now has arrived.
    private(set) var caughtUp = false
    /// Why the stream stopped, until it runs again.
    private(set) var error: LatchError?
    private(set) var loadingOlder = false

    /// The most records kept; older ones are dropped and can be loaded again.
    static let limit = 1000

    private var cursor: String?
    private var task: Task<Void, Never>?
    private var watchers = 0

    init(client: any LatchClient, host: String?) {
        self.client = client
        self.host = host
    }

    /// A screen showing this feed appeared; the first one starts the stream.
    func start() {
        watchers += 1
        guard task == nil else { return }
        task = Task { await run() }
    }

    /// A screen showing it went away; the last one stops the stream (and
    /// latch with it).
    func stop() {
        watchers = max(0, watchers - 1)
        guard watchers == 0 else { return }
        task?.cancel()
        task = nil
    }

    /// Follows until stopped. A dropped stream (the host stopped or out of
    /// reach) is picked up again after the last record, waiting longer each
    /// time up to a minute; an error that won't change by waiting stops it.
    private func run() async {
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

    /// Errors that waiting won't fix.
    private static let final: Set<String> = ["no_host_here", "owner_only", "usage", "audit_broken", "unknown_host", "not_logged_in"]

    func apply(_ line: AuditLine) {
        switch line {
        case let .snapshot(_, hasOlder):
            if records.isEmpty { self.hasOlder = hasOlder }
        case let .record(cursor, record):
            self.cursor = cursor
            error = nil
            guard !records.contains(where: { $0.id == record.id }) else { return }
            records.append(record)
            if records.count > Self.limit {
                records.removeFirst(records.count - Self.limit)
                hasOlder = true
            }
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
            let held = Set(records.map(\.id))
            records.insert(contentsOf: page.records.filter { !held.contains($0.id) }, at: 0)
            hasOlder = page.hasOlder
        } catch {
            self.error = error as? LatchError ?? .unreachable(error.localizedDescription)
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
            case .notCovered: "Refused · not covered"
            }
        }
    }

    var status: Status {
        if op == "connect" { return .connected }
        if decision == "allow" { return .allowed(outcome == "ok" ? nil : outcome.replacingOccurrences(of: "_", with: " ")) }
        switch outcome {
        case "exec_denied": return .refused("exec is off for this agent")
        case "not_enabled": return .refused("\(op.capitalized) is off")
        case "no_intent": return .refused("no instructions from you")
        case "gatekeeper_unavailable": return .refused("judge unavailable")
        case "gatekeeper_denied":
            return gate?.verdict == "owner_said_no" ? .refused("you said no") : .notCovered
        default: return .refused(outcome.replacingOccurrences(of: "_", with: " "))
        }
    }

    /// The request, close to how it was typed after `latch`, secrets by
    /// name only.
    var command: String {
        let a = args
        switch op {
        case "exec":
            var words: [String] = []
            for path in a?.writes ?? [] { words += ["--write", Self.quoted(path)] }
            if a?.network == true { words.append("--network") }
            for secret in a?.secretEnv ?? [] { words += ["--secret-env", "\(secret.name ?? "?")=•••"] }
            words += (a?.argv ?? []).map(Self.quoted)
            return words.joined(separator: " ")
        case "file_get": return "file get \(a?.path ?? "")"
        case "file_put": return "file put \(a?.path ?? "")"
        case "connect": return "connected"
        case "messages", "contacts":
            return [op, a?.action?.replacingOccurrences(of: "\(op).", with: "")].compactMap { $0 }.joined(separator: " ")
        case "browser":
            return ["browser", a?.command, a?.origin].compactMap { $0 }.joined(separator: " ")
        case "rules":
            let scope = a?.scope.map { " · \($0 == "every agent" ? "rules for every agent" : $0)" } ?? ""
            return ["rules", a?.command].compactMap { $0 }.joined(separator: " ") + scope
        default:
            return [op.replacingOccurrences(of: "_", with: " "), a?.command?.replacingOccurrences(of: ".", with: " ")]
                .compactMap { $0 }.joined(separator: " ")
        }
    }

    private static func quoted(_ word: String) -> String {
        let plain = word.allSatisfy { $0.isLetter || $0.isNumber || "-_./=:,@%+~".contains($0) }
        return plain && !word.isEmpty ? word : "'" + word.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Who asked: an agent by its name, else the owner, from which machine.
    /// `hostKey` is the key of the host the record is on.
    func callerLabel(hostKey: String?, hostName: String, isThisMac: Bool) -> (who: String, kind: String) {
        if let agent { return (callerName ?? "Agent \(agent.prefix(8))", "agent") }
        if let caller, caller == hostKey { return ("You", isThisMac ? "on this Mac" : "on \(hostName)") }
        return ("You", callerName ?? caller.map { String($0.prefix(8)) } ?? "unknown")
    }
}

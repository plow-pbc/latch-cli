import Foundation

/// One record of a host's audit, as `latch audit` prints it. Every field
/// past the first few is optional: older records and other ops leave them
/// out, and a newer latch may add more.
nonisolated struct AuditRecord: Decodable, Equatable, Identifiable {
    var id: String
    var time: Date
    /// The calling device's or agent's key.
    var caller: String?
    /// The caller's name when it called: an agent's in Plow, else its
    /// machine's. Null on some records (an agent's first call after a host
    /// starts, or records older than the field).
    var callerName: String?
    /// The agent's Plow uid; nil for the owner's own devices.
    var agent: String?
    /// `exec`, `file_get`, `google`, `browser`, `connect`, `rules`, …
    var op: String
    /// `allow` or `deny`.
    var decision: String
    /// `ok`, or why not: `gatekeeper_denied`, `exec_denied`, `not_enabled`, …
    var outcome: String
    var details: Details?

    struct Details: Decodable, Equatable {
        var started: Date?
        /// The fields the app formats itself (exec's, mostly).
        var args: Args?
        var gate: Gate?
        /// Everything the record holds, as latch wrote it: each op's args
        /// (Google's accounts and recipients, a file's path, size and
        /// SHA-256, a browser step's place…) and any other detail, so the
        /// inspector shows all of it, not only what the app knows.
        var fields: [String: JSONValue]

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            started = try? c.decodeIfPresent(Date.self, forKey: .started)
            // An op's args have their own shape; what the typed view doesn't
            // understand is still in `fields`.
            args = try? c.decodeIfPresent(Args.self, forKey: .args)
            gate = try? c.decodeIfPresent(Gate.self, forKey: .gate)
            // Decoded as a dictionary, the keys stay as latch spelled them.
            fields = (try? decoder.singleValueContainer().decode([String: JSONValue].self)) ?? [:]
        }

        private enum CodingKeys: String, CodingKey { case started, args, gate }
    }

    /// What the request asked, as the audit keeps it (credential-looking
    /// values already hidden by latch).
    struct Args: Decodable, Equatable {
        var argv: [String]?
        var workdir: String?
        var writes: [String]?
        var network: Bool?
        var secretEnv: [Secret]?
        var timeout: Int?
        var exitCode: Int?
        var durationMs: Int?
        var stderrBytes: Int?
        var timedOut: Bool?
        /// `rules`, `vault`, `plugins`, `browser`, `google`: which command.
        var command: String?
        var scope: String?
        var path: String?
        var item: String?
        var action: String?
        var url: String?
        var origin: String?
        /// A browser judgment, kept beside what was judged (`judged`).
        var gate: Gate?

        struct Secret: Decodable, Equatable { var name: String? }
    }

    /// The gatekeeper's judgment, when it was asked.
    struct Gate: Decodable, Equatable {
        /// `allow`, `not_covered`, `owner_said_no`, `no_intent`.
        var verdict: String?
        /// What it read: `owner_request`, `rules`, `chat`, `chat_and_rules`.
        var source: String?
        /// `COVERED`, `NOT_COVERED`, `FORBIDDEN`, each 0…1.
        var p: [String: Double]?
        var rules: Rules?
        var ownerMessages: [String]?
        var model: String?
        var latencyMs: Int?
        var cost: Double?
        var rule: String?

        struct Rules: Decodable, Equatable {
            /// Revisions of the rules for every agent and the agent's own.
            var host: String?
            var agent: String?
        }
    }

    var args: Args? { details?.args }
    /// The gatekeeper's judgment: a browser step's is in its args (its
    /// details' `gate` may be only the host's `owner_on_host` mark), any
    /// other's in the details.
    var gate: Gate? { details?.args?.gate ?? details?.gate }
    var isRefused: Bool { decision != "allow" }

    /// The owner looking, not acting: `rules show`, `vault list`/`show`,
    /// `plugins list`, `status`, `audit` (the app's own reads among them).
    /// Home leaves them out; Activity shows them with connections.
    var isOwnerRead: Bool {
        guard agent == nil else { return false }
        switch op {
        case "status", "audit": return true
        case "rules": return args?.command == "show"
        case "vault": return ["list", "show"].contains(args?.command)
        case "plugins": return args?.command == "list"
        default: return false
        }
    }

    /// A connection or an owner's read: shown in Activity only on request.
    var isBackground: Bool { (op == "connect" && decision == "allow") || isOwnerRead }
}

/// Any JSON value, kept as latch wrote it.
nonisolated enum JSONValue: Decodable, Equatable, Sendable {
    case string(String), number(Double), bool(Bool), null
    case array([JSONValue]), object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let value = try? c.decode(Bool.self) { self = .bool(value) }
        else if let value = try? c.decode(Double.self) { self = .number(value) }
        else if let value = try? c.decode(String.self) { self = .string(value) }
        else if let value = try? c.decode([JSONValue].self) { self = .array(value) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    /// In words, one line per item: scalars as they are, an object as
    /// "key: value" pairs.
    var text: String {
        switch self {
        case let .string(value): value
        case let .number(value): value == value.rounded() && abs(value) < 1e15 ? String(Int64(value)) : String(value)
        case let .bool(value): value ? "Yes" : "No"
        case .null: "—"
        case let .array(values): values.isEmpty ? "None" : values.map(\.text).joined(separator: "\n")
        case let .object(fields):
            fields.keys.sorted().map { "\($0.replacingOccurrences(of: "_", with: " ")): \(fields[$0]!.inline)" }.joined(separator: ", ")
        }
    }

    private var inline: String {
        if case let .array(values) = self { return values.map(\.inline).joined(separator: ", ") }
        if case let .object(fields) = self { return "{\(JSONValue.object(fields).text)}" }
        return text
    }
}

/// A line of `latch audit`: the opening, a record each, then caught up;
/// with `--follow`, records keep coming.
nonisolated enum AuditLine: Decodable, Equatable {
    case hello
    /// The cursor before the first record shown, and whether older records exist.
    case snapshot(cursor: String, hasOlder: Bool)
    case record(cursor: String, AuditRecord)
    case caughtUp(cursor: String, hasOlder: Bool)
    /// A line type this app doesn't know yet.
    case other

    private enum CodingKeys: String, CodingKey { case type, cursor, hasOlder, record }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "hello": self = .hello
        case "snapshot":
            self = .snapshot(cursor: try c.decode(String.self, forKey: .cursor),
                             hasOlder: try c.decodeIfPresent(Bool.self, forKey: .hasOlder) ?? false)
        case "record":
            self = .record(cursor: try c.decode(String.self, forKey: .cursor), try c.decode(AuditRecord.self, forKey: .record))
        case "caught_up":
            self = .caughtUp(cursor: try c.decode(String.self, forKey: .cursor),
                             hasOlder: try c.decodeIfPresent(Bool.self, forKey: .hasOlder) ?? false)
        default: self = .other
        }
    }
}

/// A page of `latch audit` without `--follow`: its records, oldest first.
nonisolated struct AuditPage: Equatable {
    var records: [AuditRecord]
    var hasOlder: Bool

    static func parse(_ data: Data) throws -> AuditPage {
        var page = AuditPage(records: [], hasOlder: false)
        for line in data.split(separator: UInt8(ascii: "\n")) where !line.isEmpty {
            switch try LatchJSON.decoder.decode(AuditLine.self, from: Data(line)) {
            case let .snapshot(_, hasOlder), let .caughtUp(_, hasOlder): page.hasOlder = hasOlder
            case let .record(_, record): page.records.append(record)
            case .hello, .other: break
            }
        }
        return page
    }
}

/// A line of `latch status --local --follow`.
nonisolated enum StatusLine: Decodable {
    case status(LocalStatus)
    case other

    private enum CodingKeys: String, CodingKey { case type, status }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self = try c.decode(String.self, forKey: .type) == "status" ? .status(try c.decode(LocalStatus.self, forKey: .status)) : .other
    }
}

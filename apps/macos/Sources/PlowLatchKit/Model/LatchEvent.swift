import Foundation

/// The host's lifecycle as `latch events` reports it.
public enum HostLifecycle: Equatable, Sendable {
    case starting
    case serving
    case stopped
    case error(String)
}

/// One verified audit record, as the activity stream carries it. `id` is
/// stable across replays, so a resumed stream can drop what it already sent.
public struct ActivityEntry: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var time: Date
    /// The caller's peer key, and its name when the CLI could resolve it.
    public var caller: String
    public var callerName: String?
    public var agent: String?
    public var op: String
    public var decision: String
    public var outcome: String

    public init(
        id: String, time: Date, caller: String, callerName: String? = nil,
        agent: String? = nil, op: String, decision: String, outcome: String
    ) {
        self.id = id
        self.time = time
        self.caller = caller
        self.callerName = callerName
        self.agent = agent
        self.op = op
        self.decision = decision
        self.outcome = outcome
    }
}

/// One line of `latch events --follow`.
public enum LatchEvent: Equatable, Sendable {
    /// The current lifecycle and a bounded replay of recent activity, oldest first.
    case snapshot(cursor: String, host: HostLifecycle, activity: [ActivityEntry])
    case host(cursor: String, HostLifecycle)
    case activity(cursor: String, ActivityEntry)
    /// The cursor could not be honoured; a fresh snapshot follows and replaces
    /// everything the app holds.
    case reset
}

extension LatchEvent: Decodable {
    private enum Keys: String, CodingKey {
        case type, cursor, host, state, error, activity, entry
    }

    private struct HostLine: Decodable {
        var state: String
        var error: String?
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "snapshot":
            let host = try c.decode(HostLine.self, forKey: .host)
            self = .snapshot(
                cursor: try c.decode(String.self, forKey: .cursor),
                host: try Self.lifecycle(host.state, host.error, c),
                activity: try c.decodeIfPresent([ActivityEntry].self, forKey: .activity) ?? [])
        case "host":
            self = .host(
                cursor: try c.decode(String.self, forKey: .cursor),
                try Self.lifecycle(
                    try c.decode(String.self, forKey: .state),
                    try c.decodeIfPresent(String.self, forKey: .error), c))
        case "activity":
            self = .activity(
                cursor: try c.decode(String.self, forKey: .cursor),
                try c.decode(ActivityEntry.self, forKey: .entry))
        case "reset":
            self = .reset
        case let other:
            throw DecodingError.dataCorruptedError(
                forKey: .type, in: c, debugDescription: "unknown event type \(other)")
        }
    }

    private static func lifecycle(
        _ state: String, _ error: String?, _ c: KeyedDecodingContainer<Keys>
    ) throws -> HostLifecycle {
        switch state {
        case "starting": .starting
        case "serving": .serving
        case "stopped": .stopped
        case "error": .error(error ?? "The host stopped with an error.")
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .state, in: c, debugDescription: "unknown host state \(state)")
        }
    }
}

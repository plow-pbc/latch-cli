import Foundation

/// Everything the app asks of latch. The only type that knows a CLI exists;
/// views and the model see typed values.
public protocol LatchClient: Sendable {
    /// The state folder latch is run with; nil is latch's own default.
    var stateDir: String? { get }
    func localStatus() async throws -> LocalStatus
    func rules() async throws -> RulesShow
    /// `latch events --follow`: a snapshot, then changes as they happen.
    /// Ends when the caller stops iterating.
    func events(after cursor: String?, lifecycleOnly: Bool) -> AsyncThrowingStream<LatchEvent, Error>
    /// `latch login --events`: the code to text, then how it ends. Stopping
    /// the iteration cancels the login.
    func login() -> AsyncThrowingStream<LoginEvent, Error>
    /// `latch shares set`: the folders `latch serve` shares from now on.
    func setShares(_ folders: [String]) async throws -> Shares
}

/// Sharing in the background: the host the app keeps running for this
/// Mac, as a launch agent that outlives the app.
public protocol SharingService: Sendable {
    /// Whether the agent is registered with the system, right now.
    func agent() -> SharingAgent
    /// The process launchd runs the agent as, if it is running: the host is
    /// the app's only when latch's serve is this process.
    func agentProcess() async -> Int?
    /// The state folder the agent serves; nil is latch's own default. It
    /// must be the app's own, or the app would show one host and run another.
    var stateDir: String? { get }
    /// Registers the agent, which starts `latch serve`. Registered is not
    /// serving: latch's own status says when it is.
    func startSharing() async throws
    /// Unregisters the agent, which stops `latch serve` and keeps it stopped.
    func stopSharing() async throws
}

public enum SharingAgent: String, Sendable {
    /// Not registered: the app is not keeping a host running.
    case off
    case on
    /// Registered, but the person turned it off in System Settings ▸ Login Items.
    case needsApproval
    /// This build has no agent to register.
    case missing
}

/// What an app has when its bundle carries no agent.
public struct UnavailableSharing: SharingService {
    public init() {}

    public func agent() -> SharingAgent { .missing }
    public func agentProcess() async -> Int? { nil }
    public var stateDir: String? { nil }
    public func startSharing() async throws { throw Self.error }
    public func stopSharing() async throws { throw Self.error }

    private static let error = LatchError.unreachable("This build of Plow Latch can't share in the background.")
}

/// Why a call to latch produced no answer.
public enum LatchError: Error, Equatable, Sendable {
    /// latch ran and refused, with its own error.
    case failed(LatchFailure)
    /// latch could not be run, or printed something that isn't its JSON.
    case unreachable(String)

    public var message: String {
        switch self {
        case let .failed(failure): failure.message
        case let .unreachable(message): message
        }
    }
}

enum LatchJSON {
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let date = parseTimestamp(text) else {
                throw DecodingError.dataCorrupted(.init(
                    codingPath: decoder.codingPath, debugDescription: "not a timestamp: \(text)"))
            }
            return date
        }
        return decoder
    }()

    /// latch's timestamps are RFC 3339 in UTC with up to nanoseconds; Foundation
    /// parses at most milliseconds, so the rest of the fraction is dropped.
    static func parseTimestamp(_ text: String) -> Date? {
        guard let dot = text.firstIndex(of: "."),
              let zone = text[dot...].firstIndex(where: { !$0.isNumber && $0 != "." })
        else { return try? Date(text, strategy: .iso8601) }
        let fraction = text[text.index(after: dot)..<zone].prefix(3)
        let millis = fraction.padding(toLength: 3, withPad: "0", startingAt: 0)
        let trimmed = text[..<dot] + "." + millis + text[zone...]
        return try? Date(String(trimmed), strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }
}

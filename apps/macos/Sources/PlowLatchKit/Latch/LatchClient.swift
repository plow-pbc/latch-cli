import Foundation

/// Everything the app asks of latch. The only type that knows a CLI exists;
/// views and the model see typed values.
public protocol LatchClient: Sendable {
    func localStatus() async throws -> LocalStatus
    func rules() async throws -> RulesShow
    /// `latch events --follow`: a snapshot, then changes as they happen.
    /// Ends when the caller stops iterating.
    func events(after cursor: String?, lifecycleOnly: Bool) -> AsyncThrowingStream<LatchEvent, Error>
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

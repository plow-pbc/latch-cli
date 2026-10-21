import Foundation

/// What to text, from `latch login --events`. The code is a credential: the
/// app shows it and never logs it.
public struct LoginCode: Codable, Equatable, Sendable {
    public var code: String
    /// The number to text, E.164, and as people read it.
    public var sendTo: String
    public var sendToDisplay: String
    /// The QR payload, `SMSTO:<number>:<CODE>`, exactly as the CLI's.
    public var smsto: String
    /// Opens Messages with the text filled in. Only ever opened on a click.
    public var smsLink: URL
    public var expiresAt: Date?

    public init(code: String, sendTo: String, sendToDisplay: String, smsto: String, smsLink: URL, expiresAt: Date? = nil) {
        self.code = code
        self.sendTo = sendTo
        self.sendToDisplay = sendToDisplay
        self.smsto = smsto
        self.smsLink = smsLink
        self.expiresAt = expiresAt
    }
}

/// One line of `latch login --events`.
public enum LoginEvent: Equatable, Sendable {
    case code(LoginCode)
    case waiting
    /// Logged in; this Mac's identity.
    case verified(LocalStatus.Peer)
    /// The code ran out before anyone texted it.
    case expired
    case error(LatchFailure)
}

extension LoginEvent: Decodable {
    private enum Keys: String, CodingKey { case type, peer, error }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "code": self = .code(try LoginCode(from: decoder))
        case "waiting": self = .waiting
        case "verified": self = .verified(try c.decode(LocalStatus.Peer.self, forKey: .peer))
        case "expired": self = .expired
        case "error": self = .error(try c.decode(LatchFailure.self, forKey: .error))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown login event \(other)")
        }
    }
}

/// `latch shares set`'s answer: the folders the host will share.
public struct Shares: Codable, Equatable, Sendable {
    public var shares: [String]

    public init(shares: [String]) {
        self.shares = shares
    }
}

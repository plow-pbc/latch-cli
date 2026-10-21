import Foundation

/// What `latch status --local` reports about this Mac's latch: works before
/// login and offline, so "not logged in" and "not serving" are data, not errors.
public struct LocalStatus: Codable, Equatable, Sendable {
    public var stateDir: String
    public var version: String
    public var `protocol`: Int
    /// This Mac's identity, or nil when it is not logged in.
    public var peer: Peer?
    public var serve: Serve
    public var shares: [String]
    public var lastError: LatchFailure?

    public struct Peer: Codable, Equatable, Sendable {
        public var name: String
        public var key: String
        public var kind: String

        public init(name: String, key: String, kind: String) {
            self.name = name
            self.key = key
            self.kind = kind
        }
    }

    public struct Serve: Codable, Equatable, Sendable {
        public var running: Bool
        public var ready: Bool
        public var pid: Int?

        public init(running: Bool, ready: Bool, pid: Int? = nil) {
            self.running = running
            self.ready = ready
            self.pid = pid
        }
    }

    public init(
        stateDir: String, version: String, protocol: Int, peer: Peer?,
        serve: Serve, shares: [String], lastError: LatchFailure? = nil
    ) {
        self.stateDir = stateDir
        self.version = version
        self.protocol = `protocol`
        self.peer = peer
        self.serve = serve
        self.shares = shares
        self.lastError = lastError
    }
}

/// The CLI's error object: `{"error": {"code", "message", ...}}` on stderr,
/// or a `last_error` inside a status.
public struct LatchFailure: Codable, Equatable, Sendable, Error {
    public var code: String
    public var message: String

    public init(code: String, message: String) {
        self.code = code
        self.message = message
    }
}

import Foundation

/// `latch status --local`: what this Mac knows about its own latch.
nonisolated struct LocalStatus: Decodable, Equatable {
    struct Peer: Decodable, Equatable {
        var name: String
        /// This machine's key: the `caller` on records of its own requests.
        var key: String?
    }

    struct Serve: Decodable, Equatable {
        /// `starting`, `serving`, `stopped` or `error`.
        var state: String
        /// A `latch serve` holds this state folder.
        var running: Bool
        /// It is online and accepting callers.
        var ready: Bool
        /// Run as this user's service: `serve -d` (in the background), or a
        /// `latch serve` held by a terminal (`attached`).
        var managed: Bool
        var attached: Bool?
        /// Set when a running host can't be used, e.g. another latch version's.
        var blocked: LatchFailure?
    }

    struct Update: Decodable, Equatable {
        var available: Bool
        var build: String?
    }

    var peer: Peer?
    var serve: Serve
    var lastError: LatchFailure?
    var update: Update?
}

/// `latch status`: the account's hosts, as Plow's directory lists them.
nonisolated struct AccountStatus: Decodable, Equatable {
    struct Host: Decodable, Equatable {
        var key: String
        var name: String
        /// When Plow last heard from it, and whether it counts it online.
        var lastSeenAt: Date?
        var online: Bool?
        /// This machine, the one that ran `latch status`.
        var isThisMac: Bool

        private enum CodingKeys: String, CodingKey {
            case key, name, lastSeenAt, online
            case isThisMac = "self"
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            key = try c.decode(String.self, forKey: .key)
            name = try c.decode(String.self, forKey: .name)
            lastSeenAt = try c.decodeIfPresent(Date.self, forKey: .lastSeenAt)
            online = try c.decodeIfPresent(Bool.self, forKey: .online)
            isThisMac = try c.decodeIfPresent(Bool.self, forKey: .isThisMac) ?? false
        }
    }

    var hosts: [Host]
}

/// What to text, from `latch login --events`. The code is a credential:
/// shown, never logged.
nonisolated struct LoginCode: Decodable, Equatable {
    var code: String
    var sendToDisplay: String
    /// The QR payload, `SMSTO:<number>:<CODE>`, exactly as the CLI's.
    var smsto: String
    /// Opens Messages with the text filled in; only ever on a click.
    var smsLink: URL
    var expiresAt: Date?
}

/// An event of `latch login --events`.
nonisolated enum LoginEvent: Equatable {
    case code(LoginCode)
    case waiting
    case verified(name: String)
    case expired
    case error(LatchFailure)
}

/// A line of `latch login --events`: its event, or nil for a line the app
/// doesn't act on, the opening `hello` and any type a newer latch adds.
nonisolated struct LoginLine: Decodable {
    var event: LoginEvent?

    private enum Keys: String, CodingKey { case type, peer, error }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "code": event = .code(try LoginCode(from: decoder))
        case "waiting": event = .waiting
        case "verified": event = .verified(name: try c.decode(LocalStatus.Peer.self, forKey: .peer).name)
        case "expired": event = .expired
        case "error": event = .error(try c.decode(LatchFailure.self, forKey: .error))
        default: event = nil
        }
    }
}

/// `latch rules show`: the rules for every agent on a host, and each
/// agent's own rules there, each with the revision a save checks against.
nonisolated struct RulesShow: Decodable, Equatable {
    struct Text: Decodable, Equatable {
        var text: String
        var revision: String?
    }
    struct Agent: Decodable, Equatable {
        var agentName: String?
        var agentUid: String?
        var text: String?
        var revision: String?
    }
    /// Agents the host refuses exec (`serve --deny-exec`).
    struct DenyExec: Decodable, Equatable {
        var everyAgent: Bool?
        var agentUids: [String]?
    }
    var everyAgent: Text?
    var agents: [Agent]?
    var denyExec: DenyExec?

    func agent(_ uid: String) -> Agent? { agents?.first { $0.agentUid == uid } }

    /// Whether the host refuses this agent exec.
    func execBlocked(_ uid: String) -> Bool {
        denyExec?.everyAgent == true || denyExec?.agentUids?.contains(uid) == true
    }
}

/// `latch agents [--host NAME]`: the account's agents and, with a host,
/// each one's standing there.
nonisolated struct AgentsList: Decodable, Equatable {
    struct Agent: Decodable, Equatable, Identifiable {
        struct Line: Decodable, Equatable { var number: String?; var name: String? }
        struct Standing: Decodable, Equatable {
            /// It has rules of its own on that host.
            var ownRules: Bool?
            /// The host refuses it exec (`serve --deny-exec`).
            var execBlocked: Bool?
        }
        var uid: String
        var name: String
        /// `own` or `cloud`.
        var kind: String?
        var cloudKind: String?
        var line: Line?
        /// Plow's word on a cloud agent (e.g. `provisioning`); nil for own.
        var status: String?
        /// As Plow writes them (UTC, no zone); shown as given.
        var createdAt: String?
        var lastSeenAt: String?
        var standing: Standing?

        var id: String { uid }
    }
    var agents: [Agent]
}

/// `latch vault list`: a host's vault items, never their secrets.
nonisolated struct VaultList: Decodable, Equatable {
    struct Item: Decodable, Equatable { var item: String }
    var items: [Item]
}

/// `latch rules try`: the verdict a request would get now, without running it.
nonisolated struct RulesTry: Decodable, Equatable {
    /// `allow`, `not_covered`, `owner_said_no`, `no_intent` or `host_refused`.
    var verdict: String
    /// Why the host refused it before asking the judge, for `host_refused`.
    var refused: LatchFailure?
    /// The gatekeeper's judgment, when it was asked.
    var gate: AuditRecord.Gate?
}

/// `latch setup --status`: each setup step as the state folder says, with
/// no host needed.
nonisolated struct SetupStatus: Decodable, Equatable {
    struct Step: Decodable, Equatable {
        /// `login`, a connector's name, or `host`.
        var step: String
        /// `done`, `needed` or `blocked`.
        var state: String
        /// Why, in latch's words.
        var reason: String
        /// The words after `latch` that do this step, while it isn't done.
        var args: [String]?
        /// A connector's: its owner turned it on.
        var enabled: Bool?
        /// Google's: the accounts connected in Plow, when Plow said.
        var accounts: [String]?
        /// Google's, when none is connected in Plow: where to connect one.
        var connectUrl: URL?
        /// Messages' and Contacts', when asked: whether Plow Latch has Full
        /// Disk Access (`granted`, `not_granted` or `unknown`).
        var access: String?
    }
    var steps: [Step]

    func step(_ name: String) -> Step? { steps.first { $0.step == name } }
}

/// `latch plugins list`: each connector on a host and whether it's on.
nonisolated struct PluginsList: Decodable, Equatable {
    struct Plugin: Decodable, Equatable {
        var name: String
        /// `enabled`, `disabled` or `needs_setup`.
        var state: String
    }
    var plugins: [Plugin]
}

/// `latch plugins enable google`'s answer.
nonisolated struct GoogleEnabled: Decodable, Equatable {
    var accounts: [String]
}

nonisolated enum LatchJSON {
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let date = parseTimestamp(text) else {
                throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "not a timestamp: \(text)"))
            }
            return date
        }
        return decoder
    }()

    /// latch's timestamps are RFC 3339 in UTC with up to nanoseconds;
    /// Foundation parses at most milliseconds, so the rest is dropped.
    static func parseTimestamp(_ text: String) -> Date? {
        guard let dot = text.firstIndex(of: "."),
              let zone = text[dot...].firstIndex(where: { !$0.isNumber && $0 != "." })
        else { return try? Date(text, strategy: .iso8601) }
        let millis = text[text.index(after: dot)..<zone].prefix(3).padding(toLength: 3, withPad: "0", startingAt: 0)
        let trimmed = text[..<dot] + "." + millis + text[zone...]
        return try? Date(String(trimmed), strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }
}

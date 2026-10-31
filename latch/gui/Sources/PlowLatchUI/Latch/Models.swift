import Foundation

/// `latch status --local`: what this Mac knows about its own latch.
nonisolated struct LocalStatus: Decodable, Equatable {
    /// This machine's login, as Plow calls it: `device` or `agent`.
    struct Login: Decodable, Equatable {
        var kind: String
    }

    /// The machine whose status this is: its name and key once logged in
    /// (the key is the `caller` on records of its own requests).
    struct ThisMachine: Decodable, Equatable {
        var name: String?
        var key: String?
        var hosting: Hosting
    }

    struct Hosting: Decodable, Equatable {
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
        var pid: Int?
        /// Set when a running host can't be used, e.g. another latch version's.
        var blocked: LatchFailure?
        /// The keep-awake switch; nil when its saved settings can't be read.
        var keepAwake: KeepAwake?
    }

    /// "Keep this Mac awake while plugged in" (`latch settings set
    /// --keep-awake`): the saved choice, and what the host makes of it.
    struct KeepAwake: Decodable, Equatable {
        /// The saved choice, which the switch shows.
        var enabled: Bool
        /// Whether the serving host holds off idle sleep now; nil until a
        /// starting host says.
        var held: Bool?
        /// Why not, when not held: `disabled`, `not_serving`, `on_battery`,
        /// `unsupported`, `refused` or `unknown`.
        var reason: String?

        /// What the host is doing about it, in words; nil when the switch
        /// says it all. `willRun`: a host not running yet is about to start.
        func said(willRun: Bool = false) -> String? {
            guard enabled, held != nil else { return nil }
            if held == true { return "Latch is keeping this Mac awake while it's plugged in." }
            return switch reason {
            case "not_serving" where willRun: "Once Latch is running, it keeps this Mac awake while it's plugged in."
            case "not_serving": "Latch isn't running, so this Mac sleeps as usual."
            case "on_battery": "On battery, this Mac sleeps as usual."
            case "refused": "macOS didn't let Latch keep this Mac awake."
            case "unknown": "Latch can't tell whether this Mac is plugged in, so it sleeps as usual."
            case "unsupported": "This Mac can't be kept awake by Latch."
            default: nil
            }
        }
    }

    struct Update: Decodable, Equatable {
        var available: Bool
        /// The newest build the host's last lookup found, a number.
        var build: Int?
    }

    /// nil before login: logged in is this, never whether a name is known.
    var login: Login?
    var thisMachine: ThisMachine
    /// The bundle's latch, in words: version, commit and build.
    var version: String?
    var lastError: LatchFailure?
    var update: Update?
    /// Each connector here and whether it's on.
    var plugins: [Plugin]?

    /// This machine's name once logged in; nil before login.
    var loggedInAs: String? { login == nil ? nil : thisMachine.name }
}

/// `latch status`: the account's hosts, as Plow's directory lists them;
/// nil before login.
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
    }

    /// Who the account is to Plow. `phones`, `emails` (the Google accounts
    /// connected in Plow) and the credit left are read only on the owner's
    /// login: nil on an agent's.
    struct Account: Decodable, Equatable {
        var displayName: String?
        var phones: [String]?
        var emails: [String]?
        /// US dollars, a decimal as Plow writes it; below zero once spent past.
        var availableBalanceUsd: String?
    }

    var account: Account?
    var hosts: [Host]?
    /// The Plow this machine's latch asks.
    var apiUrl: URL?
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
    private struct Peer: Decodable { var name: String }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "code": event = .code(try LoginCode(from: decoder))
        case "waiting": event = .waiting
        case "verified": event = .verified(name: try c.decode(Peer.self, forKey: .peer).name)
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
        struct Line: Decodable, Equatable {
            var number: String?
            var name: String?
            /// Its persona's contact card (a vCard, photo included); nil
            /// for an unnamed line.
            var cardUrl: String?
        }
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
    struct Field: Decodable, Equatable, Hashable {
        var label: String
        /// A secret: its value never leaves the host.
        var hidden: Bool
        var custom: Bool?
    }
    struct Item: Decodable, Equatable, Identifiable {
        var item: String
        /// Bumped by every change; an edit or removal names the one it saw.
        var revision: Int
        /// `login`, `card`, `identity` or `note` (an import may bring others).
        var kind: String
        var title: String
        var username: String?
        /// A login's sites; for a card or identity, the only ones it fills on.
        var sites: [String]
        var otherTopSites: [String]?
        /// The item's fields by label, and which are secret.
        var fields: [Field]
        /// A login with an authenticator key.
        var totp: Bool

        var id: String { item }
    }
    var items: [Item]
}

/// `latch agents catalog`: the cloud agents Plow offers.
nonisolated struct AgentCatalog: Decodable, Equatable {
    struct Kind: Decodable, Equatable, Identifiable { var id: String; var name: String }
    var kinds: [Kind]
}

/// `latch agents start-by-text`: Plow's number, and what to text it to
/// start each kind of cloud agent.
nonisolated struct AgentStartByText: Decodable, Equatable {
    struct Kind: Decodable, Equatable, Identifiable {
        var id: String
        var name: String
        var phrase: String
        /// For a QR code: a phone's camera opens a text of `phrase` to the number.
        var smsto: String
        /// The same for Messages on a Mac.
        var smsLink: String
    }
    var number: String
    /// The kind Plow offers without asking which.
    var `default`: String?
    var kinds: [Kind]
}

/// `latch devices`: every machine logged in to the account.
nonisolated struct DeviceList: Decodable, Equatable {
    struct Device: Decodable, Equatable, Identifiable {
        var key: String
        var name: String
        var online: Bool?
        var lastSeenAt: Date?
        var id: String { key }
    }
    var devices: [Device]
}

/// `latch logout`'s answer: whether Plow took this machine out of the
/// account. A local logout succeeds even when Plow couldn't be told.
nonisolated struct LogoutResult: Decodable, Equatable {
    struct Device: Decodable, Equatable {
        /// Nil when the state folder, and the name it kept, were gone.
        var name: String?
        var removed: Bool
    }
    /// Nil when there was nothing to take out of the account.
    var device: Device?
}

/// `latch update --check`: whether a newer Latch is published.
nonisolated struct UpdateCheck: Decodable, Equatable {
    struct Release: Decodable, Equatable { var build: Int?; var tag: String? }
    var available: Bool
    /// This latch's build number; nil for a local build.
    var current: Int?
    var latest: Release?
}

/// `latch update`: whether it installed a build; false when this one was
/// already the newest.
nonisolated struct UpdateResult: Decodable, Equatable {
    var updated: Bool
}

/// A vault item as the person wrote it, for `vault add` or `vault edit`:
/// the title and sites go in argv, every value only on stdin.
nonisolated struct VaultDraft: Equatable, Sendable {
    var kind: String
    var title: String
    var sites: [String]
    var otherTopSites: [String]
    /// Values by latch's labels, secrets included; for an edit, only those
    /// that changed (an empty one clears it).
    var values: [String: String]
}

/// `latch vault show ITEM`: an item and the values of its fields that
/// aren't secret, by label.
nonisolated struct VaultShow: Decodable, Equatable {
    var item: VaultList.Item
    var values: [String: String]
}

/// `latch vault import FILE --preview`: what an export holds, never a
/// secret; each row's `pick` is what `--pick` takes.
nonisolated struct ImportPreview: Decodable, Equatable {
    struct Row: Decodable, Equatable, Identifiable {
        var pick: Int
        var kind: String
        var title: String
        var username: String?
        var url: String?
        /// The vault already holds it.
        var duplicate: Bool
        /// It updates an item the vault holds: the fields it changes.
        var changed: [String]?
        var hasPassword: Bool?
        var hasTotp: Bool?
        var warnings: [String]?

        var id: Int { pick }
    }
    struct Skipped: Decodable, Equatable {
        var title: String
        var reason: String
    }
    var source: String
    /// What `--pick --expect` checks the rows still mean; nil from a host
    /// whose latch can't check it.
    var expect: String?
    var items: [Row]
    var skipped: [Skipped]?
}

/// What `latch vault import FILE --pick ROWS` did.
nonisolated struct ImportResult: Decodable, Equatable {
    var saved: Int
    var updated: Int
    var duplicates: Int
    /// Rows that couldn't be saved, and why; the rest were.
    var failed: [ImportPreview.Skipped]
}

/// `latch rules try`: the verdict a request would get now, without running it.
nonisolated struct RulesTry: Decodable, Equatable {
    /// `allow`, `not_covered`, `owner_said_no` or `host_refused`.
    var verdict: String
    /// Why the host refused it before asking the judge, for `host_refused`.
    var refused: LatchFailure?
    /// The gatekeeper's judgment, when it was asked.
    var gate: AuditRecord.Gate?
    /// What was tried: its kind (`exec`, `google`, …).
    var request: Request?

    struct Request: Decodable, Equatable {
        var op: String
    }

    /// The connector the request is for, when it is one's.
    var connector: String? {
        request.map(\.op).flatMap { ["google", "messages", "contacts"].contains($0) ? $0 : nil }
    }
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
        /// Google's: those of them Google wants the owner to sign in with
        /// again, when there are some.
        var needsSignIn: [String]?
        /// Messages' and Contacts', when asked: whether Plow Latch has Full
        /// Disk Access (`granted`, `not_granted` or `unknown`).
        var access: String?
    }
    var steps: [Step]

    func step(_ name: String) -> Step? { steps.first { $0.step == name } }
}

/// One connector on a host, as its `status --local` lists it.
nonisolated struct Plugin: Decodable, Equatable {
    var name: String
    /// `enabled`, `disabled` or `needs_setup`.
    var state: String
    /// Why, in latch's words, and the command that turns it on.
    var reason: String?
    var enable: String?
}

/// `latch plugins enable google`'s answer.
nonisolated struct GoogleEnabled: Decodable, Equatable {
    /// The accounts that work.
    var accounts: [String]
    /// The rest, which Google wants the owner to sign in with again.
    var needsSignIn: [String]?
}

/// An event of `latch plugins enable google --events`.
nonisolated enum GoogleSignInEvent: Equatable {
    /// A single-use Google sign-in link, a credential until used: opened in
    /// the browser, never shown, saved or logged. A later one replaces it.
    case link(URL)
    /// Google is on, with these accounts.
    case enabled(GoogleEnabled)
}

/// A line of `latch plugins enable google --events`: its event, or nil for
/// the opening `hello`, the closing `error` (latch's error comes on stderr
/// as for any command) and any type a newer latch adds.
nonisolated struct GoogleSignInLine: Decodable {
    var event: GoogleSignInEvent?

    private enum Keys: String, CodingKey { case type, url, result }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "link": event = .link(try c.decode(URL.self, forKey: .url))
        case "result": event = .enabled(try c.decode(GoogleEnabled.self, forKey: .result))
        default: event = nil
        }
    }
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

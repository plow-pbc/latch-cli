import Foundation

/// The account's own pages: no host owns them.
enum AccountPage: String, CaseIterable, Identifiable {
    case agents, connections

    var id: String { rawValue }

    var title: String {
        switch self {
        case .agents: "Agents"
        case .connections: "Connections"
        }
    }

    var symbol: String {
        switch self {
        case .agents: "person.2"
        case .connections: "link"
        }
    }
}

/// A host's screens, each shown for one host.
enum HostScreen: String, CaseIterable, Identifiable {
    case home, activity, rules, vault, settings

    var id: String { rawValue }
    /// The screens with an inspector.
    static let inspected: Set<HostScreen> = [.activity, .vault]

    var title: String {
        switch self {
        case .home: "Home"
        case .activity: "Activity"
        case .rules: "Rules"
        case .vault: "Vault"
        case .settings: "Host Settings"
        }
    }

    var symbol: String {
        switch self {
        case .home: "house"
        case .activity: "waveform.path.ecg"
        case .rules: "list.bullet.rectangle"
        case .vault: "lock.rectangle.stack"
        case .settings: "gearshape"
        }
    }
}

/// The main window's place: an account page, or one of the host
/// workspace's screens. As text ("agents", "home"), it's what the window's
/// scene state keeps.
enum Place: Hashable, RawRepresentable {
    case account(AccountPage)
    case host(HostScreen)

    init?(rawValue: String) {
        if let page = AccountPage(rawValue: rawValue) { self = .account(page) }
        else if let screen = HostScreen(rawValue: rawValue) { self = .host(screen) }
        else { return nil }
    }

    var rawValue: String {
        switch self {
        case let .account(page): page.rawValue
        case let .host(screen): screen.rawValue
        }
    }
}

/// What the main window shows: an account page, with no host, or a host's
/// screen on the one host it shows (nil while there's none).
enum Route: Equatable {
    case account(AccountPage)
    case host(HostScreen, HostEntry?)

    var title: String {
        switch self {
        case let .account(page): page.title
        case let .host(screen, _): screen.title
        }
    }
}

/// An agent on a host, as a link to its rules or activity there names it.
struct AgentTarget: Equatable {
    var host: String
    var agent: String
}

/// Something selected on one host (a record, a vault item): it's shown only
/// while that host is.
struct OnHost: Equatable {
    var host: String
    var id: String
}

/// One of the account's hosts, as the sidebar lists them: this Mac, or
/// another.
struct HostEntry: Equatable, Identifiable {
    var name: String
    var isThisMac: Bool
    /// For another host, Plow's word: whether it's online, and when Plow
    /// last heard from it. This Mac's state is its own `status --local`.
    var online: Bool?
    var lastSeen: Date?
    /// Its key, as Plow lists it: the `caller` on records of requests the
    /// host made of itself.
    var key: String?

    var id: String { name }

    /// The account's hosts, this Mac first, then by name. This Mac is a
    /// host once it has served (Plow lists it), while it serves, and,
    /// logged in, whenever Plow hasn't answered: only Plow's list can say
    /// it never served, so until then it keeps its place, running or not.
    static func list(local: LocalStatus?, account: AccountStatus?) -> [HostEntry] {
        var hosts = (account?.hosts ?? []).map {
            HostEntry(name: $0.name, isThisMac: $0.isThisMac, online: $0.online, lastSeen: $0.lastSeenAt, key: $0.key)
        }
        if let local, let name = local.loggedInAs, local.thisMachine.hosting.running || account == nil, !hosts.contains(where: \.isThisMac) {
            hosts.removeAll { $0.name == name }
            hosts.append(HostEntry(name: name, isThisMac: true, key: local.thisMachine.key))
        }
        return hosts.sorted { ($0.isThisMac ? 0 : 1, $0.name) < ($1.isThisMac ? 0 : 1, $1.name) }
    }
}

extension HostEntry {
    /// How an error sentence names it: "this Mac" for this Mac's own host.
    var spoken: String { isThisMac ? ErrorCopy.thisMac : name }
}

/// A host's state in a word or two, as the sidebar and Home show it.
enum HostState: Equatable {
    case serving, notReady, starting, stopped, failed, blocked
    case online, offline, unknown

    init(_ host: HostEntry, local: LocalStatus?) {
        if host.isThisMac {
            guard let serve = local?.thisMachine.hosting else { self = .unknown; return }
            if serve.blocked != nil { self = .blocked; return }
            self = switch serve.state {
            case "serving": serve.ready ? .serving : .notReady
            case "starting": .starting
            case "error": .failed
            default: .stopped
            }
        } else {
            self = switch host.online {
            case true?: .online
            case false?: .offline
            case nil: .unknown
            }
        }
    }

    var title: String {
        switch self {
        case .serving: "Serving"
        case .notReady: "Not ready"
        case .starting: "Starting…"
        case .stopped: "Stopped"
        case .failed: "Stopped after an error"
        case .blocked: "Can't be used"
        case .online: "Online"
        case .offline: "Offline"
        case .unknown: "Checking…"
        }
    }

    var isUp: Bool { self == .serving || self == .online }
    var isProblem: Bool { self == .failed || self == .blocked }
}

extension HostEntry {
    /// "This Mac · Serving", or "Online · seen 3:06 PM".
    func subtitle(local: LocalStatus?) -> String {
        let state = HostState(self, local: local).title
        if isThisMac { return "This Mac · \(state)" }
        return seenText.map { "\(state) · \($0)" } ?? state
    }

    /// When Plow last heard from another host; Plow gives no time for one
    /// it counts offline.
    var seenText: String? {
        guard let lastSeen else { return nil }
        let time = lastSeen.formatted(date: .omitted, time: .shortened)
        if Calendar.current.isDateInToday(lastSeen) { return "seen \(time)" }
        return "seen \(lastSeen.formatted(.dateTime.month(.abbreviated).day())), \(time)"
    }
}

/// `latch status` failed, so the account's other hosts aren't known: in
/// plain words, Plow being out of reach above all.
struct PlowNotice: Equatable {
    var title: String
    var detail: String
    /// What Plow or latch said, when the title doesn't: shown in the app's
    /// words, with latch's under Details.
    var error: LatchError?

    init(error: LatchError, listed: Bool) {
        let others = listed ? "Your other hosts are shown as Plow last listed them."
            : "Your other hosts can't be listed until it can."
        if error.code == "plow_unreachable" {
            title = "Plow can't be reached right now."
            detail = "\(others) This Mac's own host works as usual."
        } else {
            title = "Couldn't list your hosts."
            detail = others
            self.error = error
        }
    }
}

/// What a host said of itself, its rules and its vault, each read on its
/// own: one can fail while the others answer; nil until it has answered.
struct HostSummary: Equatable {
    /// Another host's own `status --local`, its plugins among it.
    var status: Result<LocalStatus, LatchError>?
    var rules: Result<RulesShow, LatchError>?
    var vault: Result<VaultList, LatchError>?
}

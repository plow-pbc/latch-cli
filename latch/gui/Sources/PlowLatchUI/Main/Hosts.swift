import Foundation

/// The main window's sidebar: the account tier (Agents, Connections), then
/// the selected host's screens.
enum SidebarItem: String, CaseIterable, Identifiable {
    case agents, connections, home, activity, rules, vault, settings

    var id: String { rawValue }
    static let account: [SidebarItem] = [.agents, .connections]
    static let host: [SidebarItem] = [.home, .activity, .rules, .vault, .settings]

    var title: String {
        switch self {
        case .agents: "Agents"
        case .connections: "Connections"
        case .home: "Home"
        case .activity: "Activity"
        case .rules: "Rules"
        case .vault: "Vault"
        case .settings: "Host Settings"
        }
    }

    var symbol: String {
        switch self {
        case .agents: "person.2"
        case .connections: "link"
        case .home: "house"
        case .activity: "waveform.path.ecg"
        case .rules: "list.bullet.rectangle"
        case .vault: "lock.rectangle.stack"
        case .settings: "gearshape"
        }
    }
}

/// A host the picker offers: this Mac, or another of the account's hosts.
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
        if let local, let peer = local.peer, local.serve.running || account == nil, !hosts.contains(where: \.isThisMac) {
            hosts.removeAll { $0.name == peer.name }
            hosts.append(HostEntry(name: peer.name, isThisMac: true, key: peer.key))
        }
        return hosts.sorted { ($0.isThisMac ? 0 : 1, $0.name) < ($1.isThisMac ? 0 : 1, $1.name) }
    }
}

extension HostEntry {
    /// How an error sentence names it: "this Mac" for this Mac's own host.
    var spoken: String { isThisMac ? ErrorCopy.thisMac : name }
}

/// A host's state in a word or two, as the picker and Home show it.
enum HostState: Equatable {
    case serving, notReady, starting, stopped, failed, blocked
    case online, offline, unknown

    init(_ host: HostEntry, local: LocalStatus?) {
        if host.isThisMac {
            guard let serve = local?.serve else { self = .unknown; return }
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

/// What a host said of its rules, vault and connectors, each read on its
/// own: one can fail while the others answer; nil until it has answered.
struct HostSummary: Equatable {
    /// Another host's own `status --local`; Host Settings reads it.
    var status: Result<LocalStatus, LatchError>?
    var rules: Result<RulesShow, LatchError>?
    var vault: Result<VaultList, LatchError>?
    var plugins: Result<PluginsList, LatchError>?
}

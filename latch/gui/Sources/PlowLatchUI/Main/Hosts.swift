import Foundation

/// The main window's sidebar: the account tier (Agents), then the selected
/// host's screens.
enum SidebarItem: String, CaseIterable, Identifiable {
    case agents, home, activity, rules, vault, settings

    var id: String { rawValue }
    static let account: [SidebarItem] = [.agents]
    static let host: [SidebarItem] = [.home, .activity, .rules, .vault, .settings]

    var title: String {
        switch self {
        case .agents: "Agents"
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

    var id: String { name }

    /// The account's hosts, this Mac first, then by name. This Mac is a
    /// host once it has served (Plow lists it), or while it serves before
    /// Plow has answered.
    static func list(local: LocalStatus?, account: AccountStatus?) -> [HostEntry] {
        var hosts = (account?.hosts ?? []).map {
            HostEntry(name: $0.name, isThisMac: $0.isThisMac, online: $0.online, lastSeen: $0.lastSeenAt)
        }
        if let local, let peer = local.peer, local.serve.running, !hosts.contains(where: \.isThisMac) {
            hosts.removeAll { $0.name == peer.name }
            hosts.append(HostEntry(name: peer.name, isThisMac: true))
        }
        return hosts.sorted { ($0.isThisMac ? 0 : 1, $0.name) < ($1.isThisMac ? 0 : 1, $1.name) }
    }
}

/// A host's state in a word or two, as the picker and Home show it.
enum HostState: Equatable {
    case serving, starting, stopped, failed, blocked
    case online, offline, unknown

    init(_ host: HostEntry, local: LocalStatus?) {
        if host.isThisMac {
            guard let serve = local?.serve else { self = .unknown; return }
            if serve.blocked != nil { self = .blocked; return }
            self = switch serve.state {
            case "serving": .serving
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

/// What a host said of its rules, vault and connectors, each read on its
/// own: one can fail while the others answer; nil until it has answered.
struct HostSummary: Equatable {
    var rules: Result<RulesShow, LatchError>?
    var vault: Result<VaultList, LatchError>?
    var plugins: Result<PluginsList, LatchError>?
}

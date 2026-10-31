import SwiftUI

/// The account's agents: a list of them, and the one picked in full, with
/// Message, a row for each host that opens its rules and its requests
/// there, and Copy and Remove… in its ⋯ menu. New Agent is here too.
struct AgentsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(spacing: 0) {
            // The page's own button, above it: the window's toolbar is the same on
            // every page.
            HStack {
                Spacer()
                Button("New Agent", systemImage: "plus") { app.sheet = .newAgent(NewAgent()) }
                    .help("Start a cloud agent by text")
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 10)
            Divider()
            Group {
                if let agents = app.agents {
                    if agents.isEmpty {
                        ContentUnavailableView("No agents yet", systemImage: "person.2",
                                               description: Text("Agents you create, run by Plow or by you, show up here."))
                    } else {
                        HStack(spacing: 0) {
                            AgentList(agents: agents)
                                .frame(width: 240)
                            Divider()
                            if let agent = app.shownAgent {
                                AgentDetail(agent: agent).id(agent.uid)
                            }
                        }
                        .safeAreaInset(edge: .top, spacing: 0) {
                            if let error = app.agentRemoveError {
                                RemoveFailed(error: error) { app.agentRemoveError = nil }
                            }
                        }
                    }
                } else if let error = app.agentsError {
                    ContentUnavailableView {
                        Label("Couldn't list your agents", systemImage: "exclamationmark.triangle")
                    } description: {
                        ErrorText(error: error, titled: true)
                    } actions: {
                        Button("Try Again") { Task { await app.refreshAgents() } }
                    }
                } else {
                    ProgressView("Listing your agents…")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

/// Said above the list and the agent, where latch's reason can sit under
/// Details (an alert has no room for it).
private struct RemoveFailed: View {
    var error: LatchError
    var dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top) {
            Label {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Couldn't remove the agent")
                    ErrorText(error: error, titled: true).foregroundStyle(.secondary)
                }
            } icon: {
                Image(systemName: "exclamationmark.triangle").foregroundStyle(.orange)
            }
            Spacer()
            Button("OK", action: dismiss)
        }
        .padding(10)
        .background(.bar)
    }
}

/// Every agent, by name and number; one picked shows beside it.
private struct AgentList: View {
    var agents: [AgentsList.Agent]
    @Environment(AppModel.self) private var app

    var body: some View {
        List(agents, selection: selection) { agent in
            AgentRow(agent: agent)
        }
        .contextMenu(forSelectionType: AgentsList.Agent.ID.self) { ids in
            if ids.count == 1, let agent = agents.first(where: { ids.contains($0.uid) }) {
                AgentMenuItems(agent: agent)
            }
        }
    }

    private var selection: Binding<AgentsList.Agent.ID?> {
        Binding { app.shownAgent?.uid }
            set: { uid in if let uid { app.selectedAgent = uid } }
    }
}

private struct AgentRow: View {
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        HStack(spacing: 10) {
            Monogram(agent: agent, size: 30)
            VStack(alignment: .leading, spacing: 1) {
                Text(agent.displayName).lineLimit(1)
                if let phone = agent.phone {
                    Text(phone).font(.callout).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([agent.displayName, agent.phone].compactMap { $0 }.joined(separator: ", "))
    }
}

/// One agent in full: who it is, how to reach it, and where it stands on
/// each host.
struct AgentDetail: View {
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .center, spacing: 14) {
                Monogram(agent: agent, size: 56)
                VStack(alignment: .leading, spacing: 4) {
                    Text(agent.displayName).font(.title2.weight(.semibold)).lineLimit(1).textSelection(.enabled)
                    Text(agent.kindName(kinds: app.agentKinds)).font(.callout).foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                if let sms = agent.number?.sms {
                    Button("Message") { app.open(sms) }
                        .buttonStyle(.borderedProminent)
                        .help("Message \(agent.displayName) in Messages")
                }
                Menu {
                    AgentMenuItems(agent: agent)
                } label: {
                    Image(systemName: "ellipsis")
                }
                .menuIndicator(.hidden)
                .fixedSize()
                .help("More")
                .accessibilityLabel("More for \(agent.displayName)")
            }
            .padding(.horizontal, 24)
            .padding(.top, 20)
            .padding(.bottom, 4)
            Form {
                Section {
                    if let phone = agent.phone {
                        LabeledContent("Phone") { Text(phone).monospacedDigit().textSelection(.enabled) }
                    }
                    LabeledContent("Handle") { Text(agent.name).textSelection(.enabled) }
                    if let created = agent.created {
                        LabeledContent("Created", value: created.formatted(date: .abbreviated, time: .omitted))
                    }
                }
                Section("On your hosts") {
                    ForEach(app.hosts) { host in
                        AgentHostRow(agent: agent, host: host)
                    }
                }
            }
            .formStyle(.grouped)
        }
    }
}

/// The ⋯ menu's items, which a row's context menu shares.
struct AgentMenuItems: View {
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        if let number = agent.line?.number, agent.number != nil {
            Button("Copy Phone Number") { app.copy(number) }
        }
        Button("Copy Handle") { app.copy(agent.name) }
        Divider()
        Button("Remove \(agent.displayName)…", role: .destructive) { app.ask(.removeAgent(agent)) }
    }
}

/// An agent on one host: whether the host is up, the agent's standing
/// there, and its rules and its requests there.
private struct AgentHostRow: View {
    var agent: AgentsList.Agent
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        let offline = agent.isOffline(host)
        let state = HostState(host, local: app.status)
        LabeledContent {
            if offline {
                Button("Activity") { app.openActivity(host, agent: agent.uid) }
                    .buttonStyle(.borderless)
                    .help("\(agent.displayName)'s requests to \(host.name) that were read before it went offline")
            } else {
                Menu("Rules & Activity") {
                    Button("Rules on \(host.name)") { app.openRules(host, agent: agent.uid) }
                    Button("Activity on \(host.name)") { app.openActivity(host, agent: agent.uid) }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .help("\(agent.displayName)'s rules and requests on \(host.name)")
            }
        } label: {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                StateDot(state: state).accessibilityLabel(state.title)
                VStack(alignment: .leading, spacing: 2) {
                    Text(host.isThisMac ? "\(host.name) (this Mac)" : host.name)
                    if !offline, case let .failure(error)? = app.standings[host.name] {
                        ErrorText(error: error, host: host.spoken).font(.callout).foregroundStyle(.secondary)
                    } else if let standing = agent.standing(on: host, app.standings[host.name]) {
                        Text(standing).font(.callout).foregroundStyle(.secondary)
                    }
                }
            }
            .opacity(offline ? 0.6 : 1)
        }
    }
}

/// An agent's persona photo, from its line's contact card, or its initial
/// on a tint of its own; with a badge for who runs it.
struct Monogram: View {
    var agent: AgentsList.Agent
    var size: CGFloat
    @Environment(AppModel.self) private var app

    var body: some View {
        ZStack {
            Circle().fill(agent.tint.gradient)
            if let photo = agent.line?.cardUrl.flatMap({ app.agentPhotos[$0] }) {
                Image(decorative: photo, scale: 1).resizable().scaledToFill().clipShape(Circle())
            } else if let initial = agent.displayName.first(where: \.isLetter) {
                Text(String(initial).uppercased())
                    .font(.system(size: size * 0.44, weight: .semibold, design: .rounded))
                    .foregroundStyle(.white)
            } else {
                Image(systemName: "person.fill").font(.system(size: size * 0.44)).foregroundStyle(.white)
            }
        }
        .frame(width: size, height: size)
        .overlay(alignment: .bottomTrailing) {
            Image(systemName: agent.kind == "cloud" ? "cloud.fill" : "house.fill")
                .font(.system(size: size * 0.22, weight: .semibold))
                .foregroundStyle(.secondary)
                .padding(size * 0.07)
                .background(Circle().fill(.background))
                .offset(x: size * 0.08, y: size * 0.08)
        }
        .accessibilityHidden(true)
        .task(id: agent.line?.cardUrl) {
            guard !app.still else { return }
            await app.loadPhoto(of: agent)
        }
    }
}

extension AgentsList.Agent {
    /// The name people know it by: its line's name in Plow, else its
    /// handle, else its number.
    var displayName: String {
        if let name = line?.name?.trimmingCharacters(in: .whitespaces), !name.isEmpty { return name }
        if !name.isEmpty { return name }
        return phone ?? line?.number ?? ""
    }

    /// Its line's number, when it's a phone number.
    var number: PhoneNumber? { line?.number.flatMap(PhoneNumber.init) }

    /// Its line, as people write it.
    var phone: String? { number?.formatted() ?? line?.number }

    /// When Plow made it (Plow writes it in UTC, with no zone).
    var created: Date? { createdAt.flatMap(Self.plowDate) }

    private static func plowDate(_ text: String) -> Date? {
        LatchJSON.parseTimestamp(text.hasSuffix("Z") || text.contains("+") ? text : text + "Z")
    }

    /// Who runs it: for a cloud agent, its kind as Plow's catalog names it.
    func kindName(kinds: [String: String]?) -> String {
        guard kind == "cloud" else { return "Your own agent" }
        return ["Cloud agent", cloudKind.flatMap { kinds?[$0] }].compactMap { $0 }.joined(separator: " · ")
    }

    /// The host is known to be offline, so its standing there can't be read.
    func isOffline(_ host: HostEntry) -> Bool { !host.isThisMac && host.online == false }

    /// Its standing on a host, in words: offline, not read yet, or that it
    /// can't run programs there; nil when there's nothing to say.
    func standing(on host: HostEntry, _ read: Result<[String: Standing], LatchError>?) -> String? {
        if isOffline(host) { return "Offline" }
        switch read {
        case nil: return "Checking…"
        case let .failure(error)?: return ErrorCopy.said(error, host: host.spoken)
        case let .success(byAgent)?: return byAgent[uid]?.execBlocked == true ? "Can't run programs" : nil
        }
    }

    /// Its monogram's tint, the same each time.
    var tint: Color {
        let tints: [Color] = [.blue, .green, .orange, .pink, .purple, .teal, .indigo, .brown]
        return tints[uid.unicodeScalars.reduce(0) { $0 &+ Int($1.value) } % tints.count]
    }
}

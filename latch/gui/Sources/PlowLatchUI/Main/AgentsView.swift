import SwiftUI

/// The account's agents, each with a pill per host it can reach (today,
/// all of them) marking its standing there; a pill opens its rules on that
/// host, a row its page.
struct AgentsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        NavigationStack(path: $app.agentsPath) {
            Group {
                if let agents = app.agents {
                    if agents.isEmpty {
                        ContentUnavailableView("No agents yet", systemImage: "person.2",
                                               description: Text("Agents you create in Plow, cloud or your own, show up here."))
                    } else {
                        List(agents) { agent in
                            NavigationLink(value: agent.uid) { AgentRow(agent: agent) }
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
                    ProgressView("Listing your agents…").frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .navigationTitle("Agents")
            .navigationSubtitle(app.agents.map { "\($0.count) agent\($0.count == 1 ? "" : "s")" } ?? "")
            .navigationDestination(for: String.self) { uid in
                if let agent = app.agents?.first(where: { $0.uid == uid }) {
                    AgentPage(agent: agent)
                } else {
                    ContentUnavailableView("Agent not found", systemImage: "person.crop.circle.badge.questionmark")
                }
            }
        }
        .toolbar {
            ToolbarItem {
                Button("New Agent", systemImage: "plus") { app.newAgent = NewAgent() }
                    .help("Create a cloud agent or your own")
            }
        }
        .sheet(item: $app.newAgent) { NewAgentSheet(flow: $0) }
    }
}

extension NewAgent: Identifiable {}

private struct AgentRow: View {
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: agent.kind == "cloud" ? "cloud" : "person.crop.circle")
                .font(.title2)
                .foregroundStyle(.secondary)
                .frame(width: 28)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(agent.name).font(.headline)
                Text(agent.summary).font(.callout).foregroundStyle(.secondary)
            }
            Spacer(minLength: 16)
            HStack(spacing: 6) {
                ForEach(app.hosts) { host in
                    HostPill(host: host, agent: agent)
                }
            }
        }
        .padding(.vertical, 4)
    }
}

/// One host an agent can reach, marked with its standing there: its own
/// rules (accent dot), exec refused (⦸), or the host offline (dimmed,
/// dashed). Hover names the marks; a click opens its rules there.
struct HostPill: View {
    var host: HostEntry
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        let standing = self.standing
        let offline = !host.isThisMac && host.online == false
        Button {
            app.openRules(host, agent: agent.uid)
        } label: {
            HStack(spacing: 4) {
                if standing?.ownRules == true {
                    Circle().fill(Color.accentColor).frame(width: 6, height: 6)
                }
                Text(host.name)
                if standing?.execBlocked == true {
                    Image(systemName: "nosign").foregroundStyle(.red)
                }
            }
            .font(.callout)
            .padding(.horizontal, 9)
            .padding(.vertical, 3)
            .background(offline ? Color.clear : Color.secondary.opacity(0.12), in: .capsule)
            .overlay {
                Capsule().strokeBorder(Color.secondary.opacity(0.5), style: StrokeStyle(lineWidth: 1, dash: offline ? [3, 2] : []))
                    .opacity(offline ? 1 : 0)
            }
            .opacity(offline ? 0.55 : 1)
        }
        .buttonStyle(.borderless)
        .help(help(offline: offline, standing: standing))
        .accessibilityLabel(help(offline: offline, standing: standing))
    }

    private var standing: AgentsList.Agent.Standing? {
        guard case let .success(byAgent)? = app.standings[host.name] else { return nil }
        return byAgent[agent.uid]
    }

    private func help(offline: Bool, standing: AgentsList.Agent.Standing?) -> String {
        var marks: [String] = []
        if offline { marks.append("\(host.name) is offline") }
        if standing?.ownRules == true { marks.append("\(agent.name) has rules of its own here") }
        if standing?.execBlocked == true { marks.append("\(agent.name) can't run programs on \(host.name)") }
        if !offline, standing == nil { marks.append("Its standing on \(host.name) isn't known yet") }
        if marks.isEmpty { marks.append("\(agent.name) follows the rules for every agent here") }
        return marks.joined(separator: ". ") + ". Click to open its rules on \(host.name)."
    }
}

extension AgentsList.Agent {
    /// "Your own agent · +1 555-000-0002", or a cloud agent's kind and state.
    var summary: String {
        let what = kind == "cloud" ? "Cloud agent" + (cloudKind.map { " · \($0)" } ?? "") : "Your own agent"
        return [what, line?.number, status].compactMap { $0 }.joined(separator: " · ")
    }
}

/// One agent: every host it can reach and its standing on each, and its
/// details from Plow.
struct AgentPage: View {
    var agent: AgentsList.Agent
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            Section {
                Text("Can reach all your hosts. On each, it follows the rules for every agent, and its own rules where it has them.")
                    .foregroundStyle(.secondary)
            }
            Section("On your hosts") {
                ForEach(app.hosts) { host in
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(host.isThisMac ? "\(host.name) (this Mac)" : host.name)
                            if case let .failure(error)? = app.standings[host.name], host.isThisMac || host.online != false {
                                ErrorText(error: error, host: host.spoken).font(.callout).foregroundStyle(.secondary)
                            } else {
                                Text(standingText(on: host)).font(.callout).foregroundStyle(.secondary)
                            }
                        }
                        Spacer()
                        Button("Rules on \(host.name)") {
                            app.openRules(host, agent: agent.uid)
                        }
                    }
                }
            }
            Section("Agent") {
                LabeledContent("Kind", value: agent.kind == "cloud" ? "Cloud agent" + (agent.cloudKind.map { " (\($0))" } ?? "") : "Your own agent")
                LabeledContent("Phone line", value: agent.line?.number ?? "None")
                if let status = agent.status { LabeledContent("Status in Plow", value: status) }
                LabeledContent("ID") { Text(agent.uid).font(.body.monospaced()).textSelection(.enabled) }
            }
        }
        .formStyle(.grouped)
        .navigationTitle(agent.name)
        .navigationSubtitle(agent.summary)
    }

    private func standingText(on host: HostEntry) -> String {
        if !host.isThisMac, host.online == false { return "Offline: its standing there can't be read" }
        switch app.standings[host.name] {
        case nil: return "Checking…"
        case let .failure(error)?: return ErrorCopy.said(error, host: host.spoken)
        case let .success(byAgent)?:
            let standing = byAgent[agent.uid]
            var words = [standing?.ownRules == true ? "Its own rules, and the rules for every agent" : "The rules for every agent"]
            if standing?.execBlocked == true { words.append("can't run programs") }
            return words.joined(separator: " · ")
        }
    }
}

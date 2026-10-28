import SwiftUI

/// The account's agents, each in its own section, always open: a row per
/// host with its standing there and buttons to its rules and its requests
/// on that host. New Agent and Remove… are here too.
struct AgentsView: View {
    @Environment(AppModel.self) private var app
    @State private var position = ScrollPosition()

    var body: some View {
        VStack(spacing: 0) {
            // The page's own button, above it: the window's toolbar is the same on
            // every page.
            HStack {
                Spacer()
                Button("New Agent", systemImage: "plus") { app.sheet = .newAgent(NewAgent()) }
                    .help("Create a cloud agent or your own")
            }
            .padding(.horizontal, 20)
            .padding(.top, 10)
            Group {
                if let agents = app.agents {
                    if agents.isEmpty {
                        ContentUnavailableView("No agents yet", systemImage: "person.2",
                                               description: Text("Agents you create in Plow, cloud or your own, show up here."))
                    } else {
                        Form {
                            if let error = app.agentRemoveError {
                                Section {
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
                                        Button("OK") { app.agentRemoveError = nil }
                                    }
                                }
                            }
                            ForEach(agents) { agent in
                                AgentSection(agent: agent, remove: { app.ask(.removeAgent(agent)) })
                            }
                        }
                        .formStyle(.grouped)
                        // Back where it was left (a Rules or Activity link and
                        // back, another page and back): scrolled there as it
                        // appears, and again as its content changes size, until
                        // the person scrolls. Kept from the top of the content,
                        // under the toolbar's inset, which is where scrollTo
                        // measures from.
                        .scrollPosition($position)
                        .onScrollGeometryChange(for: CGFloat.self, of: { $0.contentSize.height }) { _, _ in
                            if !position.isPositionedByUser { position.scrollTo(y: app.agentsScrolled) }
                        }
                        .onScrollGeometryChange(for: CGFloat.self, of: { $0.contentOffset.y + $0.contentInsets.top }) { _, y in
                            if position.isPositionedByUser { app.agentsScrolled = y }
                        }
                        .onAppear { position.scrollTo(y: app.agentsScrolled) }
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
        }
    }
}

/// One agent: who it is, then a row for each host.
private struct AgentSection: View {
    var agent: AgentsList.Agent
    var remove: () -> Void
    @Environment(AppModel.self) private var app

    var body: some View {
        Section {
            ForEach(app.hosts) { host in
                AgentHostRow(agent: agent, host: host)
            }
        } header: {
            HStack(alignment: .firstTextBaseline) {
                Label {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(agent.name).font(.headline)
                        Text(agent.summary).font(.callout).foregroundStyle(.secondary)
                    }
                } icon: {
                    Image(systemName: agent.kind == "cloud" ? "cloud" : "person.crop.circle")
                }
                Spacer()
                Button("Remove…", role: .destructive, action: remove)
                    .help("Remove \(agent.name) from your account")
                    .accessibilityLabel("Remove \(agent.name)…")
            }
        }
    }
}

/// An agent on one host: its standing there, and its rules and requests.
private struct AgentHostRow: View {
    var agent: AgentsList.Agent
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        LabeledContent {
            HStack {
                Button("Rules") { app.openRules(host, agent: agent.uid) }
                    .help("\(agent.name)'s rules on \(host.name)")
                Button("Activity") { app.openActivity(host, agent: agent.uid) }
                    .help("\(agent.name)'s requests to \(host.name)")
            }
        } label: {
            Text(host.isThisMac ? "\(host.name) (this Mac)" : host.name)
            if case let .failure(error)? = app.standings[host.name], host.isThisMac || host.online != false {
                ErrorText(error: error, host: host.spoken)
            } else {
                Text(standing)
            }
        }
    }

    /// Its standing on the host, in words: offline, not known yet, or
    /// whose rules judge it and whether it may run programs.
    private var standing: String {
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

extension AgentsList.Agent {
    /// "Your own agent · +1 555-000-0002", or a cloud agent's kind and state.
    var summary: String {
        let what = kind == "cloud" ? "Cloud agent" + (cloudKind.map { " · \($0)" } ?? "") : "Your own agent"
        return [what, line?.number, status].compactMap { $0 }.joined(separator: " · ")
    }
}

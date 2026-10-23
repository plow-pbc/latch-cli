import SwiftUI

/// The selected host's settings, and only that host's: its state (and, for
/// this Mac, restart and stop), its version, what it refuses agents, and its
/// plugins (on and off only here, for this Mac).
struct HostSettingsView: View {
    @Environment(AppModel.self) private var app
    @State private var stopping = false

    var body: some View {
        Form {
            if let host = app.host {
                HostSection(host: host, stopping: $stopping)
                if !host.isThisMac && host.online == false {
                    Section {
                        Text("\(host.name) is offline. Its running programs and plugins show here once it's back online.")
                            .foregroundStyle(.secondary)
                    }
                } else {
                    ExecSection(host: host)
                    PluginsSection(host: host)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Host Settings")
        .navigationSubtitle(app.host?.name ?? "")
        .task(id: app.host?.id) {
            guard !app.still else { return }
            await app.refreshSummary()
        }
        .confirmationDialog("Stop Latch on this Mac?", isPresented: $stopping) {
            Button("Stop", role: .destructive) { Task { await app.stopHost() } }
        } message: {
            Text("Your agents can't reach this Mac until you start it again.")
        }
    }
}

/// The host's name and state; this Mac's restarts and stops here.
private struct HostSection: View {
    var host: HostEntry
    @Binding var stopping: Bool
    @Environment(AppModel.self) private var app

    var body: some View {
        let state = HostState(host, local: app.status)
        Section(host.isThisMac ? "This Mac's host" : "Host") {
            LabeledContent("Name", value: host.isThisMac ? "\(host.name) (this Mac)" : host.name)
            LabeledContent("State") {
                HStack(spacing: 6) {
                    StateDot(state: state)
                    Text(stateText(state))
                }
            }
            if host.isThisMac {
                HStack {
                    if let action = app.hostAction {
                        ProgressView().controlSize(.small)
                        Text(action == .restarting ? "Restarting…" : "Stopping…").foregroundStyle(.secondary)
                    }
                    Spacer()
                    if app.status?.serve.running == true {
                        Button("Restart") { Task { await app.restartHost() } }
                            .help("Stop Latch and start it again as it last ran")
                        Button("Stop…") { stopping = true }
                    } else {
                        Button(app.startingHost ? "Starting…" : "Start Latch") { Task { await app.startHosting() } }
                            .disabled(app.startingHost)
                    }
                }
                .disabled(app.hostAction != nil)
                if let error = app.hostActionError ?? app.startHostError {
                    Text(error).foregroundStyle(.red)
                }
                if let status = app.status {
                    if let version = status.version {
                        LabeledContent("Version") { Text(version).textSelection(.enabled).multilineTextAlignment(.trailing) }
                    }
                    DisclosureGroup("Details") {
                        LabeledContent("Key") { Text(status.peer?.key ?? "—").font(.body.monospaced()).textSelection(.enabled) }
                        LabeledContent("State folder") { Text(app.client.stateDir).font(.body.monospaced()).textSelection(.enabled) }
                    }
                }
            } else {
                Text("Start, stop and update \(host.name) on \(host.name) itself.").foregroundStyle(.secondary)
            }
        }
    }

    private func stateText(_ state: HostState) -> String {
        if host.isThisMac {
            if let serve = app.status?.serve, let pid = serve.pid, serve.running {
                let how = serve.managed ? " · in the background" : serve.attached == true ? " · in a terminal" : ""
                return "\(state.title)\(how) · pid \(pid)"
            }
            return state.title
        }
        return host.seenText.map { "\(state.title) · \($0)" } ?? state.title
    }
}

/// Whether the host refuses agents exec (`serve --deny-exec`), as its
/// rules say.
private struct ExecSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        if case let .success(rules)? = app.summaries[host.name]?.rules, let deny = rules.denyExec {
            Section("Running programs") {
                LabeledContent("Agents may run programs", value: text(deny))
            }
        }
    }

    private func text(_ deny: RulesShow.DenyExec) -> String {
        if deny.everyAgent == true { return "No: \(host.name) refuses every agent exec" }
        let uids = deny.agentUids ?? []
        guard !uids.isEmpty else { return "Yes, judged by your rules" }
        let names = uids.map { uid in app.agents?.first { $0.uid == uid }?.name ?? "Agent \(uid.prefix(8))" }
        return "Yes, except \(names.joined(separator: ", "))"
    }
}

/// The host's plugins: on and off here for this Mac; another host's are
/// turned on there. Google's accounts are the account's, in Connections.
private struct PluginsSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app
    @State private var changing: String?
    @State private var error: String?

    var body: some View {
        Section {
            switch app.summaries[host.name]?.plugins {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case .failure?:
                Text("\(host.name) couldn't list its plugins.").foregroundStyle(.secondary)
            case let .success(list)?:
                ForEach(list.plugins, id: \.name) { plugin in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(PluginCopy.title(plugin.name))
                            Group {
                                if host.isThisMac {
                                    Text(PluginCopy.state(plugin))
                                } else if plugin.state != "enabled" {
                                    Text("Turn this on from \(host.name).")
                                }
                            }
                            .font(.callout)
                            .foregroundStyle(.secondary)
                        }
                        Spacer()
                        if plugin.name == "google" {
                            Button("Manage Accounts…") { app.manageAccounts() }
                        }
                        if host.isThisMac {
                            Toggle(PluginCopy.title(plugin.name), isOn: binding(plugin))
                                .labelsHidden()
                                .disabled(changing != nil)
                        } else {
                            Text(plugin.state == "enabled" ? "On" : "Off").foregroundStyle(.secondary)
                        }
                    }
                }
                if let error { Text(error).foregroundStyle(.red) }
            }
        } header: {
            Text("Plugins")
        } footer: {
            if let changing { Text("Turning \(PluginCopy.title(changing)) \(isOn(changing) ? "off" : "on")…").foregroundStyle(.secondary) }
        }
    }

    private func isOn(_ name: String) -> Bool {
        guard case let .success(list)? = app.summaries[host.name]?.plugins else { return false }
        return list.plugins.first { $0.name == name }?.state == "enabled"
    }

    private func binding(_ plugin: PluginsList.Plugin) -> Binding<Bool> {
        Binding { plugin.state == "enabled" } set: { on in
            changing = plugin.name
            Task {
                defer { changing = nil }
                do {
                    try await app.setConnector(plugin.name, on: on, host: host)
                    error = nil
                } catch {
                    self.error = error.localizedDescription
                }
            }
        }
    }
}

/// A plugin in the app's words: latch's reasons and refusals name commands
/// and pages to visit, which the app has buttons for.
enum PluginCopy {
    static func title(_ name: String) -> String {
        name.prefix(1).uppercased() + name.dropFirst()
    }

    /// Under its name: on, off, or what it needs first.
    static func state(_ plugin: PluginsList.Plugin) -> String {
        switch plugin.state {
        case "enabled": "On"
        case "needs_setup": needs(plugin.name)
        default: "Off"
        }
    }

    /// Why turning a plugin on or off failed.
    static func refusal(_ error: LatchError, plugin: String, on: Bool) -> String {
        switch error.code {
        case "google_not_connected"?: "Connect a Google account first, in Connections."
        case "needs_setup"?: needs(plugin) + "."
        default:
            plain(error.message) ? error.message : "Couldn't turn \(title(plugin)) \(on ? "on" : "off")."
        }
    }

    private static func needs(_ plugin: String) -> String {
        switch plugin {
        case "google": "Needs a Google account, in Connections"
        case "messages", "contacts": "Needs Full Disk Access for Plow Latch, in System Settings"
        default: "Needs setting up"
        }
    }

    /// Prose with no command or address in it.
    static func plain(_ message: String) -> Bool {
        !message.contains("`") && !message.contains("://")
    }
}

import SwiftUI

/// The selected host's settings, and only that host's: its state (and, for
/// this Mac, restart and stop), its version (another host's as it says),
/// what it refuses agents, and its plugins (on and off only here, for this
/// Mac). The window's title names the host.
struct HostSettingsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            if let host = app.host {
                HostSection(host: host)
                if host.isThisMac {
                    Section("Sleep") {
                        Toggle(isOn: app.keepAwakeBinding) {
                            Text(KeepAwakeSwitch.title)
                            Text(KeepAwakeSwitch.detail)
                        }
                        .toggleStyle(.switch)
                        .accessibilityLabel(KeepAwakeSwitch.title)
                        .disabled(!app.canSetKeepAwake)
                        KeepAwakeSwitch.Said().foregroundStyle(.secondary)
                    }
                }
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
    }
}

/// The host's state; this Mac's restarts and stops here.
private struct HostSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        let state = HostState(host, local: app.status)
        Section(host.isThisMac ? "This Mac's host" : "Host") {
            LabeledContent("State") {
                HStack(spacing: 6) {
                    StateDot(state: state).accessibilityHidden(true)
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
                    if app.status?.thisMachine.hosting.running == true {
                        Button("Restart") { Task { await app.restartHost() } }
                            .help("Stop Latch and start it again as it last ran")
                        Button("Stop…") { app.ask(.stopHost) }
                    } else {
                        Button(app.startingHost ? "Starting…" : "Start Latch") { Task { await app.startHosting() } }
                            .disabled(app.startingHost)
                    }
                }
                .disabled(app.hostAction != nil)
                if let error = app.hostActionError ?? app.startHostError {
                    ErrorText(error: error).foregroundStyle(.red)
                }
                if let status = app.status {
                    if let version = status.version {
                        LabeledContent("Version") { Text(version).textSelection(.enabled).multilineTextAlignment(.trailing) }
                    }
                    DisclosureGroup("Details") {
                        LabeledContent("Key") { Text(status.thisMachine.key ?? "—").font(.body.monospaced()).textSelection(.enabled) }
                        LabeledContent("State folder") { Text(app.client.stateDir).font(.body.monospaced()).textSelection(.enabled) }
                    }
                }
            } else {
                if host.online != false {
                    LabeledContent("Version") {
                        switch app.summaries[host.name]?.status {
                        case let .success(status)?:
                            Text(status.version ?? "—").textSelection(.enabled).multilineTextAlignment(.trailing)
                        case .failure?: Text("\(host.name) didn't say")
                        case nil: ProgressView().controlSize(.small)
                        }
                    }
                }
                Text("Start, stop and update \(host.name) on \(host.name) itself.").foregroundStyle(.secondary)
            }
        }
    }

    private func stateText(_ state: HostState) -> String {
        if host.isThisMac {
            if let serve = app.status?.thisMachine.hosting, serve.running {
                let how = serve.managed ? " · in the background" : serve.attached == true ? " · in a terminal" : ""
                return "\(state.title)\(how)"
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
        if deny.everyAgent == true { return "No" }
        let uids = deny.agentUids ?? []
        guard !uids.isEmpty else { return "Yes, judged by your rules" }
        let names = uids.map { uid in app.agents?.first { $0.uid == uid }?.name ?? "Agent \(uid.prefix(8))" }
        return "Yes, except \(names.joined(separator: ", "))"
    }
}

/// The host's plugins: on and off here for this Mac, a change at a time;
/// another host's are turned on there. Google's accounts are the
/// account's, in Connections.
private struct PluginsSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        Section {
            switch app.plugins(of: host) {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case .failure?:
                Text("\(host.name) couldn't list its plugins.").foregroundStyle(.secondary)
            case let .success(list)?:
                ForEach(list, id: \.name) { plugin in
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
                                .disabled(app.connectorChange != nil)
                        } else {
                            Text(plugin.state == "enabled" ? "On" : "Off").foregroundStyle(.secondary)
                        }
                    }
                }
                if host.isThisMac, let error = app.connectorError { ErrorText(error: error, host: host.spoken).foregroundStyle(.red) }
            }
        } header: {
            Text("Plugins")
        } footer: {
            if host.isThisMac, let change = app.connectorChange {
                Text("Turning \(PluginCopy.title(change.plugin)) \(change.on ? "on" : "off")…").foregroundStyle(.secondary)
            }
        }
    }

    private func binding(_ plugin: Plugin) -> Binding<Bool> {
        Binding { plugin.state == "enabled" } set: { on in
            Task { await app.setConnector(plugin.name, on: on) }
        }
    }
}

/// A plugin in the app's words: latch's reasons and refusals name commands
/// and pages to visit, which the app has buttons for.
nonisolated enum PluginCopy {
    static func title(_ name: String) -> String {
        name.prefix(1).uppercased() + name.dropFirst()
    }

    /// Under its name: on, off, or what it needs first.
    static func state(_ plugin: Plugin) -> String {
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
        default: "Couldn't turn \(title(plugin)) \(on ? "on" : "off")."
        }
    }

    private static func needs(_ plugin: String) -> String {
        switch plugin {
        case "messages", "contacts": "Needs Full Disk Access for Plow Latch, in System Settings"
        default: "Needs setting up"
        }
    }
}

/// The keep-awake switch, on Start and in Host Settings. It shows the
/// choice latch reads back (`status --local`), never the click, and below
/// it what the host makes of it.
enum KeepAwakeSwitch {
    static let title = "Keep this Mac awake while plugged in"
    static let detail = "The display still sleeps, and closing the lid still sleeps the Mac."

    /// What the host does about the choice, and a failed save. On Start,
    /// with Run Latch now on, a host not running yet is about to be.
    struct Said: View {
        var willRun = false
        @Environment(AppModel.self) private var app

        var body: some View {
            if let error = app.keepAwakeError {
                ErrorText(error: error).foregroundStyle(.red)
            } else if app.status != nil, app.status?.thisMachine.hosting.keepAwake == nil {
                Text("Latch couldn't read this Mac's settings.")
            } else if let keepAwake = app.status?.thisMachine.hosting.keepAwake, let said = keepAwake.said(willRun: willRun) {
                Text(said)
            }
        }
    }
}

extension AppModel {
    var keepAwakeBinding: Binding<Bool> {
        Binding { self.status?.thisMachine.hosting.keepAwake?.enabled ?? false } set: { on in
            Task { await self.setKeepAwake(on) }
        }
    }

    /// Only once latch has said the choice. Not turned off while one saves:
    /// a switch disabled in the middle of its own click can stay drawn
    /// mid-flip, its knob gone (setKeepAwake drops a second click instead).
    var canSetKeepAwake: Bool { status?.thisMachine.hosting.keepAwake != nil }
}

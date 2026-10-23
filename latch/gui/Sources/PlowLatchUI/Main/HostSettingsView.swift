import SwiftUI

/// The selected host's settings: its state (and, for this Mac, restart and
/// stop), what it refuses agents, its connectors (on and off only here,
/// for this Mac), updates, and the account's logged-in machines.
struct HostSettingsView: View {
    @Environment(AppModel.self) private var app
    @State private var stopping = false
    @State private var removingDevice: DeviceList.Device?
    @State private var loggingOut = false
    @State private var logOutError: String?

    var body: some View {
        Form {
            if app.viewerOnly, let me = app.status?.peer {
                Section("This Mac") {
                    HStack {
                        LabeledContent(me.name, value: "Not hosting")
                        Button(app.startingHost ? "Starting…" : "Start Hosting on This Mac…") { app.confirmingStartHosting = true }
                            .disabled(app.startingHost)
                    }
                    if let error = app.startHostError { Text(error).foregroundStyle(.red) }
                }
            }
            if let host = app.host {
                HostSection(host: host, stopping: $stopping)
                ExecSection(host: host)
                ConnectorsSection(host: host)
                UpdatesSection(host: host)
            }
            accountSection
        }
        .formStyle(.grouped)
        .navigationTitle("Host Settings")
        .navigationSubtitle(app.host?.name ?? "")
        .task(id: app.host?.id) {
            guard !app.still else { return }
            await app.refreshSummary()
        }
        .task {
            guard !app.still else { return }
            await app.refreshDevices()
        }
        .confirmationDialog("Stop Latch on this Mac?", isPresented: $stopping) {
            Button("Stop", role: .destructive) { Task { await app.stopHost() } }
        } message: {
            Text("Your agents can't reach this Mac until you start it again.")
        }
        .confirmationDialog("Remove “\(removingDevice?.name ?? "")” from your account?", isPresented: removingBinding,
                            presenting: removingDevice) { device in
            Button("Remove", role: .destructive) {
                Task { await app.removeDevice(device.name) }
            }
        } message: { device in
            Text("\(device.name) is logged out of Plow and can no longer send requests as you.")
        }
        .confirmationDialog("Log out this Mac?", isPresented: $loggingOut) {
            Button("Log Out", role: .destructive) {
                Task {
                    do {
                        try await app.logOut()
                    } catch {
                        logOutError = error.localizedDescription
                    }
                }
            }
        } message: {
            Text("Latch stops here and this Mac leaves your Plow account. Its vault and activity stay, readable after you log in again.")
        }
        .alert("Still in your Plow account", isPresented: logOutWarningBinding) {
            Button("OK") { app.acknowledgeLogOutWarning() }
        } message: {
            Text(app.logOutWarning ?? "")
        }
    }

    @ViewBuilder
    private var accountSection: some View {
        Section {
            switch app.devices {
            case nil:
                LabeledContent("Logged-in devices") { ProgressView().controlSize(.small) }
            case let .failure(error)?:
                LabeledContent("Logged-in devices") { Text(error.message).foregroundStyle(.secondary) }
            case .success?:
                let devices = app.loggedInDevices ?? []
                if devices.isEmpty {
                    LabeledContent("Logged-in devices", value: "None")
                }
                ForEach(devices) { device in
                    let thisMac = device.key == app.status?.peer?.key
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(thisMac ? "\(device.name) (this Mac)" : device.name)
                            Text(seen(device)).font(.callout).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Remove…") { removingDevice = device }
                            .disabled(thisMac)
                            .help(thisMac ? "Log out this Mac instead" : "Remove it from your account")
                    }
                }
            }
            if let error = app.deviceRemoveError { Text(error).foregroundStyle(.red) }
            if let me = app.status?.peer {
                HStack {
                    LabeledContent("Logged in as", value: me.name)
                    Button("Log Out…") { loggingOut = true }
                }
                if let logOutError { Text(logOutError).foregroundStyle(.red) }
            }
        } header: {
            Text("Your account")
        } footer: {
            Text("Logged-in devices can send requests as you but don't host.").foregroundStyle(.secondary)
        }
    }

    private var logOutWarningBinding: Binding<Bool> {
        Binding { app.logOutWarning != nil } set: { if !$0 { app.acknowledgeLogOutWarning() } }
    }

    private var removingBinding: Binding<Bool> {
        Binding { removingDevice != nil } set: { if !$0 { removingDevice = nil } }
    }

    private func seen(_ device: DeviceList.Device) -> String {
        let state = device.online == true ? "Online" : "Offline"
        guard let seen = device.lastSeenAt else { return state }
        return "\(state) · seen \(seen.formatted(date: .abbreviated, time: .shortened))"
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
                    DisclosureGroup("Details") {
                        LabeledContent("Key") { Text(status.peer?.key ?? "—").font(.body.monospaced()).textSelection(.enabled) }
                        LabeledContent("State folder") { Text(app.client.stateDir).font(.body.monospaced()).textSelection(.enabled) }
                        if let version = status.version {
                            LabeledContent("Version") { Text(version).textSelection(.enabled).multilineTextAlignment(.trailing) }
                        }
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

/// The host's connectors: on and off here for this Mac; another host's
/// are turned on there.
private struct ConnectorsSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app
    @State private var changing: String?
    @State private var error: String?

    var body: some View {
        Section {
            switch app.summaries[host.name]?.plugins {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case let .failure(failure)?:
                Text(failure.message).foregroundStyle(.secondary)
            case let .success(list)?:
                ForEach(list.plugins, id: \.name) { plugin in
                    let title = plugin.name.prefix(1).uppercased() + plugin.name.dropFirst()
                    if host.isThisMac {
                        Toggle(isOn: binding(plugin)) {
                            Text(title)
                            Text(reason(plugin))
                        }
                        .disabled(changing != nil)
                    } else {
                        LabeledContent {
                            Text(plugin.state == "enabled" ? "On" : "Off")
                        } label: {
                            Text(title)
                            if plugin.state != "enabled" {
                                Text("Turn this on from \(host.name): `\(plugin.enable ?? "latch plugins enable \(plugin.name)")`")
                            } else {
                                Text(reason(plugin))
                            }
                        }
                    }
                }
                if let error { Text(error).foregroundStyle(.red) }
            }
        } header: {
            Text("Connectors")
        } footer: {
            if let changing { Text("Turning \(changing) \(isOn(changing) ? "off" : "on")…").foregroundStyle(.secondary) }
        }
    }

    private func isOn(_ name: String) -> Bool {
        guard case let .success(list)? = app.summaries[host.name]?.plugins else { return false }
        return list.plugins.first { $0.name == name }?.state == "enabled"
    }

    private func reason(_ plugin: PluginsList.Plugin) -> String {
        guard let reason = plugin.reason, !reason.isEmpty else { return plugin.state == "enabled" ? "On" : "Off" }
        return reason.prefix(1).uppercased() + reason.dropFirst() + "."
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

/// This Mac's Latch version and Check Now; another host updates itself.
private struct UpdatesSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        Section("Updates") {
            if host.isThisMac {
                HStack {
                    LabeledContent("Latch", value: text)
                    Button(app.checkingUpdate ? "Checking…" : "Check Now") { Task { await app.checkForUpdate() } }
                        .disabled(app.checkingUpdate)
                }
                if case let .failure(error)? = app.update {
                    Text(error.message).foregroundStyle(.red)
                }
                if available {
                    Text("Run `latch update` in Terminal to install it; Latch restarts what was running.").foregroundStyle(.secondary)
                }
            } else {
                Text("Run `latch update` on \(host.name) to update it.").foregroundStyle(.secondary)
            }
        }
    }

    private var available: Bool {
        if case let .success(check)? = app.update { return check.available }
        return app.status?.update?.available == true
    }

    private var text: String {
        switch app.update {
        case let .success(check)?:
            if check.available { return "Build \(check.latest?.build.map(String.init) ?? "?") is available" }
            guard let current = check.current else { return "A local build · latest is \(check.latest?.tag ?? "unknown")" }
            return "Up to date · build \(current)"
        default:
            if let update = app.status?.update, update.available { return "Build \(update.build.map(String.init) ?? "?") is available" }
            return "Not checked yet"
        }
    }
}

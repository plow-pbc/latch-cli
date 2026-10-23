import SwiftUI

/// Home for the selected host: what needs the person (this Mac's own host
/// only), and the host's rules, vault and connectors. Another host's
/// activity can't be read from here yet, so Home says where it is.
struct HomeView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            if app.viewerOnly {
                ViewerSection()
            }
            if let host = app.host {
                if host.isThisMac {
                    NeedsSection()
                    OnHostSection(host: host)
                } else if host.online == false {
                    OfflineSection(host: host)
                } else {
                    Section("Activity") {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("Activity on \(host.name) is kept on \(host.name).")
                            Text("Open Latch on \(host.name) to see it.").foregroundStyle(.secondary)
                        }
                    }
                    OnHostSection(host: host)
                }
            } else if app.viewerOnly {
                Section {
                    ContentUnavailableView {
                        Label("No hosts yet", systemImage: "server.rack")
                    } description: {
                        Text("To add a host, install Latch there and run `latch serve`, or start hosting on this Mac.")
                    }
                }
            } else if let error = app.accountError {
                Section("Your hosts") {
                    Text("Couldn't list your hosts.")
                    Text(error).foregroundStyle(.secondary)
                }
            } else {
                Section {
                    ProgressView("Checking your hosts…").frame(maxWidth: .infinity)
                }
            }
        }
        .formStyle(.grouped)
        .navigationTitle(app.host?.name ?? "Home")
        .navigationSubtitle(app.host?.subtitle(local: app.status) ?? "")
        .task(id: app.host?.id) {
            guard !app.still else { return }
            await app.refreshSummary()
        }
    }
}

/// Logged in with nothing served here: say so, and offer to start.
private struct ViewerSection: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Section {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "laptopcomputer")
                    .font(.title2)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text("This Mac (\(app.status?.peer?.name ?? "")) isn't hosting.").font(.headline)
                    Text("You're viewing your hosts. Your agents' work runs on them, not here.")
                        .foregroundStyle(.secondary)
                    if let error = app.startHostError {
                        Text(error).foregroundStyle(.red)
                    }
                }
                Spacer(minLength: 12)
                Button(app.startingHost ? "Starting…" : "Start hosting on this Mac…") { app.confirmingStartHosting = true }
                    .disabled(app.startingHost)
            }
        }
    }
}

/// What on this Mac's own host needs the person, from `status --local`.
private struct NeedsSection: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Section("Needs you") {
            let needs = needs
            if needs.isEmpty {
                Text("Nothing needs you right now.").foregroundStyle(.secondary)
            }
            ForEach(needs, id: \.self) { need in
                switch need {
                case let .unchecked(error): Need(text: "Couldn't check this Mac.", detail: error)
                case let .blocked(message): Need(text: "Latch can't run on this Mac right now.", detail: message)
                case let .stopped(lastError): StartNeed(lastError: lastError)
                case let .update(build):
                    Need(symbol: "arrow.down.circle", text: "A new version of Latch is available\(build.map { " (build \($0))" } ?? "").",
                         detail: "Run `latch update` to install it.")
                }
            }
        }
    }

    private enum Needed: Hashable {
        case unchecked(String), blocked(String), stopped(lastError: String?), update(build: String?)
    }

    /// What the person should act on, as latch reports it.
    private var needs: [Needed] {
        var needs: [Needed] = []
        if let error = app.statusError { needs.append(.unchecked(error)) }
        guard let status = app.status else { return needs }
        if let blocked = status.serve.blocked {
            needs.append(.blocked(blocked.message))
        } else if status.serve.state == "stopped" || status.serve.state == "error" {
            needs.append(.stopped(lastError: status.lastError?.message))
        }
        if let update = status.update, update.available { needs.append(.update(build: update.build)) }
        return needs
    }
}

/// A host's rules, vault and connectors at a glance, each opening its
/// screen; this Mac's also offers setting up again.
private struct OnHostSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        let summary = app.summaries[host.name]
        Section("On \(host.name)") {
            SummaryRow(title: "Rules", value: summary?.rules?.map(Self.rules), open: "Open") { app.section = .rules }
            SummaryRow(title: "Vault", value: summary?.vault?.map(Self.vault), open: "Open") { app.section = .vault }
            SummaryRow(title: "Connectors", value: summary?.plugins?.map(Self.plugins), open: "Host Settings") {
                app.section = .settings
            }
            if host.isThisMac {
                Button("Set up again…") { app.setUpAgain() }
            }
        }
    }

    private static func rules(_ rules: RulesShow) -> String {
        let every = rules.everyAgent == nil ? "None for every agent" : "Set for every agent"
        return switch rules.agents?.count ?? 0 {
        case 0: every
        case 1: "\(every) · 1 agent with its own rules"
        case let n: "\(every) · \(n) agents with their own rules"
        }
    }

    private static func vault(_ vault: VaultList) -> String {
        switch vault.items.count {
        case 0: "No items"
        case 1: "1 item"
        case let n: "\(n) items"
        }
    }

    /// "Google: on · Messages, Contacts: off".
    private static func plugins(_ list: PluginsList) -> String {
        let name = { (plugin: PluginsList.Plugin) in plugin.name.prefix(1).uppercased() + plugin.name.dropFirst() }
        let on = list.plugins.filter { $0.state == "enabled" }.map(name)
        let off = list.plugins.filter { $0.state != "enabled" }.map(name)
        return [on.isEmpty ? nil : on.joined(separator: ", ") + ": on", off.isEmpty ? nil : off.joined(separator: ", ") + ": off"]
            .compactMap { $0 }.joined(separator: " · ")
    }
}

/// One line of a host's summary: what latch said, or why it couldn't.
private struct SummaryRow: View {
    var title: String
    /// nil until the host has answered.
    var value: Result<String, LatchError>?
    var open: String
    var action: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            LabeledContent {
                Text(shown).multilineTextAlignment(.trailing)
            } label: {
                Text(title)
                if case let .failure(error) = value {
                    Text(error.message)
                }
            }
            Button(open, action: action).fixedSize()
        }
    }

    private var shown: String {
        switch value {
        case nil: "Checking…"
        case let .success(text): text
        case .failure: "Couldn't check"
        }
    }
}

private struct Need: View {
    var symbol = "exclamationmark.triangle"
    var text: String
    var detail: String

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(text)
                Text(LocalizedStringKey(detail)).foregroundStyle(.secondary)
            }
        } icon: {
            Image(systemName: symbol).foregroundStyle(.orange)
        }
    }
}

/// This Mac's host is stopped: say what that means, and start it.
private struct StartNeed: View {
    var lastError: String?
    @Environment(AppModel.self) private var app

    var body: some View {
        HStack(alignment: .top) {
            Need(symbol: "pause.circle", text: "Latch isn't running on this Mac, so your agents can't reach it.",
                 detail: lastError.map { "It stopped: \($0)" } ?? "Start it to let them in again.")
            Spacer(minLength: 12)
            Button(app.startingHost ? "Starting…" : "Start Latch") { Task { await app.startHosting() } }
                .disabled(app.startingHost)
        }
        if let error = app.startHostError {
            Text(error).foregroundStyle(.red)
        }
    }
}

/// Another host Plow counts offline: nothing on it can be read.
private struct OfflineSection: View {
    var host: HostEntry

    var body: some View {
        Section {
            ContentUnavailableView {
                Label("\(host.name) is offline", systemImage: "wifi.slash")
            } description: {
                Text("\(lastSeen)Start `latch serve` on \(host.name) to reach it again.")
            }
        }
    }

    private var lastSeen: String {
        guard let seen = host.lastSeen else { return "" }
        let time = seen.formatted(date: .omitted, time: .shortened)
        if Calendar.current.isDateInToday(seen) { return "Plow last saw it today at \(time). " }
        return "Plow last saw it \(seen.formatted(.dateTime.month(.wide).day())) at \(time). "
    }
}

import SwiftUI

/// The main window: a sidebar with the account tier (Agents) above the host
/// tier (the host picker and that host's screens), and the selected screen.
struct MainView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 320)
        } detail: {
            switch app.section {
            case .home: HomeView()
            case .activity: ActivityView()
            case .agents: AgentsView()
            case .rules: HostRulesView()
            default: NotYetView(item: app.section)
            }
        }
        .frame(minWidth: 680, minHeight: 440)
        .confirmationDialog("Start hosting on this Mac?", isPresented: $app.confirmingStartHosting) {
            Button("Start Hosting") { Task { await app.startHosting() } }
        } message: {
            Text("Latch runs in the background, and your agents can reach this Mac, judged by your rules.")
        }
        .task {
            guard !app.still else { return }
            await app.refreshStatus()
            app.leaveMainIfLoggedOut()
            await app.refreshAccount()
            await app.followStatus()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            guard !app.still else { return }
            Task {
                await app.refreshAccount()
                if app.section == .home { await app.refreshSummary() }
            }
        }
    }
}

private struct Sidebar: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        List(selection: selection) {
            Section("Your account") {
                ForEach(SidebarItem.account) { item in
                    Label(item.title, systemImage: item.symbol).tag(item)
                }
            }
            Section("Host") {
                HostPicker()
                ForEach(SidebarItem.host) { item in
                    Label(item.title, systemImage: item.symbol).tag(item)
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let me = app.status?.peer {
                Text(app.viewerOnly ? "Logged in as \(me.name) · viewing only" : "Logged in as \(me.name)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
            }
        }
    }

    private var selection: Binding<SidebarItem?> {
        Binding { app.section } set: { if let item = $0 { app.section = item } }
    }
}

/// Which host the host tier shows: this Mac and the account's other hosts.
/// Picking one changes the host tier only; the sidebar item stays.
struct HostPicker: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Menu {
            if app.viewerOnly, let me = app.status?.peer {
                Section {
                    Button {} label: {
                        Text("This Mac (\(me.name))")
                        Text("Not hosting · you're viewing your hosts")
                    }
                    .disabled(true)
                    Button("Start hosting on this Mac…") { app.confirmingStartHosting = true }
                }
            }
            Picker("Host", selection: chosen) {
                ForEach(app.hosts) { host in
                    Text(host.name).tag(host.name)
                        .badge(Text(host.subtitle(local: app.status)))
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
            if let notice = app.plowNotice {
                Divider()
                Text(notice.title)
            }
            Divider()
            Text("To add a host, install Latch there and run latch serve.")
        } label: {
            HStack(spacing: 8) {
                StateDot(state: app.host.map { HostState($0, local: app.status) })
                VStack(alignment: .leading, spacing: 1) {
                    Text(app.host?.name ?? "No host").font(.headline)
                    Text(app.host?.subtitle(local: app.status) ?? (app.viewerOnly ? "This Mac isn't hosting" : "Checking…"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .lineLimit(1)
            }
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.visible)
        .accessibilityLabel("Host: \(app.host.map { "\($0.name), \($0.subtitle(local: app.status))" } ?? "none")")
        .padding(.vertical, 2)
    }

    private var chosen: Binding<String> {
        Binding { app.host?.name ?? "" } set: { app.chosenHost = $0 }
    }
}

/// A host's state as a coloured dot; the words beside it say the same.
struct StateDot: View {
    var state: HostState?

    var body: some View {
        Circle()
            .fill(state?.isUp == true ? Color.green : state?.isProblem == true ? Color.red : Color.secondary)
            .frame(width: 8, height: 8)
            .accessibilityHidden(true)
    }
}

/// A screen still to come: its place in the sidebar, honestly empty.
private struct NotYetView: View {
    var item: SidebarItem
    @Environment(AppModel.self) private var app

    var body: some View {
        ContentUnavailableView(item.title, systemImage: item.symbol, description: Text("Not available yet."))
            .navigationTitle(item.title)
            .navigationSubtitle(SidebarItem.host.contains(item) ? app.host?.name ?? "" : "")
    }
}

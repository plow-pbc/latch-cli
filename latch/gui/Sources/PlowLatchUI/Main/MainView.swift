import SwiftUI

/// The main window: a sidebar with the account tier (Agents, Connections)
/// above the host tier (the host picker and that host's screens), and the
/// selected screen.
struct MainView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        let route = app.route
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 320)
        } detail: {
            switch app.place {
            case .account(.agents): AgentsView()
            case .account(.connections): ConnectionsView()
            case .host(.home): HomeView()
            case .host(.activity): ActivityView()
            case .host(.rules): HostRulesView()
            case .host(.vault): VaultView()
            case .host(.settings): HostSettingsView()
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
            await app.followStatus()
        }
        // Plow's list of hosts, which can be slow, arrives when it does;
        // this Mac's status doesn't wait for it.
        .task {
            guard !app.still else { return }
            await app.refreshAccount()
        }
        // Each screen's reads, decided by the model, for the route that
        // keyed them: a change of what they depend on cancels the last one's.
        .task(id: app.entry) {
            guard !app.still else { return }
            await app.enter(route)
        }
        // Home and Activity share the selected host's feed: going between
        // them keeps it running.
        .following(app.activityFeed, still: app.still)
        .onDisappear { app.endFeeds() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            guard !app.still else { return }
            Task { await app.activated() }
        }
    }
}

private struct Sidebar: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        List(selection: selection) {
            Section("Your account") {
                ForEach(AccountPage.allCases) { page in
                    Label(page.title, systemImage: page.symbol).tag(Place.account(page))
                }
            }
            Section("Host") {
                HostPicker()
                ForEach(HostScreen.allCases) { screen in
                    Label(screen.title, systemImage: screen.symbol).tag(Place.host(screen))
                }
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let name = app.status?.loggedInAs {
                Text(app.viewerOnly ? "Logged in as \(name) · viewing only" : "Logged in as \(name)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 10)
            }
        }
    }

    private var selection: Binding<Place?> {
        Binding { app.place } set: { if let place = $0 { app.place = place } }
    }
}

/// Which host the host tier shows: this Mac and the account's other hosts.
/// Picking one changes the host tier only; the sidebar item stays.
struct HostPicker: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Menu {
            if app.viewerOnly, let name = app.status?.loggedInAs {
                Section {
                    Button {} label: {
                        Text("This Mac (\(name))")
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
            Text("To add a host, install and start Latch on another computer.")
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

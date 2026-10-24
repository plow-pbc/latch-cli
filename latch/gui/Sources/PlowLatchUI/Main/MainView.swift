import SwiftUI

/// The main window: the sidebar's account pages and hosts; the selected
/// page, or the selected host's workspace, whose five screens the toolbar
/// switches between; and, for Activity and Vault, one inspector for the
/// whole window.
struct MainView: View {
    @Environment(AppModel.self) private var app
    /// The place and host the window was on, kept with its scene state.
    @SceneStorage("place") private var savedPlace = Place.host(.home).rawValue
    @SceneStorage("host") private var savedHost = ""

    var body: some View {
        @Bindable var app = app
        let route = app.route
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 210, ideal: 230, max: 320)
        } detail: {
            Detail(route: route)
        }
        // At the window's own level, full height, at its native width.
        .inspector(isPresented: inspecting) {
            Inspector(route: route)
        }
        // Shown or hidden by a change of screen: at once, not slid.
        .transaction(value: app.place) { $0.disablesAnimations = true }
        // As narrow as the sidebar, Activity's table columns and the
        // inspector side by side allow.
        .frame(minWidth: 1040, minHeight: 440)
        .confirmationDialog("Start hosting on this Mac?", isPresented: $app.confirmingStartHosting) {
            Button("Start Hosting") { Task { await app.startHosting() } }
        } message: {
            Text("Latch runs in the background, and your agents can reach this Mac, judged by your rules.")
        }
        .onAppear {
            guard !app.still else { return }
            app.restore(place: savedPlace, host: savedHost.isEmpty ? nil : savedHost)
        }
        .onChange(of: app.place) { savedPlace = app.place.rawValue }
        .onChange(of: app.chosenHost) { savedHost = app.chosenHost ?? "" }
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

    /// The inspector shows on Activity and Vault, as each was left.
    private var inspecting: Binding<Bool> {
        Binding {
            guard case let .host(screen) = app.place, HostScreen.inspected.contains(screen) else { return false }
            return app.inspecting.contains(screen)
        } set: { shown in
            guard case let .host(screen) = app.place, HostScreen.inspected.contains(screen) else { return }
            if shown { app.inspecting.insert(screen) } else { app.inspecting.remove(screen) }
        }
    }
}

/// The selected page or host screen, titled for the host it's on, with the
/// workspace's screens in the toolbar.
private struct Detail: View {
    var route: Route
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
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
        .navigationTitle(title)
        .navigationSubtitle(subtitle)
        .toolbar {
            if case .host = route {
                ToolbarItem(placement: .principal) {
                    Picker("Screen", selection: screen) {
                        ForEach(HostScreen.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    .help("Switch between this host's screens")
                }
            }
            if case let .host(screen, _) = route, HostScreen.inspected.contains(screen) {
                ToolbarItem(placement: .primaryAction) {
                    Button("Inspector", systemImage: "sidebar.trailing") {
                        if app.inspecting.contains(screen) { app.inspecting.remove(screen) } else { app.inspecting.insert(screen) }
                    }
                    .help(app.inspecting.contains(screen) ? "Hide the inspector" : "Show the inspector")
                }
            }
        }
    }

    private var title: String {
        switch route {
        case let .account(page): page.title
        case let .host(_, host): host?.name ?? "No host"
        }
    }

    /// A host's state, or, on an account page, that no host owns it.
    private var subtitle: String {
        switch route {
        case .account: "Your account"
        case let .host(_, host): host?.subtitle(local: app.status) ?? ""
        }
    }

    private var screen: Binding<HostScreen> {
        Binding { app.screen } set: { app.place = .host($0) }
    }
}

/// The window's inspector: on Activity, the selected request; on Vault, the
/// selected item; each only while its host shows and the screen lists it.
struct Inspector: View {
    var route: Route
    @Environment(AppModel.self) private var app

    var body: some View {
        switch route {
        case let .host(.activity, host?):
            if let record = app.inspectedRecord(on: host) {
                RecordInspector(record: record, caller: record.callerLabel(hostKey: app.key(of: host), hostName: host.name,
                                                                          isThisMac: host.isThisMac), hostName: host.name)
            } else {
                ContentUnavailableView("No Selection", systemImage: "doc.text.magnifyingglass",
                                       description: Text("Select a request to see who asked, why it was allowed or refused, and what ran."))
            }
        case let .host(.vault, host?):
            if let item = app.inspectedVaultItem(on: host) {
                VaultItemInspector(item: item, show: app.vaultShow(of: item, on: host), hostName: host.name,
                                   edit: { Task { await app.beginVaultEdit(item, on: host) } },
                                   remove: { app.askToRemove(item, on: host) })
                    .task(id: "\(host.name)/\(item.item)@\(item.revision)") {
                        guard !app.still else { return }
                        await app.showVaultItem(item.item, on: host)
                    }
            } else {
                ContentUnavailableView("No Selection", systemImage: "lock.rectangle",
                                       description: Text("Select an item to see what it holds. Secret values are never shown."))
            }
        default:
            ContentUnavailableView("No Selection", systemImage: "sidebar.trailing")
        }
    }
}

/// The account's pages, then each of its hosts; picking a host keeps the
/// screen the workspace was on.
struct Sidebar: View {
    @Environment(AppModel.self) private var app

    enum Row: Hashable {
        case account(AccountPage)
        case host(String)
    }

    var body: some View {
        List(selection: selection) {
            Section("Your account") {
                ForEach(AccountPage.allCases) { page in
                    Label(page.title, systemImage: page.symbol).tag(Row.account(page))
                }
            }
            Section("Hosts") {
                ForEach(app.hosts) { host in
                    HostRow(host: host).tag(Row.host(host.name))
                }
                if app.hosts.isEmpty {
                    Text(app.viewerOnly ? "No hosts yet" : app.plowNotice == nil ? "Checking…" : "Not listed")
                        .foregroundStyle(.secondary)
                }
                if let notice = app.plowNotice {
                    Label(notice.title, systemImage: "icloud.slash")
                        .foregroundStyle(.secondary)
                        .help(notice.detail)
                }
                if app.viewerOnly {
                    Button("Start Hosting on This Mac…") { app.confirmingStartHosting = true }
                        .help("This Mac isn't hosting; you're viewing your hosts")
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

    private var selection: Binding<Row?> {
        Binding {
            switch app.route {
            case let .account(page): .account(page)
            case let .host(_, host): host.map { .host($0.name) }
            }
        } set: { row in
            switch row {
            case let .account(page)?: app.place = .account(page)
            case let .host(name)?: app.showHost(name)
            case nil: break
            }
        }
    }
}

/// A host in the sidebar: its name, and its state in words.
private struct HostRow: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: 1) {
                Text(host.name).lineLimit(1).truncationMode(.middle)
                Text(host.subtitle(local: app.status)).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        } icon: {
            Image(systemName: host.isThisMac ? "laptopcomputer" : "server.rack")
        }
        .help("\(host.name): \(host.subtitle(local: app.status))")
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

import SwiftUI

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
                    Button("Start Hosting on This Mac…") { app.ask(.startHosting) }
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

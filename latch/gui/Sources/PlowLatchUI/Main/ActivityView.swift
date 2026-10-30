import SwiftUI

/// Activity's outcome filter.
enum ActivityFilter: String, CaseIterable, Identifiable {
    case all = "All", allowed = "Allowed", refused = "Refused"
    var id: Self { self }
}

/// What Activity's heading, states and footer show of the feed, set by the
/// table's controller when it reloads (once a batch at most), not read from
/// the feed by each: reading it, they redrew for every record.
@Observable final class ActivityShown {
    /// The host showing, if its activity can be shown.
    var host: HostEntry?
    /// Rows matching the filters, and records loaded.
    var rows = 0
    var records = 0
    var caughtUp = false
    var error: LatchError?
    /// The oldest record loaded, while older ones are on the host.
    var since: Date?
    /// The agent asked for, by the name on its records.
    var agentName: String?

    func set<T: Equatable>(_ path: ReferenceWritableKeyPath<ActivityShown, T>, _ value: T) {
        if self[keyPath: path] != value { self[keyPath: path] = value }
    }
}

/// Activity's heading, then its scope bar: the outcome, the agent asked for
/// (a click removes it) and Show Connections and Reads. Too narrow for one
/// line, they stack, and the outcome's segments narrow to fit.
struct ActivityHeading: View {
    var shown: ActivityShown
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        let outcome = Picker("Show", selection: $app.activityFilter) {
            ForEach(ActivityFilter.allCases) { Text($0.rawValue).tag($0) }
        }
        .pickerStyle(.segmented)
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Activity").font(.title2.bold())
                Spacer()
                Toggle("Show Connections and Reads", systemImage: "point.3.connected.trianglepath.dotted", isOn: $app.showConnections)
                    .toggleStyle(.button)
                    .labelStyle(.iconOnly)
                    .help("Also show when callers connected, and your own reads (rules, vault, status, activity)")
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) {
                    outcome.fixedSize()
                    agentFilter
                }
                VStack(alignment: .leading, spacing: 8) {
                    outcome
                    agentFilter
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    @ViewBuilder private var agentFilter: some View {
        if let host = shown.host, let uid = app.activityAgent(on: host) {
            let name = app.agents?.first(where: { $0.uid == uid })?.name ?? shown.agentName ?? "Unnamed agent"
            Button {
                app.activityAgent = nil
            } label: {
                Label("Agent: \(name)", systemImage: "xmark.circle.fill")
            }
            .buttonStyle(.bordered)
            .help("Show every agent's requests")
            .accessibilityLabel("Remove filter: agent \(name)")
        }
    }
}

/// Where Activity has no rows to list, why: no host, offline, couldn't
/// read, reading, nothing yet, nothing matching.
struct ActivityStates: View {
    var shown: ActivityShown
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
            if let host = app.host {
                if !host.isThisMac, host.online == false {
                    ContentUnavailableView {
                        Label("\(host.name) is offline", systemImage: "wifi.slash")
                    } description: {
                        Text("Its activity is kept on \(host.name). Start Latch there to see it.")
                    }
                } else if shown.records == 0, let error = shown.error {
                    Unavailable(error: error, host: host)
                } else if shown.records == 0, !shown.caughtUp {
                    ProgressView("Reading \(host.isThisMac ? "this Mac's" : "\(host.name)'s") activity…")
                } else if shown.records == 0 {
                    ContentUnavailableView("No activity yet", systemImage: "tray",
                                           description: Text("Every request to \(host.name), allowed or refused, shows up here."))
                } else {
                    ContentUnavailableView("No matching requests in the loaded history", systemImage: "line.3.horizontal.decrease.circle",
                                           description: Text("Try another filter."))
                }
            } else {
                ContentUnavailableView("No host", systemImage: "waveform.path.ecg",
                                       description: Text("Activity is kept on each host. Pick one in the sidebar."))
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.background)
    }
}

/// From when the records loaded go back, while there are older ones (there
/// is no loading more), and how many of those loaded match.
struct ActivityFooter: View {
    var shown: ActivityShown

    var body: some View {
        if let host = shown.host {
            HStack {
                if let since = shown.since {
                    Text("Showing since \(since.formatted(date: .abbreviated, time: .shortened))").foregroundStyle(.secondary)
                }
                Spacer()
                if let error = shown.error {
                    Label(ErrorCopy.short(error, host: host.spoken), systemImage: "exclamationmark.triangle").foregroundStyle(.secondary).lineLimit(1)
                } else {
                    let rows = shown.rows, records = shown.records
                    Text("\(rows) \(rows == 1 ? "match" : "matches") · \(records) \(records == 1 ? "record" : "records") loaded\(shown.caughtUp ? "" : " · reading…")")
                        .foregroundStyle(.secondary)
                }
            }
            .font(.callout)
            .controlSize(.small)
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(.bar)
        }
    }
}

/// Why a host's activity can't be shown, in the app's words.
private struct Unavailable: View {
    var error: LatchError
    var host: HostEntry

    var body: some View {
        switch error.code {
        case "no_host_here":
            ContentUnavailableView("No activity on this Mac", systemImage: "tray",
                                   description: Text("This Mac has never hosted, so it keeps no activity. Pick one of your hosts to see its own."))
        case "host_offline", "host_stopped", "host_unreachable":
            ContentUnavailableView {
                Label("Can't reach \(host.name)", systemImage: "wifi.slash")
            } description: {
                ErrorText(error: error, said: [ErrorCopy.fact(error, host: host.spoken).flatMap { error.code == "host_unreachable" ? nil : $0 },
                                               "Latch tries again on its own while this screen is open."]
                    .compactMap { $0 }.joined(separator: "\n"))
            }
        default:
            ContentUnavailableView {
                Label("Couldn't read the activity", systemImage: "exclamationmark.triangle")
            } description: {
                ErrorText(error: error, host: host.spoken, titled: true)
            }
        }
    }
}

/// A record's status as a coloured dot and words; the words carry it all.
struct StatusLabel: View {
    var status: AuditRecord.Status

    var body: some View {
        Label {
            Text(status.title)
        } icon: {
            Circle().fill(color).frame(width: 7, height: 7)
        }
        .labelStyle(DotLabelStyle())
    }

    private var color: Color {
        switch status {
        case .connected: .secondary
        case .allowed: .green
        case .gatekeeper(saidNo: false): .orange
        case .gatekeeper(saidNo: true), .refused: .red
        case .stopped: .yellow
        }
    }
}

private struct DotLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.accessibilityHidden(true)
            configuration.title
        }
    }
}

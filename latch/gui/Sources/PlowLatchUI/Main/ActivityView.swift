import SwiftUI

/// The selected host's audit, newest first, live while on screen; a record
/// opens in the inspector. Under its heading, a scope bar narrows the
/// records loaded so far: by outcome and, from an agent's link, to that
/// agent.
struct ActivityView: View {
    @Environment(AppModel.self) private var app
    @State private var inspecting = true

    enum Filter: String, CaseIterable, Identifiable {
        case all = "All", allowed = "Allowed", refused = "Refused"
        var id: Self { self }
    }

    var body: some View {
        @Bindable var app = app
        Group {
            if let host = app.host {
                if !host.isThisMac, host.online == false {
                    ContentUnavailableView {
                        Label("\(host.name) is offline", systemImage: "wifi.slash")
                    } description: {
                        Text("Its activity is kept on \(host.name). Start Latch there to see it.")
                    }
                } else {
                    Records(feed: app.feed(for: host), host: host, inspecting: $inspecting)
                        .id(host.id)
                }
            } else {
                ContentUnavailableView("No host", systemImage: "waveform.path.ecg",
                                       description: Text("Activity is kept on each host. Pick one in the sidebar."))
            }
        }
        .navigationTitle("Activity")
        .navigationSubtitle(app.host?.name ?? "")
        .searchable(text: $app.activitySearch, placement: .toolbar, prompt: "Search requests")
        .toolbar {
            ToolbarItem {
                Toggle("Show Connections and Reads", systemImage: "point.3.connected.trianglepath.dotted", isOn: $app.showConnections)
                    .help("Also show when callers connected, and your own reads (rules, vault, status, activity)")
            }
            ToolbarItem {
                Button("Inspector", systemImage: "sidebar.trailing") { inspecting.toggle() }
                    .help(inspecting ? "Hide the inspector" : "Show the inspector")
            }
        }
    }
}

/// One host's records under its heading and scope bar, and its states
/// while there are none to show.
private struct Records: View {
    var feed: ActivityFeed
    var host: HostEntry
    @Binding var inspecting: Bool
    @Environment(AppModel.self) private var app

    var body: some View {
        let rows = app.activityRows(feed.records, on: host)
        VStack(alignment: .leading, spacing: 0) {
            heading
            Divider()
            Group {
                if feed.records.isEmpty, let error = feed.error {
                    Unavailable(error: error, host: host)
                } else if feed.records.isEmpty, !feed.caughtUp {
                    ProgressView("Reading \(host.isThisMac ? "this Mac's" : "\(host.name)'s") activity…")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if feed.records.isEmpty {
                    ContentUnavailableView("No activity yet", systemImage: "tray",
                                           description: Text("Every request to \(host.name), allowed or refused, shows up here."))
                } else if rows.isEmpty {
                    ContentUnavailableView("No matching requests in the loaded history", systemImage: "line.3.horizontal.decrease.circle",
                                           description: Text(feed.hasOlder ? "Try another filter, or load earlier requests." : "Try another filter."))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .safeAreaInset(edge: .bottom, spacing: 0) { footer(rows) }
                } else {
                    table(rows)
                        .safeAreaInset(edge: .bottom, spacing: 0) { footer(rows) }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// The heading, then the scope bar: the outcome, and the agent asked
    /// for, which a click removes. Too narrow for one line, they stack.
    private var heading: some View {
        @Bindable var app = app
        let outcome = Picker("Show", selection: $app.activityFilter) {
            ForEach(ActivityView.Filter.allCases) { Text($0.rawValue).tag($0) }
        }
        .pickerStyle(.segmented)
        .fixedSize()
        return VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Activity").font(.title2.bold())
                Text(host.subtitle(local: app.status)).font(.callout).foregroundStyle(.secondary)
            }
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 12) {
                    outcome
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
        if let uid = app.activityAgent(on: host) {
            let name = app.agents?.first(where: { $0.uid == uid })?.name
                ?? feed.records.last(where: { $0.agent == uid && $0.callerName != nil })?.callerName ?? "Unnamed agent"
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

    private func table(_ rows: [AuditRecord]) -> some View {
        @Bindable var app = app
        return Table(rows, selection: $app.selectedRecord) {
            TableColumn("Time") { record in
                Text(record.time, format: .dateTime.hour().minute().second()).foregroundStyle(.secondary)
            }
            .width(min: 95, ideal: 100, max: 130)
            TableColumn("Status") { record in StatusLabel(status: record.status).help(record.status.title) }
                .width(min: 140, ideal: 210, max: 280)
            TableColumn("Request") { record in
                Text(record.command ?? "—").font(.body.monospaced()).lineLimit(1).truncationMode(.middle)
                    .help(record.command ?? "A connection")
            }
            .width(min: 160, ideal: 200)
            TableColumn("Caller") { record in
                let caller = callerLabel(record)
                Text("\(caller.who) \(Text("· \(caller.kind)").foregroundStyle(.secondary))")
            }
            .width(min: 110, ideal: 140, max: 240)
        }
        .inspector(isPresented: $inspecting) {
            Group {
                if let record = app.selectedRecord(in: rows) {
                    RecordInspector(record: record, caller: callerLabel(record), hostName: host.name)
                } else {
                    ContentUnavailableView("No Selection", systemImage: "doc.text.magnifyingglass",
                                           description: Text("Select a request to see who asked, why it was allowed or refused, and what ran."))
                }
            }
            .inspectorColumnWidth(min: 260, ideal: 320, max: 460)
        }
    }

    /// Load Earlier, and how many of the records loaded match: what's
    /// loaded is not all there is while earlier records exist.
    private func footer(_ rows: [AuditRecord]) -> some View {
        HStack {
            if feed.hasOlder {
                Button(feed.loadingOlder ? "Loading…" : "Load Earlier") { Task { await feed.loadOlder() } }
                    .disabled(feed.loadingOlder)
            }
            Spacer()
            if let error = feed.error {
                Label(ErrorCopy.short(error, host: host.spoken), systemImage: "exclamationmark.triangle").foregroundStyle(.secondary).lineLimit(1)
            } else {
                Text("\(rows.count) \(rows.count == 1 ? "match" : "matches") · \(feed.records.count) \(feed.records.count == 1 ? "record" : "records") loaded\(feed.caughtUp ? "" : " · reading…")")
                    .foregroundStyle(.secondary)
            }
        }
        .font(.callout)
        .controlSize(.small)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
    }

    private func callerLabel(_ record: AuditRecord) -> (who: String, kind: String) {
        record.callerLabel(hostKey: app.key(of: host), hostName: host.name, isThisMac: host.isThisMac)
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
        case .notCovered: .orange
        case .refused: .red
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

extension View {
    /// Runs `feed`'s stream while this view is on screen, and for its grace
    /// after.
    func following(_ feed: ActivityFeed?, still: Bool) -> some View {
        task(id: feed.map(ObjectIdentifier.init)) {
            guard let feed, !still else { return }
            feed.start()
            // Until this view goes away: a stream nothing is sent on ends
            // only when the task is cancelled.
            let (untilGone, open) = AsyncStream<Never>.makeStream()
            for await _ in untilGone {}
            withExtendedLifetime(open) {}
            feed.stop()
        }
    }
}

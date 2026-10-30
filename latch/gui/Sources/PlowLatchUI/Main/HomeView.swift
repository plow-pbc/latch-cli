import SwiftUI

/// Home for the selected host: what needs the person (this Mac's own host
/// only), today's activity and the latest requests, live while on screen,
/// and the host's rules, vault and connectors.
struct HomeView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            if let notice = app.plowNotice {
                Section {
                    Label {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(notice.title)
                            if let error = notice.error { ErrorText(error: error, titled: true).foregroundStyle(.secondary) }
                            Text(notice.detail).foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "icloud.slash").foregroundStyle(.orange)
                    }
                }
            }
            if app.viewerOnly {
                ViewerSection()
            }
            if let host = app.host {
                if host.isThisMac {
                    NeedsSection()
                    ActivitySections(feed: app.feed(for: host), host: host)
                    OnHostSection(host: host)
                } else if host.online == false {
                    OfflineSection(host: host)
                } else {
                    ActivitySections(feed: app.feed(for: host), host: host)
                    OnHostSection(host: host)
                }
            } else if app.viewerOnly {
                Section {
                    ContentUnavailableView {
                        Label("No hosts yet", systemImage: "server.rack")
                    } description: {
                        Text("To add a host, install and start Latch on another computer, or start hosting on this Mac.")
                    }
                }
            } else if app.plowNotice == nil {
                Section {
                    ProgressView("Checking your hosts…").frame(maxWidth: .infinity)
                }
            }
        }
        .formStyle(.grouped)
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
                    Text("This Mac (\(app.status?.thisMachine.name ?? "")) isn't hosting.").font(.headline)
                    Text("You're viewing your hosts. Your agents' work runs on them, not here.")
                        .foregroundStyle(.secondary)
                    if let error = app.startHostError {
                        ErrorText(error: error).foregroundStyle(.red)
                    }
                }
                Spacer(minLength: 12)
                Button(app.startingHost ? "Starting…" : "Start hosting on this Mac…") { app.ask(.startHosting) }
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
                case let .unchecked(error): Need(text: "Couldn't check this Mac.", error: error)
                case let .blocked(problem): Need(text: "Latch can't run on this Mac right now.", error: problem)
                case let .stopped(lastError): StartNeed(lastError: lastError)
                case let .update(build):
                    HStack(alignment: .top) {
                        Need(symbol: "arrow.down.circle", text: build.map { "Build \($0) available" } ?? "Update available",
                             detail: "Latch will restart.")
                        Spacer(minLength: 12)
                        InstallUpdateButton()
                    }
                    UpdateProgress()
                }
            }
        }
    }

    private enum Needed: Hashable {
        case unchecked(LatchError), blocked(LatchError), stopped(lastError: LatchError?), update(build: Int?)
    }

    /// What the person should act on, as latch reports it.
    private var needs: [Needed] {
        var needs: [Needed] = []
        if let error = app.statusError { needs.append(.unchecked(error)) }
        guard let status = app.status else { return needs }
        if let blocked = status.thisMachine.hosting.blocked {
            needs.append(.blocked(.failed(blocked)))
        } else if status.thisMachine.hosting.state == "stopped" || status.thisMachine.hosting.state == "error" {
            needs.append(.stopped(lastError: status.lastError.map(LatchError.failed)))
        }
        if app.updateAvailable, !app.updateInstalled { needs.append(.update(build: app.availableBuild)) }
        return needs
    }
}

/// Today's requests on a host and the latest five, from its activity
/// stream; a row opens it in Activity.
private struct ActivitySections: View {
    var feed: ActivityFeed
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        let calls = HomeActivity(feed.records, today: Calendar.current.dateInterval(of: .day, for: .now)!)
        if calls.recent.isEmpty, let error = feed.error {
            Section("Activity") {
                if error.code == "no_host_here" {
                    Text("This Mac keeps no activity yet.")
                } else {
                    Text("Couldn't read the activity.")
                    ErrorText(error: error, host: host.spoken, titled: true).foregroundStyle(.secondary)
                }
            }
        } else if calls.recent.isEmpty, !feed.caughtUp {
            Section("Today") {
                ProgressView().frame(maxWidth: .infinity)
            }
        } else {
            // Only the newest records are held: if the oldest held is from
            // today, there were more today than these.
            let more = feed.hasOlder && feed.records.first.map { Calendar.current.isDateInToday($0.time) } == true ? "+" : ""
            Section("Today") {
                GoRow(hint: "Opens Activity") { app.openActivity(host) } label: {
                    LabeledContent("Requests", value: "\(calls.today)\(more)")
                }
                GoRow(hint: "Opens Activity, showing what was refused") { app.openActivity(host, showing: .refused) } label: {
                    LabeledContent("Refused") {
                        Text("\(calls.refusedToday)\(more)")
                            .foregroundStyle(calls.refusedToday > 0 ? .red : .secondary)
                    }
                }
                LabeledContent("Agents that called", value: calls.agentsToday.isEmpty ? "None" : calls.agentsToday.joined(separator: ", "))
            }
            Section("Recent activity") {
                if calls.recent.isEmpty {
                    Text(feed.loadingOlder ? "Looking further back…" : "No requests yet.").foregroundStyle(.secondary)
                }
                ForEach(calls.recent) { record in
                    GoRow(hint: "Opens it in Activity") { app.openRecord(record, on: host) } label: {
                        RecentRow(record: record, caller: record.callerLabel(hostKey: app.key(of: host), hostName: host.name,
                                                                            isThisMac: host.isThisMac))
                    }
                }
                // Home reaches back only a few pages on its own; the rest is
                // a click away.
                if calls.recent.count < 5, feed.hasOlder, !feed.loadingOlder {
                    Button("Load Earlier") { Task { await feed.loadOlder() } }
                }
            }
            // The newest page may be all connections and reads: reach back
            // for five requests, or to the start.
            .task(id: feed.caughtUp) {
                guard feed.caughtUp, !app.still else { return }
                await feed.loadOlder(until: 5, pages: ActivityFeed.homePages)
            }
        }
    }
}

/// What Home shows of a host's held records: what callers asked of it
/// (not connections, nor the owner's own reads, Home's own summary being
/// one) today, and the newest five. Read from the newest back, stopping at
/// the first record before today once five are found: today's records and
/// no more, not every held record, and no calendar asked of each one.
/// Records are in the order latch wrote them, which is their time order
/// to within the moment two requests finish together.
nonisolated struct HomeActivity {
    private(set) var today = 0
    private(set) var refusedToday = 0
    /// The agents that called today, by name, sorted.
    private(set) var agentsToday: [String] = []
    /// The newest five calls, newest first.
    private(set) var recent: [AuditRecord] = []

    /// `today`: the day the counts are for.
    init(_ records: [AuditRecord], today day: DateInterval) {
        var agents = Set<String>()
        for record in records.reversed() {
            let earlier = record.time < day.start
            guard !earlier || recent.count < 5 else { break }
            guard !record.isBackground else { continue }
            if recent.count < 5 { recent.append(record) }
            guard !earlier, record.time < day.end else { continue }
            today += 1
            if record.isRefused { refusedToday += 1 }
            if let agent = record.agent { agents.insert(record.callerName ?? "Agent \(agent.prefix(8))") }
        }
        agentsToday = agents.sorted()
    }
}

private struct RecentRow: View {
    var record: AuditRecord
    var caller: (who: String, kind: String)

    var body: some View {
        HStack(spacing: 12) {
            Text(record.command ?? "—").font(.body.monospaced()).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 8)
            Text("\(caller.who) · \(caller.kind)").foregroundStyle(.secondary).lineLimit(1)
            StatusLabel(status: record.status).lineLimit(1)
            Text(record.time, format: .dateTime.hour().minute()).foregroundStyle(.tertiary).monospacedDigit()
        }
    }
}

/// A row that takes the person somewhere: the whole row is the button,
/// with the chevron of a grouped form's navigating row, and the focus ring
/// full keyboard access draws for any button.
private struct GoRow<Label: View>: View {
    var hint: String
    var action: () -> Void
    @ViewBuilder var label: Label

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                label
                Image(systemName: "chevron.forward")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityElement(children: .combine)
        .accessibilityHint(hint)
    }
}

/// A host's rules, vault and plugins at a glance, each opening its screen,
/// and the account's Google accounts when Google is on there; this Mac's
/// also offers setting up again.
private struct OnHostSection: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        let summary = app.summaries[host.name]
        let plugins = app.plugins(of: host)
        Section("On \(host.name)") {
            SummaryRow(host: host.spoken, title: "Rules", value: summary?.rules?.map(Self.rules), opens: "Rules") { app.place = .host(.rules) }
            SummaryRow(host: host.spoken, title: "Vault", value: summary?.vault?.map(Self.vault), opens: "Vault") { app.place = .host(.vault) }
            SummaryRow(host: host.spoken, title: "Plugins", value: plugins?.map(Self.plugins), opens: "Host Settings") {
                app.place = .host(.settings)
            }
            if case let .success(list)? = plugins, list.contains(where: { $0.name == "google" && $0.state == "enabled" }) {
                SummaryRow(host: host.spoken, title: "Google accounts", value: .success(Self.accounts(app.connections)), opens: "Connections") {
                    app.manageAccounts()
                }
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

    /// How many Google accounts Plow said are connected, once Connections
    /// has asked; Home doesn't ask.
    private static func accounts(_ connections: Connections?) -> String {
        guard case let .google(google)? = connections else { return "In Connections" }
        return switch google.accounts.count {
        case 0: "None connected"
        case 1: "1 connected"
        case let n: "\(n) connected"
        }
    }

    /// "Google: on · Messages, Contacts: off".
    private static func plugins(_ list: [Plugin]) -> String {
        let name = { (plugin: Plugin) in plugin.name.prefix(1).uppercased() + plugin.name.dropFirst() }
        let on = list.filter { $0.state == "enabled" }.map(name)
        let off = list.filter { $0.state != "enabled" }.map(name)
        return [on.isEmpty ? nil : on.joined(separator: ", ") + ": on", off.isEmpty ? nil : off.joined(separator: ", ") + ": off"]
            .compactMap { $0 }.joined(separator: " · ")
    }
}

/// One line of a host's summary: what latch said, or why it couldn't;
/// the row opens the screen it summarizes. Why it couldn't is its own row
/// under it, so its Details open rather than the screen.
private struct SummaryRow: View {
    var host: String
    var title: String
    /// nil until the host has answered.
    var value: Result<String, LatchError>?
    /// The screen the row opens.
    var opens: String
    var action: () -> Void

    var body: some View {
        GoRow(hint: "Opens \(opens)", action: action) {
            LabeledContent(title) {
                Text(shown).multilineTextAlignment(.trailing)
            }
        }
        if case let .failure(error) = value {
            ErrorText(error: error, host: host, titled: true).foregroundStyle(.secondary)
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
    var detail: String?
    /// latch's reason, in the app's words with its own under Details.
    var error: LatchError?

    var body: some View {
        Label {
            VStack(alignment: .leading, spacing: 2) {
                Text(text)
                if let detail { Text(LocalizedStringKey(detail)).foregroundStyle(.secondary) }
                if let error { ErrorText(error: error, titled: true).foregroundStyle(.secondary) }
            }
        } icon: {
            Image(systemName: symbol).foregroundStyle(.orange)
        }
    }
}

/// This Mac's host is stopped: say what that means, and start it.
private struct StartNeed: View {
    var lastError: LatchError?
    @Environment(AppModel.self) private var app

    var body: some View {
        HStack(alignment: .top) {
            Need(symbol: "pause.circle", text: "Latch isn't running on this Mac, so your agents can't reach it.",
                 detail: lastError == nil ? "Start it to let them in again." : nil, error: lastError)
            Spacer(minLength: 12)
            Button(app.startingHost ? "Starting…" : "Start Latch") { Task { await app.startHosting() } }
                .disabled(app.startingHost)
        }
        if let error = app.startHostError {
            ErrorText(error: error).foregroundStyle(.red)
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
                Text("\(lastSeen)Start Latch on \(host.name) to reach it again.")
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

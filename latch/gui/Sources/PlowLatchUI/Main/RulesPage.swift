import SwiftUI

/// The account's rules: each one's text and the agents and hosts it covers,
/// as latch merged them from every host. Filtered by an agent and a host;
/// a rule is added, changed and deleted in the window's sheets, and Try a
/// Request judges a request on a host's saved rules.
struct RulesPage: View {
    @Environment(AppModel.self) private var app
    @Environment(ScreenActions.self) private var actions: ScreenActions?

    var body: some View {
        VStack(spacing: 0) {
            // The page's own controls, above it: the window's toolbar is the
            // same on every page.
            HStack {
                RulesFilters()
                Spacer()
                Button("Try a Request…") { app.sheet = .tryRequest }
                Button("New Rule…", systemImage: "plus") { app.sheet = .rule(RuleEditor()) }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 10)
            Divider()
            RulesBody()
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .safeAreaInset(edge: .top, spacing: 0) { RulesNotices() }
        }
        // File › New Item…: New Rule, whatever the hosts say.
        .onAppear { actions?.newItem = { app.sheet = .rule(RuleEditor()) } }
    }
}

/// Agent and Host: the list shows only the rules covering that one, among
/// those it holds; nothing is read again.
private struct RulesFilters: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var app = app
        HStack(spacing: 16) {
            Picker("Agent", selection: $app.rulesFilter.agent) {
                Text("Any").tag(String?.none)
                Divider()
                ForEach(app.agents ?? []) { Text($0.displayName).tag(Optional($0.uid)) }
            }
            .fixedSize()
            Picker("Host", selection: $app.rulesFilter.host) {
                Text("Any").tag(String?.none)
                Divider()
                ForEach(app.rules?.hosts ?? []) { Text(app.ruleHostName($0.key)).tag(Optional($0.key)) }
            }
            .fixedSize()
        }
    }
}

/// Reading, a failed first read, no rules, none matching, or the list.
private struct RulesBody: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if let list = app.rules {
            if list.rules.isEmpty {
                ContentUnavailableView {
                    Label("No rules yet", systemImage: "checkmark.shield")
                } description: {
                    Text("Anything beyond everyday work needs your OK.")
                } actions: {
                    Button("New Rule…") { app.sheet = .rule(RuleEditor()) }
                }
            } else if app.ruleRows(list).isEmpty {
                ContentUnavailableView {
                    Label("No matching rules", systemImage: "line.3.horizontal.decrease.circle")
                } actions: {
                    Button("Clear Filters") { app.rulesFilter = .init() }
                }
            } else {
                RuleList(list: list)
            }
        } else if let error = app.rulesError {
            ContentUnavailableView {
                Label("Couldn't read your rules", systemImage: "exclamationmark.triangle")
            } description: {
                ErrorText(error: error, titled: true, place: .rules)
            } actions: {
                Button("Try Again") { Task { await app.refreshRules() } }
            }
        } else {
            ProgressView("Reading your rules…")
        }
    }
}

/// The rules the filters match, then what every agent may do anyway.
private struct RuleList: View {
    var list: RulesList
    @Environment(AppModel.self) private var app
    /// Set by a rule picked in the list: SwiftUI leaves the keyboard focus
    /// where it was (the sidebar), so Return and the arrow keys went there.
    @FocusState private var listFocused: Bool

    var body: some View {
        List(selection: selection) {
            Section {
                ForEach(app.ruleRows(list)) { RuleRow(rule: $0).tag($0.id) }
            }
            // Untagged: these can't be selected.
            Section("Defaults and your rules") {
                Text("Agents can look things up on the web or in your mail, edit files, run project builds and tests, and install packages a project already lists.")
                Text("Sending, account changes and other riskier work need your OK. Buying or sharing private data must be named in your rules or your own Plow messages.")
                Text("A permission adds to the defaults. “Do not”, “never” and “only” set limits; “unless I ask in chat” lets you approve it yourself. An agent quoting you cannot give chat permission.")
                Text("A task you give on a site includes signing in with its saved login there. Buying, sending or changing things after sign-in needs its own permission.")
            }
            .foregroundStyle(.secondary)
        }
        // Delete, and Edit › Delete: the selected rule, confirmed.
        .onDeleteCommand {
            if let rule = rule([app.selectedRule].compactMap { $0 }) { app.ask(.deleteRule(rule)) }
        }
        .contextMenu(forSelectionType: RulesList.Rule.ID.self) { ids in
            if let rule = rule(ids) {
                Button("Edit…", systemImage: "pencil") { app.sheet = .rule(RuleEditor(editing: rule)) }
                Button("Delete…", systemImage: "trash", role: .destructive) { app.ask(.deleteRule(rule)) }
            }
        } primaryAction: { ids in
            if let rule = rule(ids) { app.sheet = .rule(RuleEditor(editing: rule)) }
        }
        .focused($listFocused)
    }

    /// The list's selection; picked in the list, the list takes the
    /// keyboard focus.
    private var selection: Binding<RulesList.Rule.ID?> {
        Binding { app.selectedRule } set: { id in
            app.selectedRule = id
            // A pick, not SwiftUI clearing a row the filters hid.
            if id != nil { listFocused = true }
        }
    }

    /// The one rule picked, while the list shows it.
    private func rule(_ ids: some Collection<RulesList.Rule.ID>) -> RulesList.Rule? {
        guard ids.count == 1 else { return nil }
        return app.ruleRows(list).first { ids.contains($0.id) }
    }
}

/// A rule's text, whom it covers, and the hosts it applies to that
/// answered without it.
private struct RuleRow: View {
    var rule: RulesList.Rule
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(rule.text)
            Text(app.scopeLine(rule)).font(.callout).foregroundStyle(.secondary)
            if !rule.behindHosts.isEmpty {
                Text("Not on \(rule.behindHosts.map(app.ruleHostName).formatted(.list(type: .or))) yet")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

/// Over the list: hosts whose rules are too long, hosts latch couldn't
/// check, a refresh that failed while the list stays, and a delete that
/// failed.
private struct RulesNotices: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(spacing: 0) {
            let hosts = app.rules?.hosts ?? []
            let over = hosts.filter { $0.budget?.overBudget == true }
            if !over.isEmpty {
                notice(symbol: "exclamationmark.triangle", tint: .orange) {
                    Text("Rules on \(names(over)) are too long")
                    Text("Shorten or delete some.").foregroundStyle(.secondary)
                }
            }
            // One notice for each thing to do about hosts that couldn't be
            // checked, naming them all.
            let unchecked = Dictionary(grouping: hosts.filter { $0.delivery == "unknown" }, by: { Self.uncheckedDetail($0.reason) })
            ForEach(unchecked.keys.sorted(), id: \.self) { detail in
                notice(symbol: "exclamationmark.triangle", tint: .secondary) {
                    Text("Couldn't check \(names(unchecked[detail] ?? []))")
                    Text(detail).foregroundStyle(.secondary)
                }
            }
            if app.rules != nil, let error = app.rulesError {
                notice(symbol: "exclamationmark.triangle", tint: .orange) {
                    Text("Couldn't read your rules")
                    ErrorText(error: error, titled: true, place: .rules).foregroundStyle(.secondary)
                }
            }
            if let error = app.ruleDeleteError {
                notice(symbol: "exclamationmark.triangle", tint: .orange) {
                    Text("Couldn't delete the rule")
                    ErrorText(error: error, titled: true, place: .rules).foregroundStyle(.secondary)
                } action: {
                    Button("OK") { app.ruleDeleteError = nil }
                }
            }
        }
    }

    /// What to do about a host latch couldn't check, by why.
    static func uncheckedDetail(_ reason: String?) -> String {
        switch reason {
        case "host_outdated": "Its Latch cannot sync these rules. Update Latch there."
        case "protocol_mismatch": "The Latch versions cannot connect. Compare the versions and update the older one."
        case "bad_rules": "Latch can't read the rules saved there."
        default: "It may not have your latest rules. Open Rules again when it's reachable."
        }
    }

    private func names(_ hosts: [RulesList.Host]) -> String {
        hosts.map { app.ruleHostName($0.key) }.formatted(.list(type: .and))
    }

    private func notice(symbol: String, tint: some ShapeStyle, @ViewBuilder _ text: () -> some View,
                        @ViewBuilder action: () -> some View = { EmptyView() }) -> some View {
        HStack(alignment: .top) {
            Label {
                VStack(alignment: .leading, spacing: 2) { text() }
            } icon: {
                Image(systemName: symbol).foregroundStyle(tint)
            }
            Spacer()
            action()
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.bar)
    }
}

extension AppModel {
    /// An agent in a rule, by the name the account gives it.
    func ruleAgentName(_ uid: String) -> String {
        agents?.first { $0.uid == uid }?.displayName ?? "Agent \(uid.prefix(8))"
    }

    /// A host in a rule, by the name latch lists it under; this Mac's says so.
    func ruleHostName(_ key: String) -> String {
        guard let host = rules?.hosts.first(where: { $0.key == key }) else { return "Host \(key.prefix(8))" }
        return host.thisMac ? "\(host.name) (this Mac)" : host.name
    }

    /// Whom a rule covers, in a line: "Maple and Oak on all hosts".
    func scopeLine(_ rule: RulesList.Rule) -> String {
        let agents = switch rule.agents {
        case .all: "All agents"
        case let .only(uids): uids.map(ruleAgentName).formatted(.list(type: .and))
        }
        let hosts = switch rule.hosts {
        case .all: "all hosts"
        case let .only(keys): keys.map(ruleHostName).formatted(.list(type: .and))
        }
        return "\(agents) on \(hosts)"
    }
}

/// New Rule and a rule's Edit: what's typed and chosen, and saving it.
@Observable
final class RuleEditor {
    /// The rule being changed; nil for a new one.
    let original: RulesList.Rule?
    var text: String
    var agents: RulesList.Scope
    var hosts: RulesList.Scope
    private(set) var saving = false
    private(set) var deleting = false
    /// Delete…'s question, asked over this sheet.
    var confirmingDelete = false
    var error: LatchError?

    /// A new rule covers every agent on every host until it's narrowed.
    init() {
        original = nil
        text = ""
        agents = .all
        hosts = .all
    }

    init(editing rule: RulesList.Rule) {
        original = rule
        text = rule.text
        agents = rule.agents
        hosts = rule.hosts
    }

    var isNew: Bool { original == nil }
    var busy: Bool { saving || deleting }

    /// What stops a save, in words; nil when it can be saved.
    var problem: String? {
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return "Write the rule first." }
        if agents == .only([]) { return "Choose at least one agent." }
        if hosts == .only([]) { return "Choose at least one host." }
        return nil
    }

    /// Adds the rule, or changes only what changed: a scope left as it was
    /// isn't sent, so latch keeps it exactly as stored. Saved on this Mac,
    /// it's saved, whichever hosts latch reached; the sheet closes then,
    /// unless it was closed meanwhile.
    @discardableResult
    func save(app: AppModel) async -> Bool {
        guard problem == nil, !busy else { return false }
        saving = true
        defer { saving = false }
        do {
            if let original {
                let text = text == original.text ? nil : text
                let agents = agents == original.agents ? nil : agents
                let hosts = hosts == original.hosts ? nil : hosts
                if text != nil || agents != nil || hosts != nil {
                    try await app.editRule(original.id, text: text, agents: agents, hosts: hosts)
                }
            } else {
                try await app.addRule(text, agents: agents, hosts: hosts)
            }
            error = nil
            app.close(.rule(self))
            return true
        } catch {
            self.error = .from(error)
            return false
        }
    }

    /// Delete…, once confirmed: the rule goes, and so does the sheet.
    func delete(app: AppModel) async {
        guard let original, !busy else { return }
        deleting = true
        defer { deleting = false }
        do {
            try await app.removeRule(original)
            error = nil
            app.close(.rule(self))
        } catch {
            self.error = .from(error)
        }
    }

    /// A refused save in the app's words, naming the agent and host whose
    /// rules it would make too long; nil to say it as any error.
    func refusal(app: AppModel) -> String? {
        guard case let .failed(failure)? = error, failure.code == "rules_too_long", let over = failure.hosts?.first,
              let host = over.name else { return nil }
        let whose = over.agent.map { "the rules for \(app.ruleAgentName($0)) on \(host)" } ?? "the rules on \(host)"
        return "Your change wasn't saved because \(whose) would be too long. Shorten this rule or delete another."
    }
}

extension RulesList.Scope {
    /// A name ticked in the scope's menu: never while it's All.
    func picks(_ id: String) -> Bool {
        if case let .only(ids) = self { ids.contains(id) } else { false }
    }

    /// The scope with a name ticked or unticked: ticking one leaves All;
    /// unticking the last leaves none, never All.
    func picking(_ id: String, _ on: Bool) -> Self {
        let ids = if case let .only(ids) = self { ids } else { [String]() }
        return .only(on ? ids.filter { $0 != id } + [id] : ids.filter { $0 != id })
    }
}

/// New Rule, or a rule's Edit: its text, then the agents and hosts it
/// covers, each a menu of names with All first.
struct RuleSheet: View {
    var editor: RuleEditor
    @Environment(AppModel.self) private var app

    static let deleteQuestion = "Delete this rule?"
    static let deleteDetail = "This can't be undone."

    var body: some View {
        @Bindable var editor = editor
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text(editor.isNew ? "New Rule" : "Edit Rule").font(.headline)
                Text("Write it as you'd tell a person.").font(.callout).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            Divider()
            Form {
                Section {
                    TextEditor(text: $editor.text)
                        .font(.body)
                        .frame(minHeight: 80)
                        .accessibilityLabel("Rule")
                }
                Section {
                    ScopeMenu(title: "Agents", all: "All agents", scope: $editor.agents,
                              choices: (app.agents ?? []).map { ($0.uid, $0.displayName) }, name: app.ruleAgentName)
                    ScopeMenu(title: "Hosts", all: "All hosts", scope: $editor.hosts,
                              choices: (app.rules?.hosts ?? []).map { ($0.key, app.ruleHostName($0.key)) }, name: app.ruleHostName)
                } footer: {
                    Text("“All” includes agents and hosts you add later.").foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
            // What's typed during a save couldn't be saved by it.
            .disabled(editor.busy)
            Divider()
            HStack {
                if !editor.isNew {
                    Button("Delete…") { editor.confirmingDelete = true }
                        .disabled(editor.busy)
                }
                // A refused save is said here, beside Save, never below the fold.
                if let error = editor.error {
                    Label {
                        ErrorText(error: error, said: editor.refusal(app: app), place: .rules)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle")
                    }
                    .font(.callout).foregroundStyle(.red)
                }
                Spacer()
                Button("Cancel") { app.sheet = nil }
                    .keyboardShortcut(.cancelAction)
                Button(editor.saving ? "Saving…" : editor.isNew ? "Add" : "Save") {
                    Task { await editor.save(app: app) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(editor.busy || editor.problem != nil)
                .help(editor.problem ?? "")
            }
            .padding()
        }
        .frame(width: 480, height: 420)
        // The sheet's own question: an alert over the window would take the
        // sheet's place, and Cancel would lose what's typed.
        .confirmationDialog(Self.deleteQuestion, isPresented: $editor.confirmingDelete) {
            Button("Delete", role: .destructive) { Task { await editor.delete(app: app) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(Self.deleteDetail)
        }
    }
}

/// Agents or Hosts: All, then each name, ticked; the button sums them up.
private struct ScopeMenu: View {
    var title: String
    var all: String
    @Binding var scope: RulesList.Scope
    var choices: [(id: String, name: String)]
    var name: (String) -> String

    var body: some View {
        LabeledContent(title) {
            Menu(summary) {
                Toggle(all, isOn: Binding { scope == .all } set: { scope = $0 ? .all : .only([]) })
                Divider()
                ForEach(choices, id: \.id) { choice in
                    Toggle(choice.name, isOn: Binding { scope.picks(choice.id) } set: { scope = scope.picking(choice.id, $0) })
                }
            }
            .fixedSize()
        }
    }

    private var summary: String {
        switch scope {
        case .all: all
        case .only([]): "None"
        case let .only(ids): ids.map(name).formatted(.list(type: .and))
        }
    }
}

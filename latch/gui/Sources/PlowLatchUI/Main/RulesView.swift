import SwiftUI

/// A host's rules: those for every agent, each agent's own, and Try a
/// request against them.
struct HostRulesView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
            if let host = app.host {
                if !host.isThisMac, host.online == false {
                    ContentUnavailableView {
                        Label("\(host.name) is offline", systemImage: "wifi.slash")
                    } description: {
                        Text("Its rules are kept on \(host.name). Start Latch there to see and change them.")
                    }
                } else {
                    HostRules(host: host).id(host.id)
                }
            } else {
                ContentUnavailableView("No host", systemImage: "list.bullet.rectangle",
                                       description: Text("Rules are kept on each host. Pick one in the sidebar."))
            }
        }
        .navigationTitle("Rules")
        .navigationSubtitle(app.host?.name ?? "")
    }
}

private struct HostRules: View {
    var host: HostEntry
    private static let verdictAnchor = "verdict"
    @Environment(AppModel.self) private var app

    var body: some View {
        let rules = app.summaries[host.name]?.rules
        ScrollViewReader { scroller in
            Form {
                switch rules {
                case nil:
                    Section { ProgressView("Reading the rules on \(host.name)…").frame(maxWidth: .infinity) }
                case let .failure(error)?:
                    Section {
                        Label("Couldn't read the rules on \(host.name)", systemImage: "exclamationmark.triangle")
                        ErrorText(error: error, host: host.spoken, titled: true).foregroundStyle(.secondary)
                    }
                case let .success(show)?:
                    RulesEditor(title: "Rules for every agent on \(host.name)",
                                note: "Every agent is judged by these, on top of anything of its own.",
                                saved: show.everyAgent, host: host, agent: nil)
                    ForEach(app.agents ?? []) { agent in
                        RulesEditor(title: "\(agent.name)'s rules on \(host.name)",
                                    note: show.execBlocked(agent.uid)
                                        ? "\(agent.name) can't run programs on \(host.name), regardless of these rules."
                                        : "Only \(agent.name) is judged by these, with the rules for every agent.",
                                    saved: show.agent(agent.uid).map { RulesShow.Text(text: $0.text ?? "", revision: $0.revision) },
                                    host: host, agent: agent)
                            .id(agent.uid)
                    }
                    // A verdict arrives below the fold: it's brought into view.
                    TryPanel(host: host, everyAgentSaved: show.everyAgent?.text ?? "")
                        .id(Self.verdictAnchor)
                }
            }
            .formStyle(.grouped)
            // A pill or "Rules on…" asks for an agent's rules: scroll once
            // its editor is on screen, which may be only after the rules
            // and agents have loaded; then the ask is done.
            .onChange(of: app.rulesScrollTarget(on: host), initial: true) { _, uid in
                guard let uid else { return }
                app.rulesFocus = nil
                // The editors appear in this same update and aren't laid
                // out yet, so the scroll waits a moment; and it isn't
                // animated: an animated scrollTo in this Form never moves.
                Task {
                    try? await Task.sleep(for: .milliseconds(100))
                    scroller.scrollTo(uid, anchor: .top)
                }
            }
            .onChange(of: app.trial.results[host.name]) {
                scroller.scrollTo(Self.verdictAnchor, anchor: .bottom)
            }
        }
    }
}

/// Save (⌘S) for the rules editor with the keyboard focus. The editor
/// publishes it while it's focused in the front window and has a change to
/// save; the menu's Save runs it.
struct RulesSave {
    let key: AppModel.RulesKey
    let run: () -> Void
}

extension FocusedValues {
    @Entry var rulesSave: RulesSave?
}

/// The menu's one Save: the focused rules editor's, when there is one.
struct RulesCommands: Commands {
    @FocusedValue(\.rulesSave) private var save

    var body: some Commands {
        CommandGroup(replacing: .saveItem) {
            Button("Save") { save?.run() }
                .keyboardShortcut("s", modifiers: .command)
                .disabled(save == nil)
        }
    }
}

/// One set of rules: the saved text, edited in place, saved only when
/// changed and only over what was there when editing began.
struct RulesEditor: View {
    var title: String
    var note: String
    var saved: RulesShow.Text?
    var host: HostEntry
    var agent: AgentsList.Agent?
    @Environment(AppModel.self) private var app
    @FocusState private var focused: Bool

    private var key: AppModel.RulesKey { .init(host: host.name, agent: agent?.uid) }

    var body: some View {
        // The draft object, not its text: typing redraws only the editor
        // that holds it (`DraftEditor`).
        let draft = app.rulesDrafts[key]
        Section {
            if saved == nil, draft == nil {
                HStack {
                    Text(agent == nil ? "None yet." : "None of its own: it follows the rules for every agent.")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Add Rules…") {
                        begin()
                        focused = true
                    }
                }
            } else {
                DraftEditor(key: key, saved: saved, draft: draft, title: title, host: host, focused: $focused, begin: begin)
            }
        } header: {
            Text(title)
        } footer: {
            Text(note).foregroundStyle(.secondary)
        }
    }

    /// Editing begins from the rules saved now.
    private func begin() {
        app.rulesDrafts[key] = .init(text: saved?.text ?? "", base: saved?.revision)
        app.rulesErrors[key] = nil
    }
}

/// The text of one set of rules, the draft's while there is one, with its
/// Revert and Save; it publishes Save (⌘S) while it has the focus.
private struct DraftEditor: View {
    var key: AppModel.RulesKey
    var saved: RulesShow.Text?
    var draft: AppModel.RulesDraft?
    var title: String
    var host: HostEntry
    var focused: FocusState<Bool>.Binding
    var begin: () -> Void
    @Environment(AppModel.self) private var app

    var body: some View {
        let saving = app.savingRules.contains(key)
        let save = app.canSaveRules(key) ? RulesSave(key: key) { Task { await app.saveRulesDraft(key) } } : nil
        TextEditor(text: Binding { draft?.text ?? saved?.text ?? "" } set: { new in
            app.typeRules(new, at: key, from: saved)
        })
        .font(.body)
        // What's typed during a save couldn't be saved by it.
        .disabled(saving)
        .frame(minHeight: 70)
        .accessibilityLabel(title)
        .focused(focused)
        .focusedValue(\.rulesSave, save)
        if let error = app.rulesErrors[key] {
            Label { ErrorText(error: error, host: host.spoken) } icon: { Image(systemName: "exclamationmark.triangle") }
                .foregroundStyle(.red)
        }
        if draft != nil {
            HStack {
                Spacer()
                Button("Revert") { Task { await app.revertRules(key) } }
                    .disabled(saving)
                Button(saving ? "Saving…" : "Save") { save?.run() }
                    .buttonStyle(.borderedProminent)
                    .disabled(save == nil)
            }
        }
    }
}

/// Try a request: the verdict the gatekeeper would give it on this host
/// now, as an agent or a new agent. Nothing runs.
private struct TryPanel: View {
    var host: HostEntry
    var everyAgentSaved: String
    @Environment(AppModel.self) private var app
    @FocusState private var editingRequest: Bool

    var body: some View {
        @Bindable var trial = app.trial
        Section {
            Picker("Agent", selection: $trial.caller) {
                ForEach(app.agents ?? []) { Text($0.name).tag(Caller.agent($0.uid)) }
                Text("A new agent").tag(Caller.newAgent)
            }
            HStack {
                    TextField("Request", text: $trial.request, prompt: Text("exec -- ls ~/Projects"))
                        .font(.body.monospaced())
                        .focused($editingRequest)
                        .onSubmit { Task { await trial.run(on: host, app: app) } }
                    Menu("Examples") {
                        ForEach(Trial.examples) { example in
                            Button {
                                trial.use(example)
                                editingRequest = true
                            } label: {
                                Label(example.title, systemImage: example.symbol)
                            }
                        }
                    }
                    .fixedSize()
            }
            Toggle("Judge against draft rules for every agent", isOn: $trial.useDraft)
            if trial.useDraft {
                TextEditor(text: $trial.draft)
                    .font(.body)
                    .frame(minHeight: 50)
                    .accessibilityLabel("Draft rules for every agent")
                    .onAppear { if trial.draft.isEmpty { trial.draft = everyAgentSaved } }
            }
            HStack {
                Text("Defaults and these rules; chat isn't included; nothing runs. Previews don't appear in Activity. Each try is billed like a real judgment, about $0.0001.")
                    .font(.callout).foregroundStyle(.secondary)
                Spacer()
                Button(trial.trying ? "Trying…" : "Try") { Task { await trial.run(on: host, app: app) } }
                    .disabled(trial.trying || trial.words == nil || trial.words?.isEmpty == true)
            }
            switch trial.results[host.name] {
            case let .success(verdict)?: VerdictView(verdict: verdict, host: host.spoken)
            case let .failure(error)?:
                Label {
                    ErrorText(error: error, host: host.spoken, place: .tryRequest(connector: trial.words?.first))
                } icon: {
                    Image(systemName: "exclamationmark.triangle")
                }
                .foregroundStyle(.red)
            case nil: EmptyView()
            }
        } header: {
            Text("Try a request")
        } footer: {
            Text("Type the request as an agent would send it, starting with exec, file, google, messages or contacts.")
                .foregroundStyle(.secondary)
        }
    }

}

/// Who a try is judged as.
enum Caller: Hashable { case newAgent, agent(String) }

/// Try a request's state, kept while the person moves around: the request,
/// who it's tried as, a draft, and each host's last verdict.
@Observable
final class Trial {
    var request = ""
    var caller = Caller.newAgent
    var draft = ""
    var useDraft = false
    private(set) var trying = false
    private(set) var results: [String: Result<RulesTry, LatchError>] = [:]

    var words: [String]? { ShellWords.split(request) }

    /// The rules screen's examples that are words to edit: the Instacart
    /// sign-in, a saved login's fill, has none.
    static var examples: [Onboarding.Example] { Onboarding.examples.filter { $0.request.words != nil } }

    /// An example from the rules screen, in the request field to edit.
    func use(_ example: Onboarding.Example) {
        request = ShellWords.join(example.request.words ?? [])
    }

    func run(on host: HostEntry, app: AppModel) async {
        guard let words, !words.isEmpty else { return }
        trying = true
        defer { trying = false }
        let agent: String? = if case let .agent(uid) = caller { uid } else { nil }
        do {
            results[host.name] = .success(try await app.tryRequest(words, on: host, agent: agent, newAgent: caller == .newAgent,
                                                                   draft: useDraft ? draft : nil))
        } catch {
            results[host.name] = .failure(.from(error))
        }
    }

    /// For snapshots: a host's verdict.
    func stage(_ verdict: RulesTry, request: String, caller: Caller, on host: String) {
        self.request = request
        self.caller = caller
        results[host] = .success(verdict)
    }
}

/// A preview's verdict, in words, with the gatekeeper's reading.
struct VerdictView: View {
    var verdict: RulesTry
    /// The host it was tried on, as its sentences name it.
    var host: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label {
                Text(title).font(.headline)
            } icon: {
                Image(systemName: symbol).foregroundStyle(tint)
            }
            if let refused = verdict.refused {
                // The title says the host refuses it; why, in the app's words.
                ErrorText(error: .failed(refused), host: host, titled: true, place: .tryRequest(connector: verdict.connector))
                    .foregroundStyle(.secondary)
            }
            if let gate = verdict.gate {
                if let source = gate.source {
                    LabeledContent("Read", value: Why.sources[source] ?? source)
                }
                if let p = gate.p {
                    Likelihood(title: "Covered", value: p["COVERED"], tint: .green)
                    Likelihood(title: "Not covered", value: p["NOT_COVERED"], tint: .orange)
                    Likelihood(title: "Forbidden", value: p["FORBIDDEN"], tint: .red)
                }
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .combine)
    }

    var title: String {
        switch verdict.verdict {
        case "allow": "Would be allowed"
        case "not_covered": "Needs your OK: would be refused"
        case "owner_said_no": "You said no: would be refused"
        case "host_refused": "The host refuses it: would be refused"
        default: verdict.verdict.replacingOccurrences(of: "_", with: " ")
        }
    }

    private var symbol: String {
        switch verdict.verdict {
        case "allow": "checkmark.circle.fill"
        case "not_covered": "questionmark.circle.fill"
        case "host_refused": "nosign"
        default: "xmark.circle.fill"
        }
    }

    private var tint: Color {
        switch verdict.verdict {
        case "allow": .green
        case "not_covered": .orange
        default: .red
        }
    }
}

/// A request typed as in a shell: words split on spaces, with quotes and
/// backslashes as a shell reads them; nil while a quote is open.
enum ShellWords {
    static func split(_ line: String) -> [String]? {
        var words: [String] = []
        var word = ""
        var inWord = false
        var quote: Character?
        var escaped = false
        for c in line {
            if escaped {
                word.append(c); escaped = false; inWord = true
            } else if c == "\\", quote != "'" {
                escaped = true
            } else if let q = quote {
                if c == q { quote = nil } else { word.append(c) }
            } else if c == "'" || c == "\"" {
                quote = c; inWord = true
            } else if c.isWhitespace {
                if inWord { words.append(word); word = ""; inWord = false }
            } else {
                word.append(c); inWord = true
            }
        }
        guard quote == nil, !escaped else { return nil }
        if inWord { words.append(word) }
        return words
    }

    /// The words as one line that `split` gives back: bare when plain,
    /// else in single quotes, or double quotes with `\` escapes for one
    /// holding a single quote.
    static func join(_ words: [String]) -> String {
        words.map { word in
            if !word.isEmpty, !word.contains(where: { $0.isWhitespace || "'\"\\".contains($0) }) { return word }
            if !word.contains("'") { return "'\(word)'" }
            return "\"" + word.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
        }
        .joined(separator: " ")
    }
}

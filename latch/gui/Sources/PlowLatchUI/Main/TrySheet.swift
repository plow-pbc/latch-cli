import SwiftUI

/// Try a Request: the verdict the gatekeeper would give a request on a host
/// now, on its saved rules, as an agent or a new agent. Nothing runs.
struct TrySheet: View {
    @Environment(AppModel.self) private var app
    @FocusState private var editingRequest: Bool

    var body: some View {
        @Bindable var trial = app.trial
        let host = app.hosts.first { $0.name == trial.host }
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Try a Request").font(.headline)
                Text("Nothing runs, and tries don't appear in Activity.").font(.callout).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            Divider()
            Form {
                Section {
                    Picker("Host", selection: $trial.host) {
                        ForEach(app.hosts) { Text($0.isThisMac ? "\($0.name) (this Mac)" : $0.name).tag(Optional($0.name)) }
                    }
                    Picker("Agent", selection: $trial.caller) {
                        ForEach(app.agents ?? []) { Text($0.displayName).tag(Caller.agent($0.uid)) }
                        Text("A new agent").tag(Caller.newAgent)
                    }
                    HStack {
                        TextField("Request", text: $trial.request, prompt: Text("exec -- ls ~/Projects"))
                            .font(.body.monospaced())
                            .focused($editingRequest)
                            .onSubmit { Task { await trial.run(app: app) } }
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
                    HStack {
                        Text("Judged on the defaults and your rules, without chat. About $0.0001 a try.")
                            .font(.callout).foregroundStyle(.secondary)
                        Spacer()
                        Button(trial.trying ? "Trying…" : "Try") { Task { await trial.run(app: app) } }
                            .disabled(trial.trying || host == nil || trial.words == nil || trial.words?.isEmpty == true)
                    }
                    switch trial.result {
                    case let .success(verdict)?: VerdictView(verdict: verdict, host: host?.spoken)
                    case let .failure(error)?:
                        Label {
                            ErrorText(error: error, host: host?.spoken, place: .tryRequest(connector: trial.words?.first))
                        } icon: {
                            Image(systemName: "exclamationmark.triangle")
                        }
                        .foregroundStyle(.red)
                    case nil: EmptyView()
                    }
                } footer: {
                    Text("Type the request as an agent would send it, starting with exec, file, google, messages or contacts.")
                        .foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
            Divider()
            HStack {
                Spacer()
                Button("Done") { app.sheet = nil }
                    .keyboardShortcut(.cancelAction)
            }
            .padding()
        }
        .frame(width: 520, height: 580)
        // A host no longer listed, or none picked yet: the first, this Mac
        // when it hosts.
        .onAppear { if host == nil { trial.host = app.hosts.first?.name } }
    }
}

/// Who a try is judged as.
enum Caller: Hashable { case newAgent, agent(String) }

/// Try a Request's state, kept while the person moves around: the host,
/// who it's tried as, the request, and the last verdict with what it was
/// for, shown only while all three still are.
@Observable
final class Trial {
    var host: String?
    var caller = Caller.newAgent
    var request = ""
    private(set) var trying = false
    private(set) var last: Tried?

    struct Tried: Equatable {
        var host: String
        var caller: Caller
        var request: String
        var result: Result<RulesTry, LatchError>
    }

    var words: [String]? { ShellWords.split(request) }

    /// The last verdict, while the host, caller and request are its own.
    var result: Result<RulesTry, LatchError>? {
        guard let last, last.host == host, last.caller == caller, last.request == request else { return nil }
        return last.result
    }

    /// Rules' examples that are words to edit: the Instacart sign-in, a
    /// saved login's fill, has none.
    static var examples: [Onboarding.Example] { Onboarding.examples.filter { $0.request.words != nil } }

    /// An example, in the request field to edit.
    func use(_ example: Onboarding.Example) {
        request = ShellWords.join(example.request.words ?? [])
    }

    /// Tries the request on the host picked, and no other. An answer for a
    /// host, caller or request no longer picked is dropped.
    func run(app: AppModel) async {
        guard let words, !words.isEmpty, let name = host, let host = app.hosts.first(where: { $0.name == name }) else { return }
        let caller = caller, request = request
        trying = true
        defer { trying = false }
        let agent: String? = if case let .agent(uid) = caller { uid } else { nil }
        let result: Result<RulesTry, LatchError>
        do {
            result = .success(try await app.tryRequest(words, on: host, agent: agent, newAgent: caller == .newAgent))
        } catch {
            result = .failure(.from(error))
        }
        guard self.host == name, self.caller == caller, self.request == request else { return }
        last = Tried(host: name, caller: caller, request: request, result: result)
    }

    /// For snapshots: a verdict on a host.
    func stage(_ verdict: RulesTry, request: String, caller: Caller, on host: String) {
        self.host = host
        self.request = request
        self.caller = caller
        last = Tried(host: host, caller: caller, request: request, result: .success(verdict))
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

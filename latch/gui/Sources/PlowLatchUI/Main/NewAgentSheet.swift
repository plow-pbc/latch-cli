import AppKit
import SwiftUI

/// The New Agent sheet's state: a cloud agent from Plow's catalog or your
/// own, its name and phone line, then the agent latch created.
@Observable
final class NewAgent {
    enum Kind: Hashable { case cloud, own }

    var kind = Kind.cloud
    var name = ""
    /// The catalog kind's id, for a cloud agent.
    var cloudKind: String?
    /// The line's uid.
    var line: String?
    /// Where your own agent's key is written: a new file.
    var tokenFile: URL?
    private(set) var catalog: Result<AgentCatalog, LatchError>?
    private(set) var lines: Result<AgentLines, LatchError>?
    private(set) var creating = false
    var error: LatchError?
    private(set) var created: AgentCreated?

    var freeLines: [AgentLines.Line] {
        guard case let .success(lines)? = lines else { return [] }
        return lines.lines.filter(\.available)
    }

    /// What stops a create, in words; nil when it can go.
    var problem: String? {
        if name.trimmingCharacters(in: .whitespaces).isEmpty { return "Give it a name." }
        if kind == .cloud, cloudKind == nil { return "Pick which cloud agent." }
        if kind == .own, tokenFile == nil { return "Choose where to save its key." }
        if line == nil { return "Pick a free phone line." }
        return nil
    }

    /// Reads Plow's catalog and the account's lines; the first free line
    /// is picked.
    func load(app: AppModel) async {
        catalog = await AppModel.result { try await app.agentCatalog() }
        lines = await AppModel.result { try await app.agentLines() }
        if line == nil { line = freeLines.first?.uid }
    }

    func create(app: AppModel) async {
        guard problem == nil, !creating, let line else { return }
        creating = true
        defer { creating = false }
        do {
            created = try await app.createAgent(name: name.trimmingCharacters(in: .whitespaces),
                                                cloud: kind == .cloud ? cloudKind : nil,
                                                tokenFile: kind == .own ? tokenFile : nil, line: line)
            error = nil
        } catch {
            self.error = .from(error)
        }
    }

    /// While the sheet shows a cloud agent Plow is still starting, its
    /// status is read again every 3 seconds, 60 times at most (3 minutes):
    /// Plow has no stream to follow. It runs in the sheet's own task, so it
    /// stops the moment the sheet closes.
    func followStart(app: AppModel) async {
        for _ in 0..<60 {
            guard let agent = created?.agent, agent.kind == "cloud", agent.status == "provisioning" else { return }
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            if let now = try? await app.agentStatus(agent.uid) { created?.agent = now }
        }
    }

    /// For snapshots: as latch said.
    func stage(catalog: AgentCatalog, lines: AgentLines, created: AgentCreated? = nil) {
        self.catalog = .success(catalog)
        self.lines = .success(lines)
        if line == nil { line = freeLines.first?.uid }
        self.created = created
    }
}

struct NewAgentSheet: View {
    var flow: NewAgent
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("New Agent").font(.headline)
                Text("An agent has its own phone line and a chat with you.").font(.callout).foregroundStyle(.secondary)
            }
            .padding()
            Divider()
            if let created = flow.created {
                CreatedView(created: created)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .task(id: created.agent.uid) {
                        guard !app.still else { return }
                        await flow.followStart(app: app)
                    }
            } else {
                form
            }
            Divider()
            HStack {
                if let error = flow.error {
                    Label { ErrorText(error: error) } icon: { Image(systemName: "exclamationmark.triangle") }
                        .font(.callout).foregroundStyle(.red)
                }
                Spacer()
                if flow.created == nil {
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                    Button(flow.creating ? "Creating…" : flow.kind == .cloud ? "Start Agent" : "Create Agent") {
                        Task { await flow.create(app: app) }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(flow.creating || flow.problem != nil)
                    .help(flow.problem ?? "")
                } else {
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
            }
            .padding()
        }
        .frame(width: 520, height: 500)
        .task {
            guard !app.still else { return }
            await flow.load(app: app)
        }
    }

    private var form: some View {
        @Bindable var flow = flow
        return Form {
            Section {
                Picker("Kind", selection: $flow.kind) {
                    Text("Cloud agent").tag(NewAgent.Kind.cloud)
                    Text("Your own agent").tag(NewAgent.Kind.own)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                TextField("Name", text: $flow.name, prompt: Text("Required"))
                if flow.kind == .cloud {
                    switch flow.catalog {
                    case nil: LabeledContent("Agent") { ProgressView().controlSize(.small) }
                    case let .failure(error)?: LabeledContent("Agent") { ErrorText(error: error).foregroundStyle(.red) }
                    case let .success(catalog)?:
                        Picker("Agent", selection: $flow.cloudKind) {
                            Text("Choose…").tag(String?.none)
                            ForEach(catalog.kinds) { Text($0.name).tag(Optional($0.id)) }
                        }
                    }
                } else {
                    LabeledContent("Save key to") {
                        HStack {
                            Text(flow.tokenFile.map { ($0.path as NSString).abbreviatingWithTildeInPath } ?? "Not chosen")
                                .foregroundStyle(flow.tokenFile == nil ? .secondary : .primary)
                                .lineLimit(1).truncationMode(.middle)
                            Button("Choose…") { chooseKeyFile() }
                        }
                    }
                }
                switch flow.lines {
                case nil: LabeledContent("Phone line") { ProgressView().controlSize(.small) }
                case let .failure(error)?: LabeledContent("Phone line") { ErrorText(error: error).foregroundStyle(.red) }
                case .success?:
                    if flow.freeLines.isEmpty {
                        LabeledContent("Phone line") { Text("No free lines").foregroundStyle(.secondary) }
                    } else {
                        Picker("Phone line", selection: $flow.line) {
                            ForEach(flow.freeLines) { line in
                                Text(line.name.map { "\($0) · \(line.number)" } ?? line.number).tag(Optional(line.uid))
                            }
                        }
                    }
                }
            } footer: {
                Text(flow.kind == .cloud ? "Plow starts it and runs it for you. You text it on its line."
                     : "Latch saves the agent's key to this file and never shows it. Give the file to your agent so it can log in. You get the key only once.")
                    .foregroundStyle(.secondary)
            }
            Section {
                Label {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("It can reach all your hosts.").font(.headline)
                        Text("\(app.hosts.map(\.name).joined(separator: ", ")). It starts with each host's rules for every agent and no rules of its own; add rules for it on any host later.")
                            .foregroundStyle(.secondary)
                    }
                } icon: {
                    Image(systemName: "person.2")
                }
            }
        }
        .formStyle(.grouped)
    }

    /// A save panel for a new file: latch writes the key there, private.
    private func chooseKeyFile() {
        let panel = NSSavePanel()
        panel.title = "Save the Agent's Key"
        panel.message = "Latch writes the key to a new file only you can read."
        panel.nameFieldStringValue = "\(flow.name.isEmpty ? "agent" : flow.name).latch-key"
        panel.directoryURL = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
        panel.canCreateDirectories = true
        if panel.runModal() == .OK { flow.tokenFile = panel.url }
    }
}

/// The agent latch created: your own agent's key file, or a cloud agent's
/// start.
private struct CreatedView: View {
    var created: AgentCreated
    @Environment(AppModel.self) private var app

    var body: some View {
        let agent = created.agent
        VStack(spacing: 14) {
            Image(systemName: agent.kind == "cloud" ? "cloud" : "checkmark.circle")
                .font(.system(size: 40)).foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text("\(agent.name) is created").font(.title3.weight(.semibold))
            Text("Its phone line is \(agent.line?.number ?? "not assigned yet"). It can reach all your hosts.")
                .foregroundStyle(.secondary)
            if let file = created.tokenFile {
                VStack(spacing: 8) {
                    Text("Its key is saved to \((file as NSString).abbreviatingWithTildeInPath). Give this file to your agent; it logs in with:")
                        .multilineTextAlignment(.center)
                    Text("PLOW_TOKEN_FILE=\(file) latch login")
                        .font(.body.monospaced()).textSelection(.enabled)
                        .padding(8).background(.quaternary, in: .rect(cornerRadius: 6))
                    Button("Show in Finder") { app.reveal(URL(filePath: file)) }
                }
            } else if let status = agent.status {
                HStack(spacing: 8) {
                    if status == "provisioning" { ProgressView().controlSize(.small) }
                    Text(status == "provisioning" ? "Plow is starting it…" : "Status in Plow: \(status)")
                }
                .foregroundStyle(.secondary)
            }
        }
        .multilineTextAlignment(.center)
        .padding(24)
    }
}

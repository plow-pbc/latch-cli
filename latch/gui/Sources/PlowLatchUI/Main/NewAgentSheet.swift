import SwiftUI

/// The New Agent sheet's state: what to text Plow's number to start a cloud
/// agent, and the wait for the agent Plow sets up.
@Observable
final class NewAgent {
    private(set) var start: Result<AgentStartByText, LatchError>?
    /// The kind picked, by its id.
    var kind: String?

    var picked: AgentStartByText.Kind? {
        guard case let .success(start)? = start else { return nil }
        return start.kinds.first { $0.id == kind } ?? start.kinds.first
    }

    /// Reads what to text; Plow's own choice is picked.
    func load(app: AppModel) async {
        guard let start = await AppModel.result({ try await app.agentStartByText() }) else { return }
        self.start = start
        if kind == nil, case let .success(read) = start { kind = read.default ?? read.kinds.first?.id }
    }

    /// How long to wait before each read of the account's agents: every
    /// 3 seconds for the first minute, while a text is likely on its way,
    /// then every 10 seconds for nine more. Plow has no stream to follow.
    static let waits: [Duration] = Array(repeating: .seconds(3), count: 20) + Array(repeating: .seconds(10), count: 54)

    /// Reads the account's agents until one appears that wasn't there when
    /// the sheet opened; then the sheet closes on Agents with it picked.
    /// It runs in the sheet's own task, so it stops when the sheet closes.
    func watch(app: AppModel, waits: [Duration] = NewAgent.waits) async {
        if app.agents == nil, await !app.refreshAgentList() { return }
        // Without a first list, every agent would look new.
        guard let agents = app.agents else { return }
        let known = Set(agents.map(\.uid))
        for wait in waits {
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled, await app.refreshAgentList() else { return }
            if let new = app.agents?.first(where: { !known.contains($0.uid) }) {
                app.selectedAgent = new.uid
                // Closing the sheet ends this task: its standings first.
                await app.refreshAgents()
                app.sheet = nil
                return
            }
        }
    }

    /// For snapshots: as latch said.
    func stage(_ start: AgentStartByText) {
        self.start = .success(start)
        kind = start.default ?? start.kinds.first?.id
    }

    func fail(_ error: LatchError) { start = .failure(error) }
}

struct NewAgentSheet: View {
    var flow: NewAgent
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("New Agent").font(.headline)
                Text("Start a cloud agent by texting Plow from your phone.").font(.callout).foregroundStyle(.secondary)
            }
            .padding()
            Divider()
            Group {
                switch flow.start {
                case nil:
                    ProgressView()
                case let .failure(error)?:
                    ContentUnavailableView {
                        Label("Couldn't read how to start an agent", systemImage: "exclamationmark.triangle")
                    } description: {
                        ErrorText(error: error, titled: true)
                    } actions: {
                        Button("Try Again") { Task { await flow.load(app: app) } }
                    }
                case let .success(start)?:
                    if let picked = flow.picked {
                        TextToStart(start: start, picked: picked, flow: flow)
                    } else {
                        ContentUnavailableView("Plow offers no cloud agents right now", systemImage: "cloud")
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                if flow.picked != nil {
                    ProgressView().controlSize(.small)
                    Text("It shows up in Agents once Plow sets it up.").font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Done") { app.sheet = nil }
                    .keyboardShortcut(.defaultAction)
            }
            .padding()
        }
        .frame(width: 560, height: 460)
        .task {
            guard !app.still else { return }
            await flow.load(app: app)
            await flow.watch(app: app)
        }
    }
}

/// The text to send: a QR code for a phone's camera, and the number and
/// the text to type or copy.
private struct TextToStart: View {
    var start: AgentStartByText
    var picked: AgentStartByText.Kind
    var flow: NewAgent
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var flow = flow
        HStack(alignment: .top, spacing: 24) {
            QRCodeView(payload: picked.smsto)
                .frame(width: 180, height: 180)
                .padding(10)
                .background(.white, in: .rect(cornerRadius: 10))
                .accessibilityLabel("QR code: text “\(picked.phrase)” to \(number)")
            VStack(alignment: .leading, spacing: 14) {
                if start.kinds.count > 1 {
                    Picker("Agent", selection: $flow.kind) {
                        ForEach(start.kinds) { Text($0.name).tag(Optional($0.id)) }
                    }
                    .fixedSize()
                }
                Copyable(title: "Text", value: number) { app.copy(start.number) }
                Copyable(title: "Message", value: picked.phrase) { app.copy(picked.phrase) }
                Text("Scan the code with your phone's camera, or text this from your phone. Plow sets the agent up on a number of its own and texts you back.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if let link = URL(string: picked.smsLink) {
                    Button("Open in Messages") { app.open(link) }
                        .help("Messages sends it from this Mac, as your phone would")
                }
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private var number: String { PhoneNumber(start.number)?.formatted() ?? start.number }
}

/// A value to read or copy, under its title.
private struct Copyable: View {
    var title: String
    var value: String
    var copy: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(.caption).foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline) {
                Text(value).font(.body.monospaced()).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                Button("Copy", action: copy).controlSize(.small)
                    .accessibilityLabel("Copy \(title.lowercased())")
            }
        }
    }
}

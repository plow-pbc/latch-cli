import SwiftUI

/// Change Line's state: the phone lines latch lists, the one picked, and
/// the move to it.
@Observable
final class ChangeLine {
    /// The agent to move, as Agents listed it.
    let agent: AgentsList.Agent
    private(set) var lines: Result<[AgentLines.Line], LatchError>?
    /// The line picked, by its uid.
    var picked: String?
    private(set) var moving = false
    /// Why the last move was refused; the sheet stays, to pick another.
    var error: LatchError?

    init(_ agent: AgentsList.Agent) { self.agent = agent }

    /// The lines it can move to: those none of the account's agents holds,
    /// so never its own.
    var choices: [AgentLines.Line] {
        guard case let .success(lines)? = lines else { return [] }
        return lines.filter(\.available)
    }

    /// What a move does, said before the person makes it.
    var effects: String {
        let old = agent.phone.map { " \($0) stops answering." } ?? ""
        let after = agent.kind == "cloud" ? "Plow restarts it on the new line." : "The program you run for it keeps running."
        return "It keeps its credential and settings, and takes the new line's name and email.\(old) \(after)"
    }

    func load(app: AppModel) async {
        guard let read = await AppModel.result({ try await app.agentLines() }) else { return }
        lines = read.map(\.lines)
    }

    /// Moves the agent to the line picked; true once moved, and the sheet
    /// closes, unless it was closed meanwhile (another may show by now).
    @discardableResult
    func move(app: AppModel) async -> Bool {
        guard let picked, !moving else { return false }
        moving = true
        defer { moving = false }
        do {
            try await app.moveAgent(agent, to: picked)
            error = nil
            app.close(.changeLine(self))
            return true
        } catch {
            self.error = .from(error)
            return false
        }
    }

    /// For snapshots: as latch said.
    func stage(_ lines: AgentLines, picked: String?, error: LatchError? = nil) {
        self.lines = .success(lines.lines)
        self.picked = picked
        self.error = error
    }
}

/// An agent's Change Line…: the free lines to pick from, what a move does,
/// and Change Line, which moves it.
struct ChangeLineSheet: View {
    var flow: ChangeLine
    @Environment(AppModel.self) private var app

    var body: some View {
        @Bindable var flow = flow
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Change Line").font(.headline)
                if let phone = flow.agent.phone {
                    Text("\(flow.agent.displayName) answers on \(phone).").font(.callout).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            Divider()
            Group {
                switch flow.lines {
                case nil:
                    ProgressView()
                case let .failure(error)?:
                    ContentUnavailableView {
                        Label("Couldn't list the phone lines", systemImage: "exclamationmark.triangle")
                    } description: {
                        ErrorText(error: error, titled: true)
                    } actions: {
                        Button("Try Again") { Task { await flow.load(app: app) } }
                    }
                case .success?:
                    if flow.choices.isEmpty {
                        ContentUnavailableView("No free lines", systemImage: "phone",
                                               description: Text("Each line already has one of your agents."))
                    } else {
                        Form {
                            Section {
                                Picker("Move to", selection: $flow.picked) {
                                    ForEach(flow.choices) { line in
                                        line.label.tag(Optional(line.uid))
                                    }
                                }
                                .pickerStyle(.radioGroup)
                            } footer: {
                                Text("Plow moves an agent only to a line you already have a chat on, such as one an agent was moved off.")
                                    .foregroundStyle(.secondary)
                            }
                            Section {
                                Text(flow.effects)
                            }
                        }
                        .formStyle(.grouped)
                        .disabled(flow.moving)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                // A refused move is said here, beside Change Line.
                if let error = flow.error {
                    Label { ErrorText(error: error, place: .changeLine) } icon: { Image(systemName: "exclamationmark.triangle") }
                        .font(.callout).foregroundStyle(.red)
                }
                Spacer()
                Button("Cancel") { app.sheet = nil }
                    .keyboardShortcut(.cancelAction)
                Button(flow.moving ? "Changing…" : "Change Line") {
                    Task { await flow.move(app: app) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(flow.moving || flow.picked == nil)
            }
            .padding()
        }
        .frame(width: 480, height: 440)
        .task {
            guard !app.still else { return }
            await flow.load(app: app)
        }
    }
}

extension AgentLines.Line {
    /// Its persona's name, then its number as people write it.
    var label: Text {
        let number = PhoneNumber(self.number)?.formatted() ?? self.number
        guard let name, !name.isEmpty else { return Text(number) }
        return Text("\(name)  \(Text(number).foregroundStyle(.secondary))")
    }
}

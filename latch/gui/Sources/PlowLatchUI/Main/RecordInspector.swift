import AppKit
import QuickLook
import SwiftUI

/// One audit record in full: who asked, what it could touch, why the
/// gatekeeper allowed or refused it, and what ran.
struct RecordInspector: View {
    var record: AuditRecord
    var caller: (who: String, kind: String)
    var hostName: String
    /// nil for this Mac's own host, else the host `--host` names.
    var host: String?
    @Environment(AppModel.self) private var app
    /// A browser plan's screenshots, for the record shown.
    @State private var shots: RecordShots?
    /// The screenshot or gatekeeper request open in Quick Look.
    @State private var preview: URL?
    /// Why the gatekeeper's request could not be read.
    @State private var judgeFailure: String?
    /// The read of the record shown; another record's is cancelled.
    @State private var judging: Task<Void, Never>?

    var body: some View {
        Form {
            Section {
                StatusLabel(status: record.status)
                if let command = record.command {
                    Text(command)
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Section("Request") {
                LabeledContent("Caller", value: "\(caller.who) · \(caller.kind)")
                LabeledContent("Time", value: record.time.formatted(date: .abbreviated, time: .standard))
                LabeledContent("Operation", value: record.op)
                LabeledContent("Outcome") { Text(record.outcome).font(.body.monospaced()) }
            }
            if let shots, shots.record == record.id {
                ShotsSection(shots: shots, hostName: hostName) { open($0) }
            } else if record.isScreenshot {
                Section("Screenshot") {
                    Text(RecordShots.standalone).foregroundStyle(.secondary)
                }
            }
            if record.op == "exec", let args = record.args {
                Section("What it could touch") {
                    LabeledContent("Folder") { Text(args.workdir ?? "—").font(.body.monospaced()).textSelection(.enabled) }
                    LabeledContent("Writes") {
                        Text((args.writes ?? []).isEmpty ? "None" : (args.writes ?? []).joined(separator: "\n"))
                            .font(.body.monospaced())
                    }
                    LabeledContent("Network", value: args.network == true ? "Yes" : "No")
                    LabeledContent("Secrets", value: (args.secretEnv ?? []).isEmpty ? "None"
                        : (args.secretEnv ?? []).compactMap(\.name).joined(separator: ", "))
                }
            }
            let fields = Self.otherFields(of: record)
            if !fields.isEmpty {
                Section(record.op == "exec" ? "More" : "Details") {
                    ForEach(fields, id: \.key) { field in
                        LabeledContent(Self.label(field.key)) {
                            Text(Self.value(field.key, field.value))
                                .font(.body.monospaced())
                                .multilineTextAlignment(.trailing)
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            if let gate = record.gate {
                Why(gate: gate, caller: caller.who)
                if !gate.keptRequests.isEmpty {
                    Section {
                        Button("Gatekeeper request") {
                            judging?.cancel()
                            judging = Task { await openJudged(gate.keptRequests) }
                        }
                        if let judgeFailure { Text(judgeFailure).foregroundStyle(.secondary) }
                    }
                }
            }
            if let args = record.args, args.exitCode != nil || args.durationMs != nil {
                Section("Ran") {
                    if let exit = args.exitCode { LabeledContent("Exit code", value: "\(exit)") }
                    if let duration = args.durationMs { LabeledContent("Took", value: "\(duration) ms") }
                    if let bytes = args.stderrBytes { LabeledContent("Error output", value: "\(bytes) bytes") }
                    if args.timedOut == true { LabeledContent("Timed out", value: "Yes") }
                }
            }
            Section("Record") {
                Text(record.id).font(.caption.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        .formStyle(.grouped)
        .controlSize(.small)
        // Read when the record is opened, and again only for another.
        .task(id: record.id) {
            judging?.cancel()
            preview = nil
            judgeFailure = nil
            shots = record.shots.isEmpty ? nil : RecordShots(client: app.client, host: host, record: record)
            await shots?.load()
        }
        .quickLookPreview($preview)
        .onChange(of: preview) { closed, _ in closed.map(QuickLookFiles.shared.remove) }
        .onDisappear {
            judging?.cancel()
            preview.map(QuickLookFiles.shared.remove)
        }
    }

    /// A screenshot in Quick Look, from a file only this user can read
    /// (`QuickLookFiles`), removed once it closes.
    private func open(_ shot: RecordShots.Shot) {
        guard case let .loaded(png) = shot.state else { return }
        preview = QuickLookFiles.shared.write(png, name: "\(shot.title).png")
    }

    /// What the gatekeeper sent and got back for this record, in Quick Look.
    private func openJudged(_ ids: [String]) async {
        switch await Self.read(ids, client: app.client, host: host) {
        case let .success(text)?:
            judgeFailure = nil
            preview = QuickLookFiles.shared.write(text, name: "Gatekeeper request.json")
        case let .failure(failure)?:
            judgeFailure = failure.message
        case nil:
            break
        }
    }

    /// `judged`, or why not; nil once cancelled, as the record it was read
    /// for is no longer the one shown.
    static func read(_ ids: [String], client: any LatchClient, host: String?) async -> Result<String, LatchError>? {
        await AppModel.result { try await judged(ids, client: client, host: host) }
    }

    /// Each kept call, as latch prints it; an agent's three calls as one
    /// JSON list.
    static func judged(_ ids: [String], client: any LatchClient, host: String?) async throws -> String {
        var calls: [String] = []
        for id in ids {
            calls.append(try await client.auditJudge(id, host: host).trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return calls.count == 1 ? calls[0] : "[\n" + calls.joined(separator: ",\n") + "\n]"
    }


    /// Everything else the record holds, by key: each op's own args, and
    /// any detail beyond args, the judgment and the start time. Exec's
    /// fields shown above aren't repeated.
    static func otherFields(of record: AuditRecord) -> [(key: String, value: JSONValue)] {
        let detailFields = record.details?.fields ?? [:]
        var fields: [(key: String, value: JSONValue)] = []
        if case let .object(args)? = detailFields["args"] {
            // A browser's judgment is in its args; Why shows it.
            let shown: Set<String> = record.op == "exec"
                ? ["argv", "workdir", "writes", "network", "secret_env", "exit_code", "duration_ms", "stderr_bytes", "timed_out"] : ["gate"]
            fields += args.filter { !shown.contains($0.key) && $0.value != .null }.sorted { $0.key < $1.key }.map { ($0.key, $0.value) }
        }
        fields += detailFields.filter { !["args", "gate", "started"].contains($0.key) && $0.value != .null }
            .sorted { $0.key < $1.key }.map { ($0.key, $0.value) }
        return fields
    }

    static func label(_ key: String) -> String {
        switch key {
        case "sha256": return "SHA-256"
        case "url": return "URL"
        default:
            let words = key.replacingOccurrences(of: "_", with: " ")
            return words.prefix(1).uppercased() + words.dropFirst()
        }
    }

    static func value(_ key: String, _ value: JSONValue) -> String {
        if key.hasSuffix("size") || key.hasSuffix("bytes"), case let .number(bytes) = value {
            return Int64(bytes).formatted(.byteCount(style: .file))
        }
        if key.hasSuffix("_ms"), case let .number(ms) = value { return "\(Int(ms)) ms" }
        return value.text
    }
}

/// A browser plan's screenshots as they arrive; one opens in Quick Look.
private struct ShotsSection: View {
    var shots: RecordShots
    var hostName: String
    var open: (RecordShots.Shot) -> Void

    var body: some View {
        Section("Screenshots") {
            VStack(alignment: .leading, spacing: 6) {
                ScrollView(.horizontal) {
                    HStack(alignment: .top, spacing: 8) {
                        ForEach(shots.shots) { shot in
                            ShotTile(shot: shot, hostName: hostName) { open(shot) }
                        }
                    }
                    .padding(.vertical, 2)
                }
                if shots.shots.contains(where: { $0.state == .gone }) {
                    Text(RecordShots.gone).font(.caption).foregroundStyle(.secondary)
                }
                ForEach(shots.shots) { shot in
                    if case let .failed(error) = shot.state {
                        Text("\(shot.title): \(ErrorCopy.short(error, host: hostName))").font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        }
    }
}

/// One screenshot as a thumbnail, or why there is none.
private struct ShotTile: View {
    var shot: RecordShots.Shot
    var hostName: String
    var open: () -> Void

    var body: some View {
        let loaded = shot.thumbnail != nil
        VStack(alignment: .leading, spacing: 4) {
            Group {
                switch shot.state {
                case .loading:
                    ProgressView().controlSize(.small)
                case .loaded:
                    if let thumbnail = shot.thumbnail {
                        Button(action: open) {
                            Image(nsImage: thumbnail).resizable().scaledToFit()
                        }
                        .buttonStyle(.plain)
                        .help("Open in Quick Look")
                    } else {
                        Image(systemName: "photo").foregroundStyle(.secondary)
                    }
                case .gone:
                    Image(systemName: "clock.badge.xmark").foregroundStyle(.secondary)
                case .failed:
                    Image(systemName: "exclamationmark.triangle").foregroundStyle(.secondary)
                }
            }
            .frame(width: 128, height: 80)
            .background(.quaternary, in: .rect(cornerRadius: 4))
            .clipShape(.rect(cornerRadius: 4))
            Text(shot.title).font(.caption).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(shot.title)
        .accessibilityValue(state)
        .accessibilityAddTraits(loaded ? .isButton : .isImage)
        .accessibilityAction { if loaded { open() } }
    }

    private var state: String {
        switch shot.state {
        case .loading: "Loading"
        case .loaded: "Screenshot"
        case .gone: RecordShots.gone
        case let .failed(error): ErrorCopy.short(error, host: hostName)
        }
    }
}

/// The gatekeeper's judgment: what it read, which rules, how sure.
struct Why: View {
    var gate: AuditRecord.Gate
    var caller: String

    var body: some View {
        Section("Why") {
            if let source = gate.source {
                LabeledContent("Read", value: Self.sources[source] ?? source)
            }
            if let rules = gate.rules {
                LabeledContent("Rules used", value: [rules.host.map { _ in "For every agent" }, rules.agent.map { _ in "\(caller)'s own" }]
                    .compactMap { $0 }.joined(separator: " + ").nonEmpty ?? "None")
            }
            if let verdict = gate.verdict {
                LabeledContent("Verdict", value: Self.verdicts[verdict] ?? verdict)
            }
            if let p = gate.p {
                Likelihood(title: "Covered", value: p["COVERED"], tint: .green)
                Likelihood(title: "Not covered", value: p["NOT_COVERED"], tint: .orange)
                Likelihood(title: "Forbidden", value: p["FORBIDDEN"], tint: .red)
            }
            if gate.model != nil || gate.latencyMs != nil || gate.cost != nil || gate.rule != nil {
            DisclosureGroup("Details") {
                if let model = gate.model { LabeledContent("Model") { Text(model).font(.caption.monospaced()) } }
                if let latency = gate.latencyMs { LabeledContent("Took", value: "\(latency) ms") }
                if let cost = gate.cost { LabeledContent("Cost", value: cost.formatted(.currency(code: "USD").precision(.significantDigits(2)))) }
                if let rule = gate.rule { LabeledContent("Rule", value: rule) }
            }
            }
        }
    }

    static let sources = [
        "owner_request": "Your own request",
        "defaults": "Defaults only (no rules, no chat)",
        "rules": "Defaults and rules (no chat)",
        "chat_and_rules": "Defaults, rules and your chat with the agent",
        "chat": "Defaults and your chat with the agent",
    ]
    static let verdicts = [
        "allow": "Allowed",
        "not_covered": "Needed your OK",
        "owner_said_no": "You said no",
    ]
}

struct Likelihood: View {
    var title: String
    var value: Double?
    var tint: Color

    var body: some View {
        if let value {
            VStack(alignment: .leading, spacing: 3) {
                LabeledContent(title, value: value.formatted(.percent.precision(.fractionLength(0))))
                ProgressView(value: value).tint(tint).accessibilityHidden(true)
            }
        }
    }
}

private extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}

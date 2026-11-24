import AppKit
import QuickLook
import SwiftUI

/// One audit record in full: who asked, on which host, what it could
/// touch, why the gatekeeper allowed or refused it (and the rule edit that
/// would rule it the other way), and when each step happened.
struct RecordInspector: View {
    var record: AuditRecord
    var hostEntry: HostEntry
    @Environment(AppModel.self) private var app

    init(record: AuditRecord, host: HostEntry) {
        self.record = record
        hostEntry = host
    }

    private var hostName: String { hostEntry.name }
    /// nil for this Mac's own host, else the host `--host` names.
    private var host: String? { hostEntry.isThisMac ? nil : hostEntry.name }
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
                if let command = record.request {
                    Text(command)
                        .font(.body.monospaced())
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Section("Request") {
                let caller = app.caller(of: record, on: hostEntry)
                LabeledContent("Caller", value: "\(caller.who) · \(caller.kind)")
                LabeledContent("Host", value: hostName)
                LabeledContent("Operation", value: record.op)
                LabeledContent("Outcome") { Text(record.outcome).font(.body.monospaced()) }
                if record.gate != nil || record.isRefused {
                    LabeledContent("Decided by", value: record.gate == nil ? "\(hostName)'s settings" : "The gatekeeper")
                }
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
                Why(gate: gate, cited: app.keptCalls[record.id]?.cited)
                if record.outcome == "gatekeeper_denied" {
                    Section {
                        SuggestionBox(record: record, host: hostEntry, inRules: false)
                        if case .rule? = app.suggestion(for: record, on: hostEntry) {} else {
                            Button("Open \(hostName)'s Rules") { app.review(record, on: hostEntry) }
                                .help("Change what's always denied on \(hostName)")
                        }
                    }
                }
                if let kept = gate.keptRequest {
                    Section {
                        Button("Gatekeeper request") {
                            judging?.cancel()
                            judging = Task { await openJudged(kept) }
                        }
                        if let judgeFailure { Text(judgeFailure).foregroundStyle(.secondary) }
                    }
                }
            }
            Section("Timeline") {
                if let judged = record.gate?.gateRecord.flatMap({ id in app.feed(for: hostEntry).records.last { $0.id == id } }) {
                    LabeledContent("Judged", value: judged.time.formatted(date: .omitted, time: .standard))
                }
                if let started = record.details?.started {
                    LabeledContent("Started", value: started.formatted(date: .omitted, time: .standard))
                }
                LabeledContent(record.isRefused ? "Refused" : "Done", value: record.time.formatted(date: .abbreviated, time: .standard))
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
        // The message it cited, and for a refusal the edit that would let
        // it through, under the rule now.
        .task(id: "\(record.id)@\(app.savedRule(hostName)?.revision ?? "")") {
            guard !app.still else { return }
            await app.readKeptCall(record, on: hostEntry)
            if record.outcome == "gatekeeper_denied" { await app.suggest(for: record, on: hostEntry) }
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
    private func openJudged(_ id: String) async {
        switch await Self.read(id, client: app.client, host: host) {
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
    static func read(_ id: String, client: any LatchClient, host: String?) async -> Result<String, LatchError>? {
        await AppModel.result { try await client.auditJudge(id, host: host).trimmingCharacters(in: .whitespacesAndNewlines) }
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

/// The gatekeeper's judgment: the message of yours it was asked under,
/// the verdict, and how sure.
struct Why: View {
    var gate: AuditRecord.Gate
    /// The cited message's text, once its kept call is read.
    var cited: String?

    var body: some View {
        Section("Why") {
            if let cited {
                LabeledContent("Cited message") { Text(cited).textSelection(.enabled) }
            }
            if let later = gate.laterOwnerMessages, later > 0 {
                LabeledContent("Your later messages", value: "\(later)")
            }
            if let verdict = gate.verdict {
                LabeledContent("Verdict", value: AuditRecord.Gate.words(verdict))
            }
            if gate.deviceRule != nil { LabeledContent("Also judged by", value: "Your device rule") }
            if let p = gate.p {
                Likelihood(title: "Denied by your device rule", value: p["denied"] ?? nil, tint: .red)
            }
            if gate.model != nil || gate.latencyMs != nil || gate.cost != nil {
            DisclosureGroup("Details") {
                if let model = gate.model { LabeledContent("Model") { Text(model).font(.caption.monospaced()) } }
                if let latency = gate.latencyMs { LabeledContent("Took", value: "\(latency) ms") }
                if let cost = gate.cost { LabeledContent("Cost", value: cost.formatted(.currency(code: "USD").precision(.significantDigits(2)))) }
            }
            }
        }
    }
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

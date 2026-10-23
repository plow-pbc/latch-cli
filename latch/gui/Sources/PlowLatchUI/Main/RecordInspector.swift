import SwiftUI

/// One audit record in full: who asked, what it could touch, why the
/// gatekeeper allowed or refused it, and what ran.
struct RecordInspector: View {
    var record: AuditRecord
    var caller: (who: String, kind: String)
    var hostName: String

    var body: some View {
        Form {
            Section {
                StatusLabel(status: record.status)
                Text(record.command)
                    .font(.body.monospaced())
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Section("Request") {
                LabeledContent("Caller", value: "\(caller.who) · \(caller.kind)")
                LabeledContent("Time", value: record.time.formatted(date: .abbreviated, time: .standard))
                LabeledContent("Operation", value: record.op)
                LabeledContent("Outcome") { Text(record.outcome).font(.body.monospaced()) }
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
    }

    /// Everything else the record holds, by key: each op's own args, and
    /// any detail beyond args, the judgment and the start time. Exec's
    /// fields shown above aren't repeated.
    static func otherFields(of record: AuditRecord) -> [(key: String, value: JSONValue)] {
        let detailFields = record.details?.fields ?? [:]
        var fields: [(key: String, value: JSONValue)] = []
        if case let .object(args)? = detailFields["args"] {
            let shown: Set<String> = record.op == "exec"
                ? ["argv", "workdir", "writes", "network", "secret_env", "exit_code", "duration_ms", "stderr_bytes", "timed_out"] : []
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

/// The gatekeeper's judgment: what it read, which rules, how sure.
private struct Why: View {
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
        "rules": "Rules only (no chat)",
        "chat_and_rules": "Your chat with the agent + rules",
        "chat": "Your chat with the agent",
    ]
    static let verdicts = [
        "allow": "Allowed",
        "not_covered": "Not covered",
        "owner_said_no": "You said no",
        "no_intent": "No instructions from you",
    ]
}

private struct Likelihood: View {
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

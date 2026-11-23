import SwiftUI

/// Rules: a host's gatekeeper band with its device rule, then the last
/// request it allowed and the last it refused, each with why and a rule
/// edit that would rule the other way next time.
struct RulesScreen: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if let host = app.host {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    GatekeeperBand(host: host)
                    if host.isThisMac || host.online != false {
                        let reading = !app.feed(for: host).caughtUp
                        if let focus = app.rulesFocus, focus.host == host.name,
                           let record = app.feed(for: host).records.last(where: { $0.id == focus.id }) {
                            LastJudged(title: "From Activity", record: record, host: host, reading: false)
                        }
                        LastJudged(title: "Last allowed", record: app.lastJudged(on: host, refused: false), host: host, reading: reading)
                        LastJudged(title: "Last refused", record: app.lastJudged(on: host, refused: true), host: host, reading: reading)
                        Button("See all activity") { app.openActivity(host) }
                            .buttonStyle(.link)
                    }
                }
                .padding(20)
                .frame(maxWidth: 760, alignment: .leading)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .onDisappear { app.rulesFocus = nil }
        } else {
            ContentUnavailableView("No host", systemImage: "checkmark.shield",
                                   description: Text("Each host keeps its own rules. Pick one in the sidebar."))
        }
    }
}

/// The gatekeeper, and what it always denies on this host, edited in place.
private struct GatekeeperBand: View {
    var host: HostEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                Image(systemName: "checkmark.shield")
                    .foregroundStyle(.green)
                    .frame(width: 26, height: 26)
                    .background(.green.opacity(0.12), in: RoundedRectangle(cornerRadius: 7))
                Text("Gatekeeper").font(.headline)
                Text("Agents act only on a message of yours they cite.")
                    .foregroundStyle(.secondary)
            }
            // The label sits level with the box's first line: a text
            // editor has no baseline to align to.
            HStack(alignment: .top, spacing: 12) {
                Text("ALWAYS DENIED")
                    .font(.caption.monospaced())
                    .kerning(1)
                    .foregroundStyle(.secondary)
                    .fixedSize()
                    .frame(height: 30)
                DeviceRuleBox(host: host)
            }
        }
    }
}

/// The last request the gatekeeper allowed, or refused, on a host: what,
/// who, the message of yours it cited, when and why; then the rule edit
/// that would rule the other way.
private struct LastJudged: View {
    var title: String
    var record: AuditRecord?
    var host: HostEntry
    var reading: Bool
    @Environment(AppModel.self) private var app

    var body: some View {
        GroupBox {
            if let record {
                details(record)
                    .task(id: "\(record.id)@\(app.savedRule(host.name)?.revision ?? "")") {
                        guard !app.still else { return }
                        await app.suggest(for: record, on: host)
                    }
            } else {
                Text(reading ? "Reading…" : "None yet.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        } label: {
            Text(title).font(.headline)
        }
    }

    private func details(_ record: AuditRecord) -> some View {
        let caller = app.caller(of: record, on: host)
        let cited = app.keptCalls[record.id]?.cited
        return VStack(alignment: .leading, spacing: 6) {
            HStack {
                StatusLabel(status: record.status)
                Spacer()
                Text(record.time.formatted(date: .abbreviated, time: .standard)).foregroundStyle(.secondary)
            }
            Text(record.request ?? record.op)
                .font(.body.monospaced())
                .textSelection(.enabled)
            Text(record.agent == nil ? "\(caller.who) · \(caller.kind)" : cited.map { "\(caller.who) · cited \u{201C}\($0)\u{201D}" } ?? caller.who)
                .foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline) {
                Text("Why").foregroundStyle(.secondary)
                Text(why(record)).textSelection(.enabled)
            }
            SuggestionBox(record: record, host: host, inRules: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func why(_ record: AuditRecord) -> String {
        let verdict = record.gate?.verdict ?? record.outcome
        if verdict == "allow" {
            return record.agent == nil ? "Your device rule doesn't deny it." : "Your message asks for it, and your device rule doesn't deny it."
        }
        if verdict == "denied_by_rule", let rule = record.gate?.deviceRule.flatMap({ app.ruleText($0, on: host) }) {
            return "Your device rule denies it: \u{201C}\(rule)\u{201D}"
        }
        return AuditRecord.Gate.words(verdict)
    }
}

/// "Want the opposite next time?": the rule edit the judge says would
/// rule the other way, applied in one click on Rules (elsewhere, a click
/// opens it there, ready to apply); for a request your message didn't ask
/// for, the message is what to change.
struct SuggestionBox: View {
    var record: AuditRecord
    var host: HostEntry
    var inRules: Bool
    @Environment(AppModel.self) private var app

    var body: some View {
        switch app.suggestion(for: record, on: host) {
        case nil:
            Label("Checking a rule that would rule the other way…", systemImage: "sparkles")
                .font(.callout)
                .foregroundStyle(.secondary)
        case let .rule(text)?:
            VStack(alignment: .leading, spacing: 6) {
                Text("Want the opposite next time?").font(.callout.weight(.semibold))
                Text(text.isEmpty ? "Deny nothing: clear the rule." : text)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                HStack {
                    Text("Checked with the gatekeeper").font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    if inRules {
                        Button("Apply") { Task { await app.applyRule(text, on: host.name) } }
                            .help("Put this in the rule box and save it")
                    } else {
                        Button("Open in Rules") { app.review(record, on: host) }
                            .help("Open \(host.name)'s Rules with this ready to apply")
                    }
                }
            }
            .padding(10)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
        case .askInMessage?:
            Text("Want the opposite next time? Ask for it in your message: a device rule only denies.")
                .font(.callout)
                .foregroundStyle(.secondary)
        case .nothing?:
            EmptyView()
        }
    }
}

/// What a gate record's kept call (`latch audit judge`) says: the text of
/// the message it cited, and the action as the judge was shown it.
nonisolated struct KeptCall: Equatable {
    var cited: String?
    var action: String?

    init(_ json: String) {
        let call = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
        let state = (call?["request"] as? [String: Any])?["state"] as? [String: Any]
        cited = (state?["cited_message"] as? [String: Any])?["text"] as? String
        action = state?["proposed_action"].flatMap { try? JSONSerialization.data(withJSONObject: $0, options: .sortedKeys) }
            .map { String(decoding: $0, as: UTF8.self) }
    }
}

/// A rule edit that would rule a request the other way, checked with the
/// judge; for a request your message didn't ask for, your message; or
/// none.
enum RuleSuggestion: Equatable {
    case rule(String), askInMessage, nothing
}

extension AppModel {
    /// The newest of a host's loaded requests the judge allowed, or the
    /// newest the gatekeeper refused.
    func lastJudged(on host: HostEntry, refused: Bool) -> AuditRecord? {
        feed(for: host).records.last { record in
            refused ? record.outcome == "gatekeeper_denied"
                : record.op != "gate" && record.decision == "allow" && record.gate?.verdict == "allow" && record.gate?.model != nil
        }
    }

    /// A device rule as a judgment names it, by its revision's first
    /// characters: the rule now, if it's at that revision, else the text
    /// the newest save of that revision kept, else nil.
    func ruleText(_ revision: String, on host: HostEntry) -> String? {
        if let rule = savedRule(host.name), rule.revision.hasPrefix(revision) { return rule.text }
        return feed(for: host).records.last { $0.op == "rules" && $0.args?.after?.hasPrefix(revision) == true && $0.args?.text != nil }?.args?.text
    }

    /// A gate record's kept call, read once; nil while it can't be read
    /// (the host keeps them 7 days).
    @discardableResult
    func readKeptCall(_ record: AuditRecord, on host: HostEntry) async -> KeptCall? {
        if let kept = keptCalls[record.id] { return kept }
        guard let id = record.gate?.keptRequest,
              case let .success(json)? = await Self.result({ try await self.client.auditJudge(id, host: host.isThisMac ? nil : host.name) })
        else { return nil }
        keptCalls[record.id] = KeptCall(json)
        return keptCalls[record.id]
    }

    /// The suggestion for a record under the host's rule now; nil until
    /// it's been checked.
    func suggestion(for record: AuditRecord, on host: HostEntry) -> RuleSuggestion? {
        suggestions["\(record.id)@\(savedRule(host.name)?.revision ?? "")"]
    }

    /// Checks, once for each rule, which edit of the host's rule would rule
    /// `record` the other way, judged on this Mac (`rules try`): offered
    /// only while the rule now still rules as it did, and only an edit
    /// that flips it. A device rule only denies, so a request your message
    /// didn't cover can't be let through by one.
    func suggest(for record: AuditRecord, on host: HostEntry) async {
        if !Self.held(summaries[host.name]?.rules) { await refreshRules(on: host) }
        let rule = savedRule(host.name)
        let key = "\(record.id)@\(rule?.revision ?? "")"
        guard suggestions[key] == nil else { return }
        let found = await suggestion(for: record, on: host, rule: rule?.text ?? "")
        guard !Task.isCancelled else { return }
        suggestions[key] = found
    }

    private func suggestion(for record: AuditRecord, on host: HostEntry, rule: String) async -> RuleSuggestion {
        let refused = record.isRefused
        if record.gate?.verdict == "not_covered" { return .askInMessage }
        guard !refused || record.gate?.verdict == "denied_by_rule", let summary = record.actionSummary,
              let action = await readKeptCall(record, on: host)?.action else { return .nothing }
        do {
            guard try await client.rulesTry(draft: rule, action: action) == refused else { return .nothing }
            for candidate in Self.candidates(rule: rule, summary: summary, refused: refused)
            where try await client.rulesTry(draft: candidate, action: action) != refused {
                return .rule(candidate)
            }
        } catch {}
        return .nothing
    }

    /// The edits to try, in order: for an allowed request, the rule and
    /// "Never …"; for a refused one, the rule without each of its first
    /// three sentences, then the rule and "You may …", then no rule when
    /// it's one sentence.
    nonisolated static func candidates(rule: String, summary: String, refused: Bool) -> [String] {
        let rule = rule.trimmingCharacters(in: .whitespacesAndNewlines)
        let gap = rule.contains("\n") ? "\n" : " "
        func adding(_ sentence: String) -> String { rule.isEmpty ? sentence : rule + gap + sentence }
        guard refused else { return [adding("Never \(summary).")] }
        var sentences: [String] = []
        rule.enumerateSubstrings(in: rule.startIndex..., options: .bySentences) { sentence, _, _, _ in
            if let sentence { sentences.append(sentence.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
        let narrowed = sentences.count < 2 ? [] : sentences.indices.prefix(3).map { cut in
            sentences.enumerated().filter { $0.offset != cut }.map(\.element).joined(separator: gap)
        }
        return narrowed + [adding("You may \(summary).")] + (sentences.count == 1 ? [""] : [])
    }

    /// A suggested rule, into the host's box and saved as typing would be.
    func applyRule(_ text: String, on host: String) async {
        typeRule(text, on: host)
        await saveRuleDraft(host)
    }

    /// When the window opens: the newest request the gatekeeper refused on
    /// any host in the last 15 minutes, from each host's newest records.
    func noticeDenials(now: Date = .now) async {
        var newest: (time: Date, denial: Denial)?
        for host in hosts where host.isThisMac || host.online != false {
            guard case let .success(page)? = await Self.result({ try await self.client.audit(host: host.isThisMac ? nil : host.name, before: nil) }),
                  let record = page.records.last(where: { $0.outcome == "gatekeeper_denied" }),
                  record.time > now.addingTimeInterval(-15 * 60), record.time > newest?.time ?? .distantPast else { continue }
            newest = (record.time, Denial(host: host.name, request: record.command ?? record.op))
        }
        denial = newest?.denial
    }

    /// A request on its host's Rules, with the edit that would rule it the
    /// other way ready to apply.
    func review(_ record: AuditRecord, on host: HostEntry) {
        chosenHost = host.name
        rulesFocus = OnHost(host: host.name, id: record.id)
        place = .host(.rules)
    }

    /// The banner's click: that host's Rules.
    func reviewDenial() {
        guard let denial else { return }
        chosenHost = denial.host
        place = .host(.rules)
        self.denial = nil
    }
}

/// A recent refusal the window opened on, for its banner.
struct Denial: Equatable {
    var host: String
    var request: String
}

/// Over every screen while there's a recent refusal: what was refused,
/// where, and the way to its host's Rules.
struct DenialBanner: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if let denial = app.denial {
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.shield.fill").foregroundStyle(.red).imageScale(.large)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Gatekeeper denied a request").font(.headline)
                    Text("\(denial.request) · on \(denial.host)")
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                Button("Review in Rules") { app.reviewDenial() }
                Button { app.denial = nil } label: { Image(systemName: "xmark") }
                    .buttonStyle(.borderless)
                    .help("Dismiss")
                    .accessibilityLabel("Dismiss Gatekeeper denial")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity)
            .background(.red.opacity(0.08))
            .contentShape(Rectangle())
            .onTapGesture { app.reviewDenial() }
        }
    }
}

nonisolated extension AuditRecord {
    /// The request in a few words, for a rule to name.
    var actionSummary: String? {
        request.map { op == "exec" ? "run `\($0)`" : "use `\($0)`" }
    }
}

import SwiftUI

/// Rules: a host's gatekeeper band with its device rule.
struct RulesScreen: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if let host = app.host {
            ScrollView {
                GatekeeperBand(host: host)
                    .padding(20)
                    .frame(maxWidth: 760, alignment: .leading)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
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

/// What a gate record's kept call (`latch audit judge`) says: the text of
/// the message it cited.
nonisolated struct KeptCall: Equatable {
    var cited: String?

    init(_ json: String) {
        let call = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any]
        let state = (call?["request"] as? [String: Any])?["state"] as? [String: Any]
        cited = (state?["cited_message"] as? [String: Any])?["text"] as? String
    }
}

extension AppModel {
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

    /// A host's Rules, where its device rule is changed.
    func showRules(on host: HostEntry) {
        chosenHost = host.name
        place = .host(.rules)
    }
}

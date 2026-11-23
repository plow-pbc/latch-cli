import SwiftUI

/// A host's device rule: what its agents are always denied, whatever a
/// message of yours says, in your words. It grants nothing; blank denies
/// nothing. Edited in place and saved as typing pauses or the box is
/// left, only over what was there when editing began.
struct DeviceRuleBox: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app
    @FocusState private var focused: Bool

    var body: some View {
        switch app.summaries[host.name]?.rules {
        case nil:
            ProgressView().controlSize(.small)
        case let .failure(error)?:
            Label { ErrorText(error: error, host: host.spoken) } icon: { Image(systemName: "exclamationmark.triangle") }
                .foregroundStyle(.secondary)
        case let .success(show)? where show.older:
            Label("\(host.isThisMac ? "This Mac" : host.name) runs an older latch. Update Latch there to choose what agents are always denied.",
                  systemImage: "arrow.down.circle")
                .foregroundStyle(.secondary)
        case .success?:
            editor
        }
    }

    private var editor: some View {
        let name = host.name
        let draft = app.rulesDrafts[name]
        return VStack(alignment: .trailing, spacing: 6) {
            TextEditor(text: Binding { draft?.text ?? app.savedRule(name)?.text ?? "" } set: { app.typeRule($0, on: name) })
                .font(.body)
                .frame(minHeight: 18, maxHeight: 120)
                .fixedSize(horizontal: false, vertical: true)
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.separator))
                .overlay(alignment: .topLeading) {
                    if (draft?.text ?? app.savedRule(name)?.text ?? "").isEmpty {
                        Text("Nothing is denied. For example: Never touch my tax returns.")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 13)
                            .padding(.vertical, 6)
                            .allowsHitTesting(false)
                    }
                }
                .focused($focused)
                .onChange(of: focused) { if !focused { Task { await app.saveRuleDraft(name) } } }
                .accessibilityLabel("Always denied on \(host.name)")
            if let error = app.rulesErrors[name] {
                Label { ErrorText(error: error, host: host.spoken) } icon: { Image(systemName: "exclamationmark.triangle") }
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if app.rulesSaved.contains(name) {
                Label("Saved", systemImage: "checkmark")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

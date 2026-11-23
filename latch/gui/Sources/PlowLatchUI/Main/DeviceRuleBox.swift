import SwiftUI

/// A host's device rule: what its agents are always denied, whatever a
/// message of yours says, in your words. It grants nothing; blank denies
/// nothing. Edited in place and saved only over what was there when
/// editing began. `answering` is the gatekeeper question's: Save is always
/// offered, so a blank answer is saved too, and Not Now takes Revert's place.
struct DeviceRuleBox: View {
    var host: HostEntry
    var answering = false
    @Environment(AppModel.self) private var app

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
        let saving = app.savingRules.contains(name)
        return VStack(alignment: .trailing, spacing: 6) {
            TextEditor(text: Binding { draft?.text ?? app.savedRule(name)?.text ?? "" } set: { app.typeRule($0, on: name) })
                .font(.body)
                .frame(minHeight: answering ? 76 : 22, maxHeight: 120)
                .fixedSize(horizontal: false, vertical: !answering)
                .scrollContentBackground(.hidden)
                .padding(.horizontal, 6)
                .padding(.vertical, 5)
                .background(.background, in: RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(.separator))
                .overlay(alignment: .topLeading) {
                    if (draft?.text ?? app.savedRule(name)?.text ?? "").isEmpty {
                        Text("Nothing is denied. For example: Never touch my tax returns.")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 11)
                            .padding(.vertical, 5)
                            .allowsHitTesting(false)
                    }
                }
                // What's typed during a save couldn't be saved by it.
                .disabled(saving)
                .accessibilityLabel("Always denied on \(host.name)")
            if let error = app.rulesErrors[name] {
                Label { ErrorText(error: error, host: host.spoken) } icon: { Image(systemName: "exclamationmark.triangle") }
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if answering || draft != nil {
                HStack {
                    // The question is never a trap: Not Now always closes it.
                    if answering {
                        Button("Not Now") { app.putOffGatekeeper(host) }.keyboardShortcut(.cancelAction)
                    } else {
                        Button("Revert") { Task { await app.revertRule(name) } }.disabled(saving)
                    }
                    Button(saving ? "Saving…" : "Save") { Task { await app.saveRuleDraft(name, answer: answering) } }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(answering ? .defaultAction : nil)
                        .disabled(saving || !(answering || app.ruleChanged(name)))
                }
            }
        }
    }
}

/// Setup's gatekeeper question over the main window, for an owner of this
/// Mac who never answered it.
struct GatekeeperSheet: View {
    var host: HostEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(GatekeeperCopy.title).font(.title2.bold())
            Text(GatekeeperCopy.lead)
            Text(GatekeeperCopy.question).font(.headline)
            Text(GatekeeperCopy.blank)
                .font(.callout)
                .foregroundStyle(.secondary)
            DeviceRuleBox(host: host, answering: true)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(24)
        .frame(width: 480)
    }
}

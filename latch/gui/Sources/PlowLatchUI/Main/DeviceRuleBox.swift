import SwiftUI

/// A host's device rule: what its agents are always denied, whatever a
/// message of yours says, in your words. It grants nothing; blank denies
/// nothing. Edited in place and saved only over what was there when
/// editing began. `answering` is the gatekeeper question's: Save is always
/// offered, so a blank answer is saved too, and there is no Revert.
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
        case .success?:
            editor
        }
    }

    private var editor: some View {
        let name = host.name
        let draft = app.rulesDrafts[name]
        let saving = app.savingRules.contains(name)
        return VStack(alignment: .leading, spacing: 6) {
            TextEditor(text: Binding { draft?.text ?? app.savedRule(name)?.text ?? "" } set: { app.typeRule($0, on: name) })
                .font(.body)
                .frame(minHeight: answering ? 76 : 44, maxHeight: 120)
                .scrollContentBackground(.hidden)
                .padding(4)
                .background(.background, in: RoundedRectangle(cornerRadius: 6))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.separator))
                .overlay(alignment: .topLeading) {
                    if (draft?.text ?? app.savedRule(name)?.text ?? "").isEmpty {
                        Text("Nothing is denied. For example: Never touch my tax returns.")
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 9)
                            .padding(.vertical, 4)
                            .allowsHitTesting(false)
                    }
                }
                // What's typed during a save couldn't be saved by it.
                .disabled(saving)
                .accessibilityLabel("Always denied on \(host.name)")
            if let error = app.rulesErrors[name] {
                Label { ErrorText(error: error, host: host.spoken) } icon: { Image(systemName: "exclamationmark.triangle") }
                    .foregroundStyle(.red)
            }
            if answering || draft != nil {
                HStack {
                    Spacer()
                    Button("Revert") { Task { await app.revertRule(name) } }.disabled(saving)
                    Button(saving ? "Saving…" : "Save") { Task { await app.saveRuleDraft(name, answer: answering) } }
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(answering ? .defaultAction : nil)
                        .disabled(saving || !(answering || app.ruleChanged(name)))
                }
            }
        }
    }
}

/// The gatekeeper question, once on this Mac: what its agents must never
/// do. Asked of everyone who never answered it, on updating from a latch
/// whose rules granted; answered, even blank, it's never asked again.
struct GatekeeperSheet: View {
    var host: HostEntry
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Meet the Gatekeeper").font(.title2.bold())
            Text("Your agents now act only on a message of yours they cite. The gatekeeper reads it on this Mac and lets through only what you asked for.")
            Text("Name anything agents must never do on \(host.name), whatever a message says, or leave it blank to deny nothing.")
                .foregroundStyle(.secondary)
            DeviceRuleBox(host: host, answering: true)
            Text("Rules from earlier versions no longer apply. They were set aside, not deleted. Change this any time at the top of Activity.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        .fixedSize(horizontal: false, vertical: true)
        .padding(24)
        .frame(width: 480)
    }
}

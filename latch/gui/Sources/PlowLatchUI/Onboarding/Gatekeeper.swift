import SwiftUI

/// The gatekeeper question: what agents must never do on this Mac, whatever
/// a message says. Blank denies nothing; an answer, blank included, is
/// saved, and never asked again.
struct RulesView: View {
    @Bindable var flow: Onboarding

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            ScreenHeader(title: "Meet the Gatekeeper.",
                         lead: "Your agents act only on a message of yours they cite. The gatekeeper reads that message on this Mac and lets through only what you asked for. An agent quoting you can't give permission.")
            VStack(alignment: .leading, spacing: 10) {
                Text("What should agents never do on this Mac, whatever a message says?")
                    .font(.headline)
                field
                    .disabled(flow.rulesUnreadable)
                Text("Leave it blank to deny nothing. Change it any time at the top of Activity.")
                    .foregroundStyle(Brand.muted)
            }
            if let error = flow.rulesError {
                ErrorText(error: error, host: ErrorCopy.thisMac).foregroundStyle(Brand.clay).font(.callout)
            }
            if flow.rulesUnreadable {
                HStack(spacing: 10) {
                    Text("Your current answer couldn't be read, so none can be saved over it.").foregroundStyle(Brand.muted)
                    Button("Try Again") { Task { await flow.loadRules() } }
                }
                .font(.callout)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 48)
        .padding(.top, 48)
        .task { if !flow.app.still { await flow.loadRules() } }
    }

    private var field: some View {
        TextEditor(text: $flow.draftRules)
            .font(.system(size: 13.5))
            .lineSpacing(3)
            .scrollContentBackground(.hidden)
            .padding(8)
            .frame(height: 110)
            .writingToolsBehavior(.disabled)
            .background(Brand.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).strokeBorder(Brand.fieldBorder))
            .overlay(alignment: .topLeading) {
                if flow.draftRules.isEmpty {
                    Text("For example: Never touch my tax returns or anything in ~/Finance.")
                        .font(.system(size: 13.5))
                        .foregroundStyle(Brand.muted.opacity(0.7))
                        .padding(.horizontal, 13)
                        .padding(.vertical, 8)
                        .allowsHitTesting(false)
                }
            }
            .accessibilityLabel("What agents must never do on this Mac")
    }
}

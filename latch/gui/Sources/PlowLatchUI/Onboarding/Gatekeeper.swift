import SwiftUI

/// The gatekeeper question's words, the same in setup and wherever else
/// it's asked.
enum GatekeeperCopy {
    static let title = "Meet the Plow Gatekeeper"
    static let lead = "Your agents act only on a message of yours they cite. The Plow Gatekeeper reads it on your Mac and lets through only what you asked for, so your agents get useful work done and nothing more."
    static let question = "What should Plow Latch never allow on your Mac?"
    static let blank = "Leave it blank to deny nothing. Change it any time at the top of Activity."
}

/// Setup's gatekeeper question: what agents must never do on this Mac,
/// whatever a message says. Blank denies nothing; an answer, blank
/// included, is saved, and never asked again.
struct RulesView: View {
    @Bindable var flow: Onboarding

    var body: some View {
        VStack(spacing: 18) {
            Text(GatekeeperCopy.title)
                .font(.system(size: 34, weight: .semibold))
            Text(GatekeeperCopy.lead)
                .foregroundStyle(Brand.muted)
                .frame(maxWidth: 440)
            Text(GatekeeperCopy.question)
                .font(.headline)
                .padding(.top, 8)
            field
                .disabled(flow.rulesUnreadable)
            Text(GatekeeperCopy.blank)
                .font(.callout)
                .foregroundStyle(Brand.muted)
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
        .multilineTextAlignment(.center)
        .padding(.horizontal, 48)
        .padding(.top, 56)
        .task { if !flow.app.still { await flow.loadRules() } }
    }

    private var field: some View {
        TextEditor(text: $flow.draftRules)
            .font(.system(size: 13.5))
            .lineSpacing(3)
            .multilineTextAlignment(.leading)
            .scrollContentBackground(.hidden)
            .padding(8)
            .frame(height: 76)
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
            .accessibilityLabel(GatekeeperCopy.question)
    }
}

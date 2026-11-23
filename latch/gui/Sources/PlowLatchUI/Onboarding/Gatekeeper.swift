import SwiftUI

/// Setup's gatekeeper question: what agents must never do on this Mac,
/// whatever a message says. Blank denies nothing; an answer, blank
/// included, is saved, and never asked again.
struct RulesView: View {
    @Bindable var flow: Onboarding

    var body: some View {
        VStack(spacing: 18) {
            Text("Meet the Plow Gatekeeper")
                .font(.system(size: 34, weight: .semibold))
            Text("Your agents act only on a message of yours they cite. The Plow Gatekeeper reads it on your Mac and lets through only what you asked for, so your agents get useful work done and nothing more.")
                .foregroundStyle(Brand.muted)
                .frame(maxWidth: 440)
            Text("What should Plow Latch never allow on your Mac?")
                .font(.headline)
                .padding(.top, 8)
            field
                .disabled(flow.rulesUnreadable)
            Text("Leave it blank to deny nothing.")
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
            GatekeeperArt().padding(.top, 10)
            Spacer(minLength: 0)
        }
        .multilineTextAlignment(.center)
        .padding(.horizontal, 48)
        .padding(.top, 40)
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
            .accessibilityLabel("What should Plow Latch never allow on your Mac?")
    }
}

/// What the gatekeeper does, drawn: requests crossing its line on the way
/// to the Mac, the asked-for let through and the rest refused. Art, not a
/// verdict: nothing here is judged.
private struct GatekeeperArt: View {
    private static let rows: [(symbol: String, text: String, allowed: Bool)] = [
        ("calendar", "Check the family calendar", true),
        ("message", "Text Mary “Running late”", true),
        ("key", "Sign in to Instacart with your password", true),
        ("square.and.arrow.up", "Post your tax return publicly", false),
        ("lock", "Copy all your saved passwords", false),
    ]

    var body: some View {
        HStack(spacing: 18) {
            VStack(spacing: 8) {
                ForEach(Self.rows, id: \.text) { row in
                    let tint = row.allowed ? Brand.volt : Brand.clay
                    HStack(spacing: 10) {
                        Image(systemName: row.symbol).frame(width: 16)
                        Text(row.text).foregroundStyle(row.allowed ? Brand.text : Brand.muted)
                        Spacer(minLength: 0)
                        Image(systemName: row.allowed ? "checkmark" : "xmark").fontWeight(.semibold)
                    }
                    .font(.system(size: 12.5))
                    .foregroundStyle(tint)
                    .padding(.horizontal, 12)
                    .frame(height: 30)
                    .background(tint.opacity(0.07), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(tint.opacity(0.45)))
                }
            }
            .frame(width: 340)
            // The gate: a glowing line the requests cross before their verdict.
            .overlay(alignment: .trailing) {
                LinearGradient(colors: [Brand.volt.opacity(0), Brand.volt, Brand.volt.opacity(0)], startPoint: .top, endPoint: .bottom)
                    .frame(width: 2)
                    .padding(.vertical, -16)
                    .shadow(color: Brand.volt, radius: 6)
                    .padding(.trailing, 34)
            }
            Image(systemName: "macmini")
                .font(.system(size: 56, weight: .ultraLight))
                .foregroundStyle(Brand.muted)
        }
        .multilineTextAlignment(.leading)
        .accessibilityHidden(true)
    }
}

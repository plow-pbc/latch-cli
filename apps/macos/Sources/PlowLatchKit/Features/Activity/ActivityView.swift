import SwiftUI

/// Recent verified activity, newest first, following live while on screen.
struct ActivityView: View {
    var showOverview: () -> Void
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme
    /// Changing it restarts following.
    @State private var attempt = 0

    var body: some View {
        content
            .navigationTitle("Activity")
            .task(id: attempt) { await model.followActivity() }
    }

    @ViewBuilder private var content: some View {
        let entries = model.activity.entries
        if model.menuState == .loggedOut {
            Unavailable(kind: .notLoggedIn) { WindowRouter.shared.showOnboarding() }
        } else if entries.isEmpty, let error = model.activityError {
            Unavailable(kind: .unreachable(error.message)) { attempt += 1 }
        } else if entries.isEmpty, model.activity.cursor == nil {
            ProgressView()
        } else if entries.isEmpty {
            Unavailable(kind: .noActivity, action: showOverview)
        } else {
            List(entries) { ActivityRow(entry: $0) }
                .latchAnimation(theme.motion.appear, value: entries.first?.id)
        }
    }
}

private struct ActivityRow: View {
    var entry: ActivityEntry
    @Environment(\.theme) private var theme

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: theme.space.medium) {
            Image(systemName: allowed ? "checkmark.circle.fill" : "xmark.octagon.fill")
                .foregroundStyle(allowed ? theme.colors.serving : theme.colors.error)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: theme.space.small) {
                    Text(entry.op).font(theme.type.mono)
                    Text("from \(entry.callerName ?? String(entry.caller.prefix(12)))")
                        .foregroundStyle(theme.colors.secondaryText)
                }
                if entry.outcome != "ok" {
                    Text(entry.outcome)
                        .font(theme.type.detail)
                        .foregroundStyle(theme.colors.secondaryText)
                }
            }
            Spacer()
            Text(entry.time, format: .dateTime.hour().minute().second())
                .font(theme.type.detail)
                .foregroundStyle(theme.colors.secondaryText)
        }
        .padding(.vertical, 2)
    }

    private var allowed: Bool { entry.decision == "allow" }
}

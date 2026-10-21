import SwiftUI

/// This Mac's host: who it is, whether it's sharing, what it shares.
struct OverviewView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme

    var body: some View {
        Group {
            switch model.status {
            case .loading:
                ProgressView()
            case let .failed(error, nil):
                Unavailable(kind: .unreachable(error.message)) { refresh() }
            default:
                if let status = model.status.value {
                    if status.peer == nil {
                        Unavailable(kind: .notLoggedIn) { WindowRouter.shared.showOnboarding() }
                    } else {
                        page(status)
                    }
                }
            }
        }
        .navigationTitle("Overview")
    }

    private func page(_ status: LocalStatus) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: theme.space.large) {
                header(status)
                if case let .failed(error, _) = model.status {
                    Label("Showing the last known state: \(error.message)", systemImage: "clock.badge.exclamationmark")
                        .font(theme.type.detail)
                        .foregroundStyle(theme.colors.secondaryText)
                }
                sharing
                if model.menuState == .stopped {
                    Unavailable(kind: .sharingStopped) { Task { await model.startSharing() } }
                        .disabled(!model.canStartSharing)
                        .latchSurface(.card)
                }
                if case let .error(message) = model.menuState {
                    problem(message)
                } else if let error = status.lastError {
                    problem(error.message)
                }
                facts(status)
            }
            .padding(theme.space.page)
            .frame(maxWidth: 720, alignment: .leading)
        }
    }

    private func header(_ status: LocalStatus) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: theme.space.medium) {
            Text(status.peer?.name ?? "This Mac").font(theme.type.hero)
            StatusBadge(model.menuState)
            Spacer()
            if model.canStopSharing {
                Button("Stop Sharing") { Task { await model.stopSharing() } }
            }
        }
    }

    /// What's special about how this Mac shares right now, if anything.
    @ViewBuilder private var sharing: some View {
        switch model.menuState {
        case .outside:
            note("A latch serve started outside Plow Latch is sharing this Mac. Stop it where it was started to manage sharing here.",
                 symbol: "terminal")
        case .needsApproval:
            HStack {
                note("Sharing in the background is turned off in Login Items.", symbol: "exclamationmark.circle")
                Button("Open Login Items…") { BackgroundSharing.openLoginItems() }
            }
        default:
            EmptyView()
        }
        if let error = model.sharingError {
            problem(error)
        }
    }

    private func note(_ message: String, symbol: String) -> some View {
        Label(message, systemImage: symbol)
            .font(theme.type.detail)
            .foregroundStyle(theme.colors.secondaryText)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func problem(_ message: String) -> some View {
        Label(message, systemImage: "exclamationmark.triangle.fill")
            .foregroundStyle(theme.colors.error)
            .padding(theme.space.medium)
            .frame(maxWidth: .infinity, alignment: .leading)
            .latchSurface(.card)
    }

    private func facts(_ status: LocalStatus) -> some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: theme.space.large, verticalSpacing: theme.space.medium) {
            row("Login") {
                Text("\(status.peer?.kind.capitalized ?? "") · \(status.peer?.key.prefix(12) ?? "")…")
                    .font(theme.type.mono)
            }
            row("Shared folders") {
                if status.shares.isEmpty {
                    Text("None chosen").foregroundStyle(theme.colors.secondaryText)
                } else {
                    VStack(alignment: .leading, spacing: theme.space.small) {
                        ForEach(status.shares, id: \.self) { path in
                            Label(path, systemImage: "folder").font(theme.type.mono)
                        }
                    }
                }
            }
            row("latch") {
                Text("\(status.version) · \(status.stateDir)")
                    .font(theme.type.mono)
                    .foregroundStyle(theme.colors.secondaryText)
            }
            if case let .loaded(_, at) = model.status {
                row("Updated") {
                    Text(at, format: .dateTime.hour().minute()).foregroundStyle(theme.colors.secondaryText)
                }
            }
        }
        .textSelection(.enabled)
        .padding(theme.space.large)
        .frame(maxWidth: .infinity, alignment: .leading)
        .latchSurface(.card)
    }

    private func row(_ title: String, @ViewBuilder value: () -> some View) -> some View {
        GridRow {
            Text(title).font(theme.type.heading).gridColumnAlignment(.trailing)
            value()
        }
    }

    private func refresh() {
        Task { await model.refreshStatus() }
    }
}

import SwiftUI

/// The host's state as a small coloured dot and label.
public struct StatusBadge: View {
    var state: MenuState
    @Environment(\.theme) private var theme

    public init(_ state: MenuState) {
        self.state = state
    }

    public var body: some View {
        Label {
            Text(state.title)
        } icon: {
            Circle().fill(theme.color(for: state)).frame(width: 8, height: 8)
        }
        .font(theme.type.detail)
        .padding(.horizontal, theme.space.small)
        .padding(.vertical, 3)
        .background(theme.color(for: state).opacity(0.12), in: Capsule())
        .latchAnimation(theme.motion.stateChange, value: state)
    }
}

extension MenuState {
    public var title: String {
        switch self {
        case .serving: "Sharing"
        case .starting: "Starting…"
        case .stopped: "Sharing stopped"
        case .loggedOut: "Not logged in"
        case .unreachable: "Can't reach latch"
        case .error: "Sharing failed"
        }
    }

    /// The template SF Symbol the menu bar shows for this state.
    public var symbol: String {
        switch self {
        case .serving: "link.circle.fill"
        case .starting, .stopped, .loggedOut: "link.circle"
        case .error, .unreachable: "exclamationmark.triangle"
        }
    }
}

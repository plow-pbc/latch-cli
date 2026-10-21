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

/// Plow's primary button: an opaque volt capsule with dark text.
public struct VoltButtonStyle: ButtonStyle {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled

    public init() {}

    public func makeBody(configuration: Configuration) -> some View {
        let brand = theme.brand
        configuration.label
            .font(brand.button)
            .foregroundStyle(brand.content)
            .padding(.horizontal, 18)
            .frame(height: 32)
            .background(brand.volt.opacity(isEnabled ? 1 : 0.35), in: Capsule())
            .brightness(configuration.isPressed ? -0.06 : 0)
            .offset(y: configuration.isPressed ? 0.5 : 0)
            .shadow(color: brand.volt.opacity(isEnabled ? 0.25 : 0), radius: 10)
    }
}

/// A thin spinning ring, for waiting.
public struct Spinner: View {
    var color: Color
    var size: CGFloat
    @ReducedMotion private var reduceMotion
    @State private var turning = false

    public init(color: Color, size: CGFloat = 10) {
        self.color = color
        self.size = size
    }

    public var body: some View {
        Circle()
            .trim(from: 0, to: 0.75)
            .stroke(color, style: StrokeStyle(lineWidth: size / 6, lineCap: .round))
            .frame(width: size, height: size)
            .rotationEffect(.degrees(turning ? 360 : 0))
            .animation(reduceMotion ? nil : .linear(duration: 1).repeatForever(autoreverses: false), value: turning)
            .onAppear { turning = true }
            .accessibilityLabel("Waiting")
    }
}

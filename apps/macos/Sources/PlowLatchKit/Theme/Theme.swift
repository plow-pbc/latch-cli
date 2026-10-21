import SwiftUI

/// Every visual choice the screens make: views use these names, never
/// literal colours, sizes or animations, so a design pass edits this folder.
public struct Theme: Sendable {
    public var colors = Colors()
    public var type = Typography()
    public var space = Spacing()
    public var radius = Radius()
    public var motion = Motion()

    public init() {}

    public struct Colors: Sendable {
        public var accent: Color = .accentColor
        public var serving: Color = .green
        public var starting: Color = .orange
        public var stopped: Color = .secondary
        public var error: Color = .red
        public var secondaryText: Color = .secondary
    }

    public struct Typography: Sendable {
        public var hero: Font = .largeTitle.weight(.semibold)
        public var title: Font = .title2.weight(.semibold)
        public var heading: Font = .headline
        public var body: Font = .body
        public var detail: Font = .callout
        public var mono: Font = .system(.callout, design: .monospaced)
    }

    public struct Spacing: Sendable {
        public var small: CGFloat = 6
        public var medium: CGFloat = 12
        public var large: CGFloat = 20
        public var page: CGFloat = 28
    }

    public struct Radius: Sendable {
        public var card: CGFloat = 12
        public var badge: CGFloat = 6
    }

    /// A theme's colour for a host state.
    public func color(for state: MenuState) -> Color {
        switch state {
        case .serving: colors.serving
        case .starting: colors.starting
        case .stopped, .loggedOut: colors.stopped
        case .error, .unreachable: colors.error
        }
    }
}

extension EnvironmentValues {
    @Entry public var theme = Theme()
}

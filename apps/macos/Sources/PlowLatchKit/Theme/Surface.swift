import SwiftUI

/// What a surface is for; how it looks (glass, material, flat) is decided
/// here only.
public enum SurfaceRole {
    /// A grouped block of content on a page.
    case card
    /// A control floating above content.
    case floating
}

extension View {
    public func latchSurface(_ role: SurfaceRole) -> some View {
        modifier(Surface(role: role))
    }
}

private struct Surface: ViewModifier {
    var role: SurfaceRole
    @Environment(\.theme) private var theme

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: theme.radius.card, style: .continuous)
        switch role {
        case .card:
            // Glass belongs to controls, not content, so cards use a plain fill.
            content.background(.background.secondary, in: shape)
        case .floating:
            content.glassEffect(.regular, in: shape)
        }
    }
}

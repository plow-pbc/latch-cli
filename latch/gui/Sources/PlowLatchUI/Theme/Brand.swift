import AppKit
import SwiftUI

/// Latch's look, the old Plow Latch app's, in both appearances: dark first
/// (a warm near-black window, off-white text, the volt accent), and a light
/// one with the same warmth. Views use these names, never literal colours.
enum Brand {
    // Dark values are the running old app's, as sampled for v9
    // (gatekeeper-faithful.md): the window's body, its footer bar, text, and
    // the text's muted (48%) and border (8%) strengths.
    static let content = Color(light: 0xF6F5EF, dark: 0x0C0C0B)
    /// The footer bar.
    static let bar = Color(light: 0xEFEEE7, dark: 0x0E0E0D)
    static let text = Color(light: 0x161614, dark: 0xF0F0E8)
    static let muted = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.56, darkOpacity: 0.48)
    static let line = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.1, darkOpacity: 0.08)
    /// Fields and grouped rows: the old app's surface (4.5%) and the
    /// stronger border its fields have (14%).
    static let surface = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.045)
    static let fieldBorder = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.16, darkOpacity: 0.14)
    /// The accent: volt on dark; a deep olive on light, where volt text
    /// would not be readable. Fills keep volt in both.
    static let volt = Color(light: 0x4E6B00, dark: 0xD5EF8A)
    static let voltFill = Color(light: 0xD5EF8A, dark: 0xD5EF8A)
    static let onVolt = Color(light: 0x01000A, dark: 0x01000A)
    static let sage = Color(light: 0x5E7A5E, dark: 0xA6C2A6)
    static let amber = Color(light: 0x9A5B00, dark: 0xFEBC2E)
    static let clay = Color(light: 0xA14A2B, dark: 0xD89478)

    static let headline = Font.largeTitle.weight(.semibold)
    static let lead = Font.title3
    static let code = Font.system(.title3, design: .monospaced).weight(.semibold)

    /// The onboarding window's content size.
    static let windowSize = CGSize(width: 600, height: 720)
    /// The welcome mark's height.
    static let markHeight: CGFloat = 260
    /// A screen change: fade in while rising a little.
    static let stepChange = Animation.easeOut(duration: 0.18)
}

extension Color {
    /// One colour for each appearance, from 0xRRGGBB.
    init(light: UInt32, dark: UInt32, opacity: Double = 1, darkOpacity: Double? = nil) {
        func rgb(_ hex: UInt32, _ alpha: Double) -> NSColor {
            NSColor(srgbRed: Double(hex >> 16 & 0xFF) / 255, green: Double(hex >> 8 & 0xFF) / 255,
                    blue: Double(hex & 0xFF) / 255, alpha: alpha)
        }
        self.init(nsColor: NSColor(name: nil) { appearance in
            appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? rgb(dark, darkOpacity ?? opacity) : rgb(light, opacity)
        })
    }
}

/// The volt primary button: the old app's, a filled capsule with dark text.
struct VoltButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .foregroundStyle(Brand.onVolt)
            .padding(.horizontal, 18)
            .frame(minHeight: 34)
            .background(Brand.voltFill.opacity(isEnabled ? 1 : 0.4), in: Capsule())
            .brightness(configuration.isPressed ? -0.06 : 0)
    }
}

extension View {
    /// Animates `value`'s changes, or not at all under Reduce Motion.
    func latchAnimation<V: Equatable>(_ animation: Animation, value: V) -> some View {
        modifier(ReducibleAnimation(animation: animation, value: value))
    }
}

private struct ReducibleAnimation<V: Equatable>: ViewModifier {
    var animation: Animation
    var value: V
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content.animation(reduceMotion ? nil : animation, value: value)
    }
}

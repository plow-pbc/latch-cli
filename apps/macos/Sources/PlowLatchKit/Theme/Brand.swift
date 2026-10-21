import SwiftUI

/// Plow's own look, used by onboarding: a warm near-black window, off-white
/// text and the volt accent.
public struct Brand: Sendable {
    public var window = Color(red: 0x11 / 255, green: 0x11 / 255, blue: 0x10 / 255)
    public var content = Color(red: 0x0C / 255, green: 0x0C / 255, blue: 0x0B / 255)
    public var text = Color(red: 0xF0 / 255, green: 0xF0 / 255, blue: 0xE8 / 255)
    public var volt = Color(red: 0xD5 / 255, green: 0xEF / 255, blue: 0x8A / 255)
    public var sage = Color(red: 0xA6 / 255, green: 0xC2 / 255, blue: 0xA6 / 255)
    public var iris = Color(red: 0xC4 / 255, green: 0xBF / 255, blue: 0xFF / 255)
    public var clay = Color(red: 0xC0 / 255, green: 0x73 / 255, blue: 0x55 / 255)
    public var amber = Color(red: 0xFE / 255, green: 0xBC / 255, blue: 0x2E / 255)
    public var success = Color(red: 0x6F / 255, green: 0xD0 / 255, blue: 0x8A / 255)
    /// The text colour at a weaker level, for secondary lines.
    public var muted: Color { text.opacity(0.56) }

    public var headline: Font = .system(size: 26, weight: .semibold)
    public var tagline: Font = .system(size: 30, weight: .semibold)
    public var lead: Font = .system(size: 15)
    public var code: Font = .system(size: 15, weight: .semibold, design: .monospaced)
    public var button: Font = .system(size: 13, weight: .semibold)

    /// The onboarding window's fixed content size.
    public var windowSize = CGSize(width: 600, height: 720)
    /// The welcome mark's height.
    public var markHeight: CGFloat = 300
    /// A screen change: fade in while rising 6 pt.
    public var stepChange: Animation = .easeOut(duration: 0.15)

    public init() {}
}

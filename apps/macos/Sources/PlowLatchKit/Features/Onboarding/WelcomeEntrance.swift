import SwiftUI

/// Everything that moves in the welcome entrance, at one moment. The
/// timeline is the old app's: the mark's outline draws on, the fill fades in
/// as the outline melts away, a sheen sweeps across, the glow rises and
/// settles, then the tagline and the button rise. It settles at 2.08 s.
public struct EntranceFrame: Equatable, Sendable {
    /// How much of the outline is drawn, 0–1.
    public var draw: Double
    public var scale: Double
    /// The three glow layers' strengths, tight to wide.
    public var glow: [Double]
    public var fill: Double
    /// The outline's width in the mark's 400-unit space, and its opacity.
    public var strokeWidth: Double
    public var strokeOpacity: Double
    /// The sheen band's left edge in the mark's 400-unit space, and its opacity.
    public var sheenX: Double
    public var sheenOpacity: Double
    public var bloomOpacity: Double
    public var bloomScale: Double
    /// The tagline's and the button's reveal: opacity, and how far below
    /// their place they still are, in points.
    public var tagline: Double
    public var taglineOffset: Double
    public var button: Double
    public var buttonOffset: Double

    /// When everything has stopped moving, except the bloom's breathing.
    public static let settlesAt = 2.08

    /// The entrance at its end: what Reduce Motion shows.
    public static let rest = at(10)

    /// The entrance `t` seconds after it starts.
    public static func at(_ t: Double) -> EntranceFrame {
        let draw = Self.progress(t, start: 0.05, duration: 0.95, curve: .css(0.66, 0, 0.2, 1))
        let fill = Self.progress(t, start: 0.82, duration: 0.55, curve: .css(0.3, 0.6, 0.25, 1))
        let reveal = UnitCurve.css(0.2, 0.7, 0.2, 1)
        let tagline = Self.progress(t, start: 1.13, duration: 0.6, curve: reveal)
        let button = Self.progress(t, start: 1.48, duration: 0.6, curve: reveal)
        let sheenRaw = Self.progress(t, start: 1.14, duration: 0.62, curve: .linear)
        let sheen = Self.progress(t, start: 1.14, duration: 0.62, curve: .css(0.4, 0, 0.25, 1))
        let stroke = Self.keyframes(t, start: 0.9, duration: 0.55, curve: .css(0.4, 0, 0.3, 1),
                                    [(0, [1.4, 0.95]), (0.7, [0.9, 0.35]), (1, [0, 0])])
        let glow = Self.keyframes(t, start: 0.05, duration: 1.7, curve: .css(0.3, 0.5, 0.2, 1),
                                  [(0, [0.3, 0.26, 0.14]), (0.45, [0.42, 0.34, 0.18]), (1, [0.22, 0.16, 0.12])])
        let bloom = Self.keyframes(t, start: 0.05, duration: 1.7, curve: .css(0.25, 0.6, 0.2, 1),
                                   [(0, [0, 0.86]), (0.4, [0.92, 1.06]), (1, [0.7, 1])])
        return EntranceFrame(
            draw: draw,
            scale: 0.965 + 0.035 * Self.progress(t, start: 0.05, duration: 0.95, curve: .css(0.2, 0.7, 0.2, 1)),
            glow: glow,
            fill: fill,
            strokeWidth: stroke[0],
            strokeOpacity: stroke[1],
            sheenX: -400 + 820 * sheen,
            // The band shows from its start until 99% of its sweep, then goes.
            sheenOpacity: t >= 1.14 && sheenRaw < 0.99 ? 1 : 0,
            bloomOpacity: bloom[0],
            bloomScale: bloom[1],
            tagline: tagline,
            taglineOffset: 14 * (1 - tagline),
            button: button,
            buttonOffset: 14 * (1 - button))
    }

    /// 0 before `start`, 1 after it ends, eased between.
    private static func progress(_ t: Double, start: Double, duration: Double, curve: UnitCurve) -> Double {
        let linear = min(max((t - start) / duration, 0), 1)
        return curve.value(at: linear)
    }

    /// Values between keyframes, each segment eased by `curve` as CSS does.
    private static func keyframes(
        _ t: Double, start: Double, duration: Double, curve: UnitCurve, _ frames: [(Double, [Double])]
    ) -> [Double] {
        let p = min(max((t - start) / duration, 0), 1)
        let next = frames.firstIndex { $0.0 >= p } ?? frames.count - 1
        guard next > 0 else { return frames[0].1 }
        let (p0, v0) = frames[next - 1]
        let (p1, v1) = frames[next]
        let eased = curve.value(at: (p - p0) / (p1 - p0))
        return zip(v0, v1).map { $0 + ($1 - $0) * eased }
    }
}

extension UnitCurve {
    /// A CSS `cubic-bezier(x1, y1, x2, y2)`.
    static func css(_ x1: Double, _ y1: Double, _ x2: Double, _ y2: Double) -> UnitCurve {
        .bezier(startControlPoint: UnitPoint(x: x1, y: y1), endControlPoint: UnitPoint(x: x2, y: y2))
    }
}

/// The glowing Plow mark with its bloom, drawn at one entrance frame.
struct EntranceMark: View {
    var frame: EntranceFrame
    /// Once settled, the bloom breathes slowly, forever.
    var breathing = false
    @Environment(\.theme) private var theme

    var body: some View {
        let brand = theme.brand
        let size = brand.markHeight
        let unit = size / 400
        // The old mark was 360 px tall; its glow radii scale with it.
        let px = size / 360
        ZStack {
            bloom(size: size)
            ZStack {
                PlowMark().fill(brand.volt.opacity(frame.fill), style: FillStyle(eoFill: true))
                PlowMark(drawn: frame.draw)
                    .stroke(brand.volt.opacity(frame.strokeOpacity),
                            style: StrokeStyle(lineWidth: frame.strokeWidth * unit, lineCap: .round, lineJoin: .round))
                sheen(size: size, unit: unit)
            }
            .frame(width: size, height: size)
            .scaleEffect(frame.scale)
            .shadow(color: brand.volt.opacity(frame.glow[0]), radius: 5 * px)
            .shadow(color: brand.volt.opacity(frame.glow[1]), radius: 18 * px)
            .shadow(color: brand.volt.opacity(frame.glow[2]), radius: 34 * px, y: 10 * px)
        }
        .accessibilityElement()
        .accessibilityLabel("Plow")
    }

    @ViewBuilder private func bloom(size: CGFloat) -> some View {
        let glow = Circle()
            .fill(RadialGradient(
                stops: [
                    .init(color: theme.brand.volt.opacity(0.14), location: 0),
                    .init(color: theme.brand.volt.opacity(0.06), location: 0.3),
                    .init(color: theme.brand.volt.opacity(0.02), location: 0.52),
                    .init(color: .clear, location: 0.72),
                ],
                center: .center, startRadius: 0, endRadius: size * 0.72))
            .frame(width: size * 1.44, height: size * 1.44)
            .blur(radius: size * 0.18)
            .offset(y: size * 0.08)
            .allowsHitTesting(false)
        if breathing {
            glow.phaseAnimator([false, true]) { view, deep in
                view.opacity(deep ? 0.82 : 0.7).scaleEffect(deep ? 1.05 : 1)
            } animation: { _ in .easeInOut(duration: 3) }
        } else {
            glow.opacity(frame.bloomOpacity).scaleEffect(frame.bloomScale)
        }
    }

    private func sheen(size: CGFloat, unit: CGFloat) -> some View {
        Rectangle()
            .fill(LinearGradient(
                stops: [
                    .init(color: .white.opacity(0), location: 0),
                    .init(color: .white.opacity(0), location: 0.42),
                    .init(color: .white.opacity(0.9), location: 0.5),
                    .init(color: .white.opacity(0), location: 0.58),
                    .init(color: .white.opacity(0), location: 1),
                ],
                startPoint: .topLeading, endPoint: UnitPoint(x: 1, y: 0.35)))
            .frame(width: size, height: size)
            .offset(x: frame.sheenX * unit)
            .opacity(frame.sheenOpacity)
            .mask { PlowMark().fill(style: FillStyle(eoFill: true)) }
    }
}

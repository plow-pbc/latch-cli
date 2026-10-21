import SwiftUI

/// The app's named animations.
public struct Motion: Sendable {
    /// Content arriving: a list row, a screen's data.
    public var appear: Animation = .smooth(duration: 0.25)
    /// A state badge or icon changing.
    public var stateChange: Animation = .snappy(duration: 0.2)

    public init() {}
}

extension View {
    /// Animates `value`'s changes with `animation`, or not at all under Reduce Motion.
    public func latchAnimation<V: Equatable>(_ animation: Animation, value: V) -> some View {
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

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

extension EnvironmentValues {
    /// Overrides the system's Reduce Motion when set, so previews and
    /// snapshots can show either path.
    @Entry public var reduceMotionOverride: Bool? = nil
}

/// Whether to keep still: the system's Reduce Motion, unless overridden.
@propertyWrapper
public struct ReducedMotion: DynamicProperty {
    @Environment(\.accessibilityReduceMotion) private var system
    @Environment(\.reduceMotionOverride) private var override

    public init() {}

    public var wrappedValue: Bool { override ?? system }
}

private struct ReducibleAnimation<V: Equatable>: ViewModifier {
    var animation: Animation
    var value: V
    @ReducedMotion private var reduceMotion

    func body(content: Content) -> some View {
        content.animation(reduceMotion ? nil : animation, value: value)
    }
}

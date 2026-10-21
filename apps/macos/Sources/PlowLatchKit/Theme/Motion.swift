import AppKit
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

extension View {
    /// Keeps `isOnScreen` true while this view's window is visible on screen.
    /// SwiftUI keeps a closed or hidden window's views alive, and anything
    /// that animates in them keeps the app busy; this is how they stop.
    public func trackingOnScreen(_ isOnScreen: Binding<Bool>) -> some View {
        background(WindowOnScreen(isOnScreen: isOnScreen))
    }
}

private struct WindowOnScreen: NSViewRepresentable {
    @Binding var isOnScreen: Bool

    func makeNSView(context: Context) -> Probe {
        let probe = Probe()
        probe.changed = { visible in
            if isOnScreen != visible { isOnScreen = visible }
        }
        return probe
    }

    func updateNSView(_ nsView: Probe, context: Context) {}

    final class Probe: NSView {
        var changed: (Bool) -> Void = { _ in }
        private var observer: NSObjectProtocol?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let observer { NotificationCenter.default.removeObserver(observer) }
            guard let window else { return changed(false) }
            observer = NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.report() }
            }
            report()
        }

        private func report() {
            changed(window.map { $0.isVisible && $0.occlusionState.contains(.visible) } ?? false)
        }
    }
}

import AppKit
import SwiftUI

/// Every way a window opens goes through here. An app with no Dock icon
/// can't bring a window forward on macOS 26 (`openSettings` and `openWindow`
/// open it behind other apps, or not at all), so the app becomes a regular,
/// active app first.
@MainActor
public final class WindowRouter {
    public static let shared = WindowRouter()
    public static let mainWindowID = "main"
    public static let onboardingWindowID = "onboarding"
    public static let keepInMenuBarKey = "KeepInMenuBar"
    /// Whether this Mac finished onboarding when the app last knew: decides
    /// which window opens at launch, before latch has answered.
    public static let onboardedKey = "Onboarded"

    private var openWindow: OpenWindowAction?
    private var openSettings: OpenSettingsAction?

    /// Called by long-lived views with their environment's actions, which
    /// SwiftUI only hands to views.
    public func install(openWindow: OpenWindowAction, openSettings: OpenSettingsAction) {
        self.openWindow = openWindow
        self.openSettings = openSettings
    }

    public func showMain() {
        activate()
        if let window = window(Self.mainWindowID) {
            window.makeKeyAndOrderFront(nil)
        } else {
            openWindow?(id: Self.mainWindowID)
        }
    }

    /// Opens onboarding in place of the main window.
    public func showOnboarding() {
        UserDefaults.standard.set(false, forKey: Self.onboardedKey)
        activate()
        window(Self.mainWindowID)?.close()
        if let window = window(Self.onboardingWindowID) {
            window.makeKeyAndOrderFront(nil)
        } else {
            openWindow?(id: Self.onboardingWindowID)
        }
    }

    /// Onboarding's Done: the main window takes its place.
    public func finishOnboarding() {
        UserDefaults.standard.set(true, forKey: Self.onboardedKey)
        window(Self.onboardingWindowID)?.close()
        showMain()
    }

    /// Once latch has said whether this Mac is logged in: a logged-out Mac
    /// gets onboarding. Being logged in never ends onboarding; only its Done
    /// button does, so folders and sharing are always chosen.
    public func launched(loggedIn: Bool) {
        if !loggedIn, window(Self.onboardingWindowID) == nil {
            showOnboarding()
        }
    }

    public func showSettings() {
        activate()
        openSettings?()
        // The Settings window appears on the next turn of the run loop.
        DispatchQueue.main.async { [self] in
            guard let settings = window("Settings") else { return }
            settings.makeKeyAndOrderFront(nil)
            settings.orderFrontRegardless()
        }
    }

    /// When the last window closes, a "keep in menu bar" app gives up its
    /// Dock icon and lives in the menu bar alone.
    public func windowClosed() {
        guard UserDefaults.standard.bool(forKey: Self.keepInMenuBarKey) else { return }
        // The closing window is still visible while its close notification runs.
        DispatchQueue.main.async {
            if Self.visibleWindows().isEmpty { NSApp.setActivationPolicy(.accessory) }
        }
    }

    /// The app's own windows that people see: not the menu bar extra.
    public static func visibleWindows() -> [NSWindow] {
        NSApp.windows.filter { $0.isVisible && $0.canBecomeMain }
    }

    private func activate() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }

    private func window(_ id: String) -> NSWindow? {
        NSApp.windows.first { $0.identifier?.rawValue.contains(id) == true }
    }
}

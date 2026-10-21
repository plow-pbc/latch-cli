import PlowLatchKit
import SwiftUI

@main
struct PlowLatchApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        // Which window opens at launch follows the last known login, so
        // neither waits for latch; the app corrects it once latch answers.
        let onboarded = UserDefaults.standard.bool(forKey: WindowRouter.onboardedKey)

        Window("Plow Latch", id: WindowRouter.mainWindowID) {
            MainView().environment(appDelegate.model)
        }
        .defaultSize(width: 900, height: 600)
        .defaultLaunchBehavior(onboarded ? .presented : .suppressed)
        .commands { ScreenCommands() }

        Window("Welcome to Plow Latch", id: WindowRouter.onboardingWindowID) {
            OnboardingView().environment(appDelegate.model)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .defaultLaunchBehavior(onboarded ? .suppressed : .presented)
        .restorationBehavior(.disabled)

        Settings {
            SettingsView().environment(appDelegate.model)
        }

        MenuBarExtra {
            StatusMenu().environment(appDelegate.model)
        } label: {
            StatusIcon().environment(appDelegate.model)
        }
    }
}

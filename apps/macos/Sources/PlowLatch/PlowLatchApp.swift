import PlowLatchKit
import SwiftUI

@main
struct PlowLatchApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        Window("Plow Latch", id: WindowRouter.mainWindowID) {
            MainView().environment(appDelegate.model)
        }
        .defaultSize(width: 900, height: 600)
        .commands { ScreenCommands() }

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

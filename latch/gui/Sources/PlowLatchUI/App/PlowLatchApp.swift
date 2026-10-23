import SwiftUI

/// latch's way in (src/gui.rs): called on the main thread with the path of
/// the bundle's own latch, the state folder, and whether that folder is
/// latch's default (no `--state-dir`); returns when the app quits.
@_cdecl("latch_gui_main")
nonisolated public func latchGUIMain(_ executable: UnsafePointer<CChar>, _ stateDir: UnsafePointer<CChar>, _ defaultState: Int32) -> Int32 {
    let client = LiveLatchClient(executable: URL(filePath: String(cString: executable)), stateDir: String(cString: stateDir))
    MainActor.assumeIsolated {
        PlowLatchApp.model = AppModel(client: client, loginItemOffered: defaultState != 0)
        #if LATCH_TEST_HOOKS
        TestHooks.install(PlowLatchApp.model)
        #endif
        PlowLatchApp.main()
    }
    return 0
}

struct PlowLatchApp: App {
    /// Set by `latchGUIMain` before the app starts.
    static var model: AppModel!
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        let model = Self.model!
        // Which window opens follows what the app remembers, so neither
        // waits for latch; onboarding then asks latch where to resume.
        let finished = model.store.load().finished
        Window("Welcome to Latch", id: AppModel.onboardingWindow) {
            OnboardingView()
                .environment(model)
                .installWindowActions(model)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(finished ? .suppressed : .presented)

        Window("Latch", id: AppModel.mainWindow) {
            MainView()
                .environment(model)
                .installWindowActions(model)
        }
        .defaultSize(width: 900, height: 600)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(finished ? .presented : .suppressed)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        LatchProcess.terminateAll()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

extension View {
    /// Hands the model SwiftUI's window actions, which only views get.
    func installWindowActions(_ model: AppModel) -> some View {
        modifier(WindowActions(model: model))
    }
}

private struct WindowActions: ViewModifier {
    var model: AppModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow

    func body(content: Content) -> some View {
        content.onAppear {
            model.openWindow = { openWindow(id: $0) }
            model.dismissWindow = { dismissWindow(id: $0) }
        }
    }
}

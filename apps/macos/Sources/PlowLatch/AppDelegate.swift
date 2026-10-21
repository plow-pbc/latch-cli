import AppKit
import PlowLatchKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel(client: AppDelegate.client())
    private var hostWatch: Task<Void, Never>?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // After launch, so no CLI call delays the first frame.
        hostWatch = Task {
            await model.refreshStatus()
            await model.watchHost()
        }
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated { WindowRouter.shared.windowClosed() }
        }
        #if LATCH_TEST_HOOKS
        TestHooks.install(model: model)
        #endif
    }

    func applicationWillTerminate(_ notification: Notification) {
        LatchProcess.terminateAll()
    }

    /// The menu bar extra keeps the app useful with no window open.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }

    /// A Dock click with no window open brings the main window back.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows { WindowRouter.shared.showMain() }
        return false
    }

    /// The real latch, or with `-LatchFake <scenario>` the sample one.
    private static func client() -> any LatchClient {
        if let name = UserDefaults.standard.string(forKey: "LatchFake"),
           let scenario = FakeLatchClient.Scenario(rawValue: name) {
            return FakeLatchClient(scenario, liveEvery: .seconds(3))
        }
        return LiveLatchClient.bundled()
    }
}

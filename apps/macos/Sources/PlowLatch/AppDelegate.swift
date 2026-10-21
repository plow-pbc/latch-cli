import AppKit
import PlowLatchKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppDelegate.makeModel()
    private var hostWatch: Task<Void, Never>?

    func applicationDidFinishLaunching(_ notification: Notification) {
        // After launch, so no CLI call delays the first frame.
        hostWatch = Task {
            await model.refreshStatus()
            if case let .loaded(status, _) = model.status {
                WindowRouter.shared.launched(loggedIn: status.peer != nil)
            }
            await model.watchHost()
        }
        NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification, object: nil, queue: .main
        ) { _ in
            MainActor.assumeIsolated { WindowRouter.shared.windowClosed() }
        }
        // Login Items can be changed in System Settings while the app runs.
        NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [model] _ in
            MainActor.assumeIsolated { model.refreshAgent() }
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

    /// On the real latch, or with `-LatchFake <scenario>` the sample one.
    private static func makeModel() -> AppModel {
        if let name = UserDefaults.standard.string(forKey: "LatchFake"),
           let scenario = FakeLatchClient.Scenario(rawValue: name) {
            let fake = FakeLatchClient(scenario, liveEvery: .seconds(3))
            return AppModel(client: fake, sharing: fake)
        }
        let sharing: any SharingService = BackgroundSharing.bundled() ?? UnavailableSharing()
        return AppModel(client: LiveLatchClient.bundled(), sharing: sharing)
    }
}

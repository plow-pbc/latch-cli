#if LATCH_TEST_HOOKS
import AppKit
import PlowLatchKit

/// Only in builds made with `-D LATCH_TEST_HOOKS` (scripts/build-app.sh
/// --test-hooks): lets the measurement scripts drive the same paths a person
/// does, without accessibility access. Each command is a distributed
/// notification `co.plow.latch.test.<command>`; `report` writes the app's
/// window state as JSON to the path in the notification's object.
@MainActor
enum TestHooks {
    private static var model: AppModel?

    static func install(model: AppModel) {
        self.model = model
        let center = DistributedNotificationCenter.default()
        for command in ["show-main", "show-settings", "close-windows", "report", "quit", "start-sharing", "stop-sharing"] {
            center.addObserver(forName: .init("co.plow.latch.test.\(command)"), object: nil, queue: .main) { note in
                let path = note.object as? String
                MainActor.assumeIsolated { run(command, path: path) }
            }
        }
    }

    private static func report(to path: String) async {
        await model?.refreshAgent()
        let report: [String: Any] = [
            "bundle": Bundle.main.bundleIdentifier ?? NSNull(),
            "menu": model?.menuState.symbol ?? NSNull(),
            "state": model?.menuState.title ?? NSNull(),
            "agent": model?.agent.rawValue ?? NSNull(),
            "agentProcess": model?.agentProcess ?? NSNull(),
            "servePid": model?.status.value?.serve.pid ?? NSNull(),
            "sharingError": model?.sharingError ?? NSNull(),
            "policy": NSApp.activationPolicy() == .regular ? "regular" : "accessory",
            "active": NSApp.isActive,
            "key": NSApp.keyWindow?.identifier?.rawValue ?? NSNull(),
            "visible": WindowRouter.visibleWindows().map { $0.identifier?.rawValue ?? $0.title },
        ]
        let data = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
        FileManager.default.createFile(atPath: path, contents: data)
    }

    private static func run(_ command: String, path: String?) {
        switch command {
        case "show-main": WindowRouter.shared.showMain()
        case "show-settings": WindowRouter.shared.showSettings()
        case "close-windows": WindowRouter.visibleWindows().forEach { $0.performClose(nil) }
        case "quit": NSApp.terminate(nil)
        case "start-sharing": Task { await model?.startSharing() }
        case "stop-sharing": Task { await model?.stopSharing() }
        case "report":
            guard let path else { return }
            Task { await report(to: path) }
        default: break
        }
    }
}
#endif

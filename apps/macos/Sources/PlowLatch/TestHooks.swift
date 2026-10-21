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
        for command in ["show-main", "show-settings", "close-windows", "report", "quit", "start-sharing", "stop-sharing",
                        "show-screen", "snapshot", "onboarding-next", "onboarding-folder", "onboarding-keep-running",
                        "onboarding-not-now", "onboarding-done"] {
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
            "peer": model?.status.value?.peer?.name ?? NSNull(),
            "shares": model?.status.value?.shares ?? [],
            "serving": model?.status.value?.serve.ready ?? false,
            "activity": model?.activity.entries.prefix(5).map { "\($0.op) \($0.decision) \($0.outcome)" } ?? [],
            "onboarding": onboardingReport ?? NSNull(),
            "policy": NSApp.activationPolicy() == .regular ? "regular" : "accessory",
            "active": NSApp.isActive,
            "key": NSApp.keyWindow?.identifier?.rawValue ?? NSNull(),
            "visible": WindowRouter.visibleWindows().map { $0.identifier?.rawValue ?? $0.title },
            "windows": NSApp.windows.map {
                "\($0.identifier?.rawValue ?? $0.className) visible=\($0.isVisible) onscreen=\($0.occlusionState.contains(.visible))"
            },
        ]
        let data = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
        FileManager.default.createFile(atPath: path, contents: data)
    }

    /// Where onboarding is. Holds the sign-in code so a script can text it
    /// from the Plow twin; reports go only to .dev/.
    private static var onboardingReport: [String: Any]? {
        guard let flow = model?.onboarding else { return nil }
        var report: [String: Any] = ["step": "\(flow.step)"]
        switch flow.signIn {
        case .starting: report["signIn"] = "starting"
        case let .waiting(code):
            report["signIn"] = "waiting"
            report["code"] = code.code
            report["sendTo"] = code.sendTo
            report["smsto"] = code.smsto
            report["smsLink"] = code.smsLink.absoluteString
        case .expired: report["signIn"] = "expired"
        case let .failed(message): report["signIn"] = "failed: \(message)"
        case let .verified(peer): report["signIn"] = "verified as \(peer.name)"
        }
        if let error = flow.keepRunningError { report["error"] = error }
        return report
    }

    private static func snapshot(_ view: NSView, window: NSWindow, to path: String) {
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds),
              let context = NSGraphicsContext(bitmapImageRep: rep) else { return }
        NSGraphicsContext.current = context
        window.effectiveAppearance.performAsCurrentDrawingAppearance {
            NSColor.windowBackgroundColor.setFill()
            view.bounds.fill()
        }
        context.cgContext.translateBy(x: 0, y: view.bounds.height)
        context.cgContext.scaleBy(x: 1, y: -1)
        view.layer?.render(in: context.cgContext)
        NSGraphicsContext.current = nil
        FileManager.default.createFile(atPath: path, contents: rep.representation(using: .png, properties: [:]))
    }

    private static func run(_ command: String, path: String?) {
        switch command {
        // What the menu's Open Plow Latch… does.
        case "show-main": WindowRouter.shared.showHome(onboarding: model?.needsOnboarding ?? false)
        case "show-settings": WindowRouter.shared.showSettings()
        case "close-windows": WindowRouter.visibleWindows().forEach { $0.performClose(nil) }
        case "quit": NSApp.terminate(nil)
        case "start-sharing": Task { await model?.startSharing() }
        case "stop-sharing": Task { await model?.stopSharing() }
        case "report":
            guard let path else { return }
            Task { await report(to: path) }
        case "show-screen": if let name = path, let screen = Screen(rawValue: name) { model?.screen = screen }
        // The app's own drawing of its front window, to the PNG at the object's path.
        case "snapshot":
            guard let path, let window = WindowRouter.visibleWindows().first, let view = window.contentView else { return }
            snapshot(view, window: window, to: path)
        case "onboarding-next": model?.onboarding?.next()
        case "onboarding-folder": if let path { model?.onboarding?.addFolders([URL(filePath: path)]) }
        case "onboarding-keep-running": Task { await model?.onboarding?.keepRunning(startSharing: true) }
        case "onboarding-not-now": Task { await model?.onboarding?.keepRunning(startSharing: false) }
        case "onboarding-done": model?.onboarding?.finish()
        default: break
        }
    }
}
#endif

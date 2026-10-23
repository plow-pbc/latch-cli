#if LATCH_TEST_HOOKS
import AppKit

/// A test build's remote control (`gui/build.sh --test-hooks`): distributed
/// notifications `co.plow.latch.test.BUNDLE_ID.COMMAND`, with an argument
/// as the object, press the same buttons a person does, write the window to
/// a PNG and the flow's state to JSON, so an end-to-end run needs no
/// Accessibility or screen-recording permission. Nothing it does reaches
/// the rest of the Mac: links, Finder reveals and the login item are
/// recorded, never made. Each test app hears only its own bundle id, so
/// test apps running side by side never drive each other.
enum TestHooks {
    /// What the app would have opened, for `state`.
    static var opened: [String] = []
    /// The login item as this run left it.
    static var loginItem = false

    static func install(_ app: AppModel) {
        // Nothing leaves the app: links and Finder reveals are recorded.
        app.open = { opened.append("open \($0.absoluteString)") }
        app.reveal = { opened.append("reveal \($0.path)") }
        app.setLoginItem = { on in
            loginItem = on
            opened.append("login item \(on ? "on" : "off")")
        }
        app.loginItemIsOn = { loginItem }
        let center = DistributedNotificationCenter.default()
        func on(_ command: String, _ act: @escaping @MainActor (String?) -> Void) {
            center.addObserver(forName: .init("co.plow.latch.test.\(Bundle.main.bundleIdentifier ?? "").\(command)"), object: nil, queue: .main) { note in
                let argument = note.object as? String
                MainActor.assumeIsolated { act(argument) }
            }
        }
        on("primary") { _ in app.onboarding?.primary() }
        on("skip") { _ in app.onboarding?.skip() }
        on("back") { _ in app.onboarding?.back() }
        on("type") { text in app.onboarding?.draftRules = text ?? "" }
        on("use-name") { name in
            app.onboarding?.loginName = name ?? ""
            app.onboarding?.useAnotherName()
        }
        on("replace") { _ in app.onboarding?.replaceMachine() }
        on("start-at-login") { value in app.onboarding?.startAtLogin = value == "1" }
        on("set-up-again") { _ in app.setUpAgain() }
        // Moves the window off every display, where the window server still
        // draws it while the screen is locked or asleep (snapshots of an
        // on-screen window come back blank then).
        on("offscreen") { _ in
            NSApp.windows.first { $0.isVisible && $0.canBecomeMain }?.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        }
        // The main window's host picker and sidebar, as a click picks them.
        on("host") { name in app.chosenHost = name }
        on("section") { item in if let item = item.flatMap(SidebarItem.init(rawValue:)) { app.section = item } }
        // Activity's selection and filter, as a click sets them.
        on("record") { id in app.selectedRecord = id }
        on("filter") { name in if let filter = name.flatMap(ActivityView.Filter.init(rawValue:)) { app.activityFilter = filter } }
        on("connections") { value in app.showConnections = value == "1" }
        on("quit") { _ in NSApp.terminate(nil) }
        on("snapshot") { path in
            guard let path, let window = NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
            try? Render.window(window, to: URL(filePath: path))
        }
        // A key press, delivered to the app as the keyboard would: `return`,
        // `tab` or `space`; nothing outside the app is touched.
        on("key") { name in
            guard let window = NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
            let keys: [String: (String, UInt16)] = ["return": ("\r", 36), "tab": ("\t", 48), "space": (" ", 49)]
            guard let (characters, code) = keys[name ?? ""] else { return }
            for type in [NSEvent.EventType.keyDown, .keyUp] {
                if let event = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                                windowNumber: window.windowNumber, context: nil, characters: characters,
                                                charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code) {
                    NSApp.sendEvent(event)
                }
            }
        }
        on("focus") { path in
            guard let path, let window = NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
            try? "\(window.firstResponder.map { "\($0)" } ?? "none")".write(toFile: path, atomically: true, encoding: .utf8)
        }
        on("state") { path in
            guard let path else { return }
            let flow = app.onboarding
            let state: [String: Any] = [
                "windows": NSApp.windows.filter { $0.isVisible && $0.canBecomeMain }.map(\.title),
                "step": flow?.step.rawValue ?? "",
                "primary": flow.map { "\($0.primaryTitle)\($0.primaryEnabled ? "" : " (disabled)")" } ?? "",
                "login": flow.map { "\($0.login)" } ?? "",
                "rules": flow?.savedRules ?? "",
                "preview": flow?.previewNote ?? "",
                "google": flow.map { "\($0.google)" } ?? "",
                "messages": flow.map { "\($0.messages)" } ?? "",
                "contacts": flow.map { "\($0.contacts)" } ?? "",
                "start_error": flow?.startError ?? "",
                "passed": app.progress.passed.mapValues(\.rawValue).reduce(into: [String: String]()) { $0[$1.key.rawValue] = $1.value },
                "finished": app.progress.finished,
                "status": app.status.map { ["peer": $0.peer?.name ?? "", "serve": $0.serve.state] } ?? [:],
                "opened": opened,
            ]
            try? JSONSerialization.data(withJSONObject: state, options: [.prettyPrinted, .sortedKeys]).write(to: URL(filePath: path))
        }
    }
}
#endif

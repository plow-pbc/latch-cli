#if LATCH_TEST_HOOKS
import AppKit

/// A test build's remote control (`gui/build.sh --test-hooks`): distributed
/// notifications `co.plow.latch.test.BUNDLE_ID.COMMAND`, with an argument
/// as the object, press the same buttons a person does, write the window to
/// a PNG and the flow's state to JSON, so an end-to-end run needs no
/// Accessibility or screen-recording permission. Nothing it does reaches
/// the rest of the Mac: links and the login item are recorded, never made,
/// and the Full Disk Access helper follows only a stand-in window this run
/// names, never System Settings. Each test app hears only its own bundle
/// id, so test apps running side by side never drive each other.
enum TestHooks {
    /// What the app would have opened, for `state`.
    static var opened: [String] = []
    /// The login item as this run left it.
    static var loginItem = false
    /// The window the helper follows in place of System Settings'.
    static var standIn: pid_t?
    /// A helper floated by `grant-helper`.
    static var helper: GrantHelper?

    static func install(_ app: AppModel) {
        // Nothing leaves the app: links are recorded.
        app.open = { opened.append("open \($0.absoluteString)") }
        GrantHelper.settingsApp = { standIn }
        let guide = app.guideGrant
        app.guideGrant = { bundle in
            opened.append("guide \(bundle.path)")
            return guide(bundle)
        }
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
        on("deck") { index in
            guard let index = Int(index ?? ""), Onboarding.decks.indices.contains(index) else { return }
            app.onboarding?.choose(Onboarding.decks[index])
        }
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
        // Try a request: "AS|REQUEST", AS being you, new, or an agent's uid.
        on("try") { argument in
            guard let argument, let bar = argument.firstIndex(of: "|"), let host = app.host else { return }
            let who = String(argument[..<bar])
            app.trial.caller = who == "you" ? .you : who == "new" ? .newAgent : .agent(who)
            app.trial.request = String(argument[argument.index(after: bar)...])
            Task { await app.trial.run(on: host, app: app) }
        }
        on("focus") { uid in app.rulesFocus = uid }
        on("agent") { uid in app.agentsPath = uid.map { [$0] } ?? [] }
        // A host pill's click: "HOST|UID".
        on("pill") { argument in
            guard let parts = argument?.split(separator: "|"), parts.count == 2 else { return }
            app.chosenHost = String(parts[0])
            app.rulesFocus = String(parts[1])
            app.section = .rules
        }
        // Saves "UID|TEXT" (UID empty: every agent's) on the selected host
        // as Save does, over the revision on screen.
        on("save-rules") { argument in
            guard let argument, let bar = argument.firstIndex(of: "|"), let host = app.host,
                  case let .success(show)? = app.summaries[host.name]?.rules else { return }
            let uid = argument[..<bar].isEmpty ? nil : String(argument[..<bar])
            let revision = uid.map { show.agent($0)?.revision } ?? show.everyAgent?.revision
            Task {
                do {
                    try await app.saveRules(String(argument[argument.index(after: bar)...]), on: host, agent: uid, revision: revision)
                    print("save-rules: saved")
                } catch {
                    print("save-rules: refused: \((error as? LatchError)?.message ?? error.localizedDescription)")
                }
            }
        }
        on("quit") { _ in NSApp.terminate(nil) }
        on("settings-stand-in") { pid in standIn = pid.flatMap { pid_t($0) } }
        on("grant-helper") { path in
            helper?.stop()
            helper = path.map { GrantHelper.show(dragging: URL(filePath: $0)) }
        }
        on("grant-helper-granted") { _ in helper?.granted() }
        on("grant-helper-snapshot") { path in
            guard let path, let view = helper?.contentView else { return }
            try? Render.png(view, to: URL(filePath: path), background: false)
        }
        on("grant-helper-state") { path in
            guard let path else { return }
            func rect(_ r: CGRect?) -> [Double] { r.map { [$0.minX, $0.minY, $0.width, $0.height] } ?? [] }
            let state: [String: Any] = [
                "showing": helper?.isShowing ?? false,
                "frame": rect(helper?.isShowing == true ? helper?.frame : nil),
                "settings": rect(helper?.settingsFrame),
                "granted": helper?.isGranted ?? false,
            ]
            try? JSONSerialization.data(withJSONObject: state, options: [.sortedKeys]).write(to: URL(filePath: path))
        }
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
                "previews": (flow?.deck.examples ?? []).map { example in
                    switch flow?.previews[example.id] {
                    case let .verdict(answer)?: "\(example.title): \(answer.verdict)"
                    case let .failed(message)?: "\(example.title): failed \(message)"
                    case .checking?: "\(example.title): checking"
                    default: "\(example.title): not checked"
                    }
                },
                "reviewing": flow?.reviewing ?? false,
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

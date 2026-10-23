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
    /// A background-only test app (gui/hooks.sh's, unless built with
    /// --window) opens no window, so its flow starts here, not in one.
    static let headless = Bundle.main.object(forInfoDictionaryKey: "LSBackgroundOnly") as? Bool == true

    /// A link as `state` records it. A Google sign-in link's code is a
    /// credential until used, so only the link's shape is kept.
    static func recorded(_ url: URL) -> String {
        guard let code = url.path.range(of: "/connect/") else { return url.absoluteString }
        return "\(url.scheme ?? "")://\(url.host ?? "")\(url.port.map { ":\($0)" } ?? "")\(url.path[..<code.upperBound])…"
    }

    static func install(_ app: AppModel) {
        // Nothing leaves the app: links and Finder reveals are recorded.
        app.open = { opened.append("open \(recorded($0))") }
        app.reveal = { opened.append("reveal \($0.path)") }
        app.copy = { opened.append("copy \($0)") }
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
        if headless {
            Task { await app.resumeOnboarding() }
        }
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
        // Resizes the main window ("WIDTHxHEIGHT"), as a drag of its corner.
        on("resize") { size in
            guard let parts = size?.split(separator: "x").compactMap({ Double($0) }), parts.count == 2,
                  let window = NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain }) else { return }
            window.setFrame(CGRect(origin: window.frame.origin, size: CGSize(width: parts[0], height: parts[1])), display: true)
        }
        // The main window's host picker and sidebar, as a click picks them.
        on("host") { name in app.chosenHost = name }
        on("section") { item in if let item = item.flatMap(SidebarItem.init(rawValue:)) { app.section = item } }
        // Activity's selection and filter, as a click sets them.
        on("record") { id in app.selectedRecord = id }
        on("filter") { name in if let filter = name.flatMap(ActivityView.Filter.init(rawValue:)) { app.activityFilter = filter } }
        on("connections") { value in app.showConnections = value == "1" }
        // Try a request: "AS|REQUEST", AS being new or an agent's uid.
        on("try") { argument in
            guard let argument, let bar = argument.firstIndex(of: "|"), let host = app.host else { return }
            let who = String(argument[..<bar])
            app.trial.caller = who == "new" ? .newAgent : .agent(who)
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
                    print("save-rules: refused: \(LatchError.from(error).message)")
                }
            }
        }
        // Vault, as a person clicks and types: select an item, open New
        // Item ("KIND") or an item's Edit, type "LABEL=VALUE" (title, sites
        // and other-sites too), save, remove an item, pick an export (a
        // path, as the file chooser gives it), and import its picked rows.
        func vaultItem(_ id: String?) -> VaultList.Item? {
            guard let host = app.host, case let .success(vault)? = app.summaries[host.name]?.vault else { return nil }
            return vault.items.first { $0.item == id }
        }
        on("vault-select") { id in app.selectedVaultItem = id }
        on("vault-new") { kind in
            guard let host = app.host else { return }
            app.vaultSheet = .editor(VaultEditor(kind: kind.flatMap(VaultKind.init(rawValue:)) ?? .login, on: host))
        }
        on("vault-edit") { id in
            guard let item = vaultItem(id), let host = app.host else { return }
            Task { await app.beginVaultEdit(item, on: host) }
        }
        on("vault-type") { argument in
            guard case let .editor(editor)? = app.vaultSheet, let argument, let equals = argument.firstIndex(of: "=") else { return }
            let label = String(argument[..<equals]), value = String(argument[argument.index(after: equals)...])
            switch label {
            case "title": editor.title = value
            case "sites": editor.sites = value
            case "other-sites": editor.otherTopSites = value
            default: editor.values[label] = value
            }
        }
        on("vault-save") { _ in
            guard case let .editor(editor)? = app.vaultSheet else { return }
            Task { if await editor.save(app: app) { app.vaultSheet = nil } }
        }
        on("vault-remove") { id in
            guard let item = vaultItem(id), let host = app.host else { return }
            Task { await app.removeVaultItem(item, on: host) }
        }
        on("vault-import") { path in
            guard let path, let host = app.host else { return }
            let importer = VaultImport(file: URL(filePath: path), on: host)
            app.vaultSheet = .importer(importer)
            Task { await importer.load(app: app) }
        }
        on("vault-import-run") { _ in
            guard case let .importer(importer)? = app.vaultSheet else { return }
            Task { await importer.run(app: app) }
        }
        on("sheet-close") { _ in
            app.vaultSheet = nil
            app.newAgent = nil
        }
        // Host Settings' buttons and toggles, as a click (and its
        // confirmation) presses them: "NAME|1" turns a connector on.
        on("check-update") { _ in Task { await app.checkForUpdate() } }
        on("restart") { _ in Task { await app.restartHost() } }
        on("stop") { _ in Task { await app.stopHost() } }
        on("start") { _ in Task { await app.startHosting() } }
        on("connector") { argument in
            guard let parts = argument?.split(separator: "|"), parts.count == 2, let host = app.host else { return }
            Task { try? await app.setConnector(String(parts[0]), on: parts[1] == "1", host: host) }
        }
        on("device-remove") { name in
            guard let name else { return }
            Task { await app.removeDevice(name) }
        }
        on("logout") { _ in Task { try? await app.logOut() } }
        // New Agent: open it, fill "FIELD=VALUE" (kind own|cloud, name,
        // cloud, line, file: the path the save panel would give), create.
        on("new-agent") { _ in app.newAgent = NewAgent() }
        on("agent-type") { argument in
            guard let flow = app.newAgent, let argument, let equals = argument.firstIndex(of: "=") else { return }
            let value = String(argument[argument.index(after: equals)...])
            switch argument[..<equals] {
            case "kind": flow.kind = value == "own" ? .own : .cloud
            case "name": flow.name = value
            case "cloud": flow.cloudKind = value
            case "line": flow.line = value
            case "file": flow.tokenFile = URL(filePath: value)
            default: break
            }
        }
        on("agent-create") { _ in
            guard let flow = app.newAgent else { return }
            Task { await flow.create(app: app) }
        }
        // The main window's open sheet, as the window server shows it.
        on("snapshot-sheet") { path in
            guard let path, let sheet = NSApp.windows.first(where: { $0.isVisible && $0.canBecomeMain })?.attachedSheet else { return }
            try? Render.window(sheet, to: URL(filePath: path))
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
                    case let .failed(error)?: "\(example.title): failed \(error.message)"
                    case .checking?: "\(example.title): checking"
                    default: "\(example.title): not checked"
                    }
                },
                "reviewing": flow?.reviewing ?? false,
                "google": flow.map { "\($0.google)" } ?? "",
                "messages": flow.map { "\($0.messages)" } ?? "",
                "contacts": flow.map { "\($0.contacts)" } ?? "",
                "start_error": flow?.startError?.message ?? "",
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

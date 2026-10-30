import AppKit
import Synchronization
import Testing
@testable import PlowLatchUI

/// The main window runs the model's reads and streams while it's open:
/// this Mac's status as it changes, the selected host's activity, each
/// place's reads (cancelled when the place changes); closing it ends them,
/// and a logout hands it to onboarding.
@MainActor
struct MainWindowTests {
    /// A latch whose status and audit streams the test writes to.
    final class LiveClient: LatchClient, @unchecked Sendable {
        let statuses = AsyncThrowingStream<LocalStatus, Error>.makeStream()
        let lines = AsyncThrowingStream<AuditLine, Error>.makeStream()
        nonisolated func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { statuses.stream }
        nonisolated func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> { lines.stream }
    }

    private func app(_ client: some LatchClient, progress: PlowLatchUI.Progress = PlowLatchUI.Progress(passed: [.welcome: .continued], finished: true)) throws -> AppModel {
        let store = ProgressStore(memory: .init(), stateDir: "/s")
        store.save(progress)
        let app = AppModel(client: client, store: store)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return app
    }

    /// The window, off every display; closed when the test ends.
    private func open(_ app: AppModel) -> MainWindowController {
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700),
                               styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let controller = MainWindowController(app: app, window: window, defaults: nil, presenter: SheetRecorder())
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        return controller
    }

    // The toolbar holds the same items on every place: nothing is added,
    // removed or moved by navigating.
    @Test func theToolbarIsTheSameOnEveryPlace() async throws {
        let app = try app(LiveClient())
        let controller = open(app)
        defer { controller.close() }
        let fixed: [NSToolbarItem.Identifier] = [.toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, .update,
                                                 .inspectorTrackingSeparator, .flexibleSpace, .toggleInspector]
        for place in [Place.host(.home), .host(.activity), .account(.agents), .host(.vault), .account(.connections), .host(.settings)] {
            app.place = place
            try await Task.sleep(for: .milliseconds(20))
            #expect(controller.window?.toolbar?.items.map(\.itemIdentifier) == fixed, "\(place)")
        }
    }

    // The update pill shows on every place while a newer build waits, and
    // goes once this window has installed it.
    @Test func theUpdatePillShowsWhileAnUpdateWaits() async throws {
        let app = try app(LiveClient())
        let controller = open(app)
        defer { controller.close() }
        let pill = try #require(controller.window?.toolbar?.items.first { $0.itemIdentifier == .update })
        try await Task.sleep(for: .milliseconds(20))
        #expect(pill.isHidden)
        app.stage(devices: nil, update: try Fixtures.decode(Fixtures.updateAvailable))
        for place in [Place.host(.home), .account(.agents), .host(.vault)] {
            app.place = place
            try await Task.sleep(for: .milliseconds(20))
            #expect(!pill.isHidden, "\(place)")
        }
        app.stage(installingUpdate: false, installed: true)
        try await Task.sleep(for: .milliseconds(20))
        #expect(pill.isHidden)
    }

    // The inspector's toggle works only where there's an inspector, Activity
    // and Vault; elsewhere it stays in place, greyed, as Finder's Show
    // Preview does where there's no preview.
    @Test func theInspectorToggleIsGreyedWhereThereIsNoInspector() async throws {
        let app = try app(LiveClient())
        let controller = open(app)
        defer { controller.close() }
        let toolbar = try #require(controller.window?.toolbar)
        for (place, enabled) in [(Place.host(.activity), true), (.host(.home), false), (.host(.vault), true), (.host(.rules), false),
                                 (.host(.settings), false), (.account(.agents), false), (.account(.connections), false)] {
            app.place = place
            try await Task.sleep(for: .milliseconds(20))
            toolbar.validateVisibleItems()
            #expect(toolbar.items.first { $0.itemIdentifier == .toggleInspector }?.isEnabled == enabled, "\(place)")
        }
    }

    // The toolbar's toggles show only their icons, and their help tag and
    // VoiceOver name say what a click does now: Hide while open, Show
    // while closed, as the menu's do.
    @Test func theTogglesAreNamedForWhatTheyDo() async throws {
        let app = try app(LiveClient())
        app.place = .host(.activity)
        let controller = open(app)
        defer { controller.close() }
        let toolbar = try #require(controller.window?.toolbar)
        let split = try #require(controller.window?.contentViewController as? NSSplitViewController)
        let frame = try #require(controller.window?.contentView?.superview)
        // What VoiceOver reads: the toolbar's buttons in the window's
        // accessibility tree, not the items' own views.
        func spoken() -> [String] {
            var stack: [Any] = [frame], found: [String] = []
            while let e = stack.popLast() {
                guard let e = e as? NSAccessibilityProtocol & NSObject else { continue }
                if e.accessibilityRole() == .toolbar {
                    found += (e.accessibilityChildren() ?? []).compactMap { $0 as? NSAccessibilityProtocol }
                        .filter { $0.accessibilityRole() == .button }.map { $0.accessibilityLabel() ?? "" }
                } else {
                    stack += e.accessibilityChildren() ?? []
                }
            }
            return found
        }
        func names() -> [String] {
            toolbar.validateVisibleItems()
            let shown = [NSToolbarItem.Identifier.toggleSidebar, .toggleInspector].compactMap { id in
                toolbar.items.first { $0.itemIdentifier == id }.map { "\($0.title)|\($0.toolTip ?? "")" }
            }
            return zip(shown, spoken()).map { "\($0)|\($1)" }
        }
        #expect(names() == ["|Hide Sidebar|Hide Sidebar", "|Hide Inspector|Hide Inspector"])
        split.toggleSidebar(nil)
        app.inspecting.remove(.activity)
        await waitUntil { names() == ["|Show Sidebar|Show Sidebar", "|Show Inspector|Show Inspector"] }
        #expect(names() == ["|Show Sidebar|Show Sidebar", "|Show Inspector|Show Inspector"])
    }

    @Test func aChangeOfPlaceCancelsTheLastPlacesReads() async throws {
        let client = CountingClient()
        let gate = Gate()
        for command in ["rules show", "vault list", "status --host"] { client.held[command] = gate }
        let app = try app(client)
        app.showHost("pi-kitchen")
        let controller = open(app)
        defer { controller.close() }
        // Home on pi-kitchen reads its rules, vault and status (all held).
        await waitUntil { ["rules show", "vault list", "status --host"].allSatisfy(client.calls.contains) }
        app.place = .account(.connections)
        await waitUntil { client.calls.contains("devices") }
        gate.open()
        try await Task.sleep(for: .milliseconds(100))
        #expect(app.summaries["pi-kitchen"]?.rules == nil)
        #expect(app.summaries["pi-kitchen"]?.vault == nil)
        #expect(app.summaries["pi-kitchen"]?.status == nil)
    }

    @Test func aPlaceLeftAloneFinishesItsReads() async throws {
        let client = CountingClient()
        let app = try app(client)
        app.showHost("pi-kitchen")
        let controller = open(app)
        defer { controller.close() }
        await waitUntil { AppModel.held(app.summaries["pi-kitchen"]?.vault) && AppModel.held(app.summaries["pi-kitchen"]?.rules) }
        #expect(AppModel.held(app.summaries["pi-kitchen"]?.vault))
        #expect(AppModel.held(app.summaries["pi-kitchen"]?.rules))
    }

    @Test func theWindowFollowsStatusAndActivityLive() async throws {
        let client = LiveClient()
        let app = try app(client)
        app.place = .host(.activity)
        let controller = open(app)
        defer { controller.close() }
        let feed = try #require(app.activityFeed)
        await waitUntil { feed.running }
        client.lines.continuation.yield(.caughtUp(cursor: String(format: "%064d", 0), hasOlder: false))
        for n in 1...3 {
            client.lines.continuation.yield(try LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line(n, op: "exec").utf8)))
        }
        await waitUntil { feed.records.count == 3 }
        // This Mac stops: the window's title says so as latch does.
        let serving = controller.window?.subtitle
        client.statuses.continuation.yield(try Fixtures.decode(Fixtures.stopped))
        await waitUntil { app.status?.thisMachine.hosting.state == "stopped" }
        await waitUntil { controller.window?.subtitle != serving }
    }

    @Test func closingTheWindowEndsItsFeedsAtOnce() async throws {
        let client = LiveClient()
        let app = try app(client)
        app.place = .host(.home)
        let controller = open(app)
        let feed = try #require(app.activityFeed)
        await waitUntil { feed.running }
        controller.close()
        #expect(!feed.running)
        // Nothing the window ran comes back: no feed starts on a change.
        app.place = .host(.activity)
        try await Task.sleep(for: .milliseconds(50))
        #expect(!feed.running)
    }

    @Test func aLogoutHandsTheWindowToOnboarding() async throws {
        let client = LiveClient()
        let app = try app(client)
        let controller = open(app)
        defer { controller.close() }
        // The window's own actions, once its views have set them, recorded
        // instead: a test opens no window.
        await waitUntil { app.openWindow != nil }
        let opened = Mutex<[String]>([]), dismissed = Mutex<[String]>([])
        app.openWindow = { id in opened.withLock { $0.append(id) } }
        app.dismissWindow = { id in dismissed.withLock { $0.append(id) } }
        client.statuses.continuation.yield(try Fixtures.decode(Fixtures.loggedOut))
        await waitUntil { app.onboarding != nil }
        #expect(app.onboarding?.step == .login)
        #expect(opened.withLock { $0 } == [AppModel.onboardingWindow])
        #expect(dismissed.withLock { $0 } == [AppModel.mainWindow])
    }

    // A closed inspector reads nothing: selecting a Vault item with it
    // closed asks latch for nothing until it opens.
    @Test func aClosedVaultInspectorReadsNoItem() async throws {
        let client = VaultClient()
        let app = try app(client)
        app.place = .host(.vault)
        app.inspecting.remove(.vault)
        let controller = open(app)
        defer { controller.close() }
        await waitUntil { AppModel.held(app.summaries["studio"]?.vault) }
        let split = try #require(controller.window?.contentViewController as? NSSplitViewController)
        #expect(split.splitViewItems[2].isCollapsed)
        app.selectedVaultItem = OnHost(host: "studio", id: "v1")
        try await Task.sleep(for: .milliseconds(200))
        #expect(app.vaultShows["studio"]?["v1"] == nil)
        app.inspecting.insert(.vault)
        await waitUntil { !split.splitViewItems[2].isCollapsed }
        await waitUntil { app.vaultShows["studio"]?["v1"] != nil }
    }

    // At the window's narrowest, opening the inspector grows the window by
    // the room it needs, as Finder's preview does; closing it leaves the
    // window as it is.
    @Test func openingTheInspectorAtTheNarrowestGrowsTheWindow() async throws {
        let app = try app(LiveClient())
        app.place = .host(.activity)
        app.inspecting.remove(.activity)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        let split = try #require(window.contentViewController as? MainSplitController)
        window.setFrame(CGRect(x: -4000, y: -4000, width: 100, height: 600), display: true)
        window.layoutIfNeeded()
        let narrowest = window.frame.width
        // The bar over the screen, as wide as before: the screen keeps its width.
        let bar = try #require(searchFields(in: window.contentView!.superview!).first?.superview)
        let content = bar.frame.width, inspector = split.splitViewItems[2]
        split.toggleInspector(nil)
        await waitUntil { !inspector.isCollapsed && window.frame.width == narrowest + inspector.minimumThickness }
        window.layoutIfNeeded()
        #expect(bar.frame.width == content)
        #expect(!NSScreen.screens.contains { $0.frame.intersects(window.frame) })
        split.toggleInspector(nil)
        await waitUntil { inspector.isCollapsed }
        #expect(window.frame.width == narrowest + inspector.minimumThickness)
    }

    // At the window's narrowest, beside Vault's buttons, the bar's search
    // field still has room for its prompt and what's typed.
    @Test func theSearchFieldIsUsableAtTheNarrowest() async throws {
        let app = try app(VaultClient())
        app.place = .host(.vault)
        app.inspecting.remove(.vault)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        window.setFrame(CGRect(x: -4000, y: -4000, width: 100, height: 600), display: true)
        let frame = try #require(window.contentView?.superview)
        let field = try #require(searchFields(in: frame).first)
        await waitUntil { buttons(in: frame).contains { $0.accessibilityLabel() == "Import…" } }
        window.layoutIfNeeded()
        #expect(field.frame.width >= 120, "\(field.frame.width)")
        let importButton = try #require(buttons(in: frame).first { $0.accessibilityLabel() == "Import…" })
        #expect(importButton.convert(importButton.bounds, to: nil).maxX <= field.convert(field.bounds, to: nil).minX)
    }

    // Vault's New Item and Import… sit in the bar, enabled once its vault
    // is read; no other screen has buttons there.
    @Test func theBarHoldsTheScreensOwnButtons() async throws {
        let gate = Gate()
        let client = VaultClient()
        client.listGate = gate
        let app = try app(client)
        app.place = .host(.home)
        let controller = open(app)
        defer { controller.close() }
        let frame = try #require(controller.window?.contentView?.superview)
        func bar() -> [NSButton] { buttons(in: frame).filter { ["New Item", "Import…"].contains($0.accessibilityLabel() ?? "") && !$0.isHiddenOrHasHiddenAncestor } }
        try await Task.sleep(for: .milliseconds(50))
        #expect(bar().isEmpty)
        app.place = .host(.vault)
        await waitUntil { bar().count == 2 }
        #expect(bar().allSatisfy { !$0.isEnabled })
        gate.open()
        await waitUntil { bar().allSatisfy(\.isEnabled) }
        for place in [Place.host(.activity), .account(.agents)] {
            app.place = place
            await waitUntil { bar().isEmpty }
        }
        // Back to a vault already read: enabled as it shows.
        app.place = .host(.vault)
        await waitUntil { bar().count == 2 && bar().allSatisfy(\.isEnabled) }
    }

    // The place, host and inspectors are the window's to keep: a new window
    // on the same defaults comes back to them.
    @Test func aNewWindowComesBackWhereTheLastWasLeft() async throws {
        let defaults = try #require(UserDefaults(suiteName: "MainWindowTests-\(UUID().uuidString)"))
        let client = LiveClient()
        let first = try app(client)
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700), styleMask: [.titled, .closable],
                               backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let controller = MainWindowController(app: first, window: window, defaults: defaults, presenter: SheetRecorder())
        first.showHost("pi-kitchen")
        first.place = .host(.vault)
        first.inspecting.remove(.vault)
        await waitUntil { defaults.string(forKey: "MainWindowPlace") == "vault" && defaults.stringArray(forKey: "MainWindowInspecting") == ["activity"] }
        controller.close()

        let second = try app(client)
        let again = MainWindowController(app: second, window: Offscreen(contentRect: .zero, styleMask: [.titled], backing: .buffered, defer: false),
                                         defaults: defaults, presenter: SheetRecorder())
        defer { again.close() }
        #expect(second.route == .host(.vault, second.hosts.first { $0.name == "pi-kitchen" }))
        #expect(second.inspecting == [.activity])
    }
}

/// The menu bar's commands reach the screen showing along the window's
/// responder chain, and are enabled where it can run them.
extension MainWindowTests {
    private func enabled(_ controller: MainWindowController) -> Set<String> {
        let actions = [MenuBridge.newItem, MenuBridge.importItems, MenuBridge.save, MenuBridge.find, MenuBridge.toggleInspector]
        return Set(actions.filter { MenuBridge.state($0, in: controller.window).enabled }.map(NSStringFromSelector))
    }

    @Test func theMenuFollowsTheScreenShowing() async throws {
        let app = try app(VaultClient())
        let controller = open(app)
        defer { controller.close() }
        await waitUntil { AppModel.held(app.summaries["studio"]?.vault) }
        let expected: [(Place, Set<String>)] = [
            (.host(.home), []),
            (.host(.activity), ["findInScreen:", "toggleInspector:"]),
            (.host(.rules), []),
            (.host(.vault), ["newDocument:", "importItems:", "findInScreen:", "toggleInspector:"]),
            (.host(.settings), []),
            (.account(.agents), []),
            (.account(.connections), []),
        ]
        for (place, actions) in expected {
            app.place = place
            await waitUntil { enabled(controller) == actions }
            #expect(enabled(controller) == actions, "\(place)")
        }
        // The sidebar's and inspector's titles say what they will do.
        #expect(MenuBridge.state(MenuBridge.toggleSidebar, in: controller.window) == .init(enabled: true, title: "Hide Sidebar"))
        app.place = .host(.activity)
        await waitUntil { MenuBridge.state(MenuBridge.toggleInspector, in: controller.window).title == "Hide Inspector" }
        app.inspecting.remove(.activity)
        await waitUntil { MenuBridge.state(MenuBridge.toggleInspector, in: controller.window).title == "Show Inspector" }
    }

    // Find is the screen's: straight from Activity or Vault to an account
    // page it's off and focuses nothing, though the bar there is hidden
    // whole with its search field as it was.
    @Test func findIsOffOnAnAccountPageReachedStraightFromASearch() async throws {
        let app = try app(VaultClient())
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        for (from, to) in [(Place.host(.activity), Place.account(.agents)), (.host(.vault), .account(.connections))] {
            app.place = from
            await waitUntil { MenuBridge.state(MenuBridge.find, in: window).enabled }
            app.place = to
            try await Task.sleep(for: .milliseconds(100))
            #expect(!MenuBridge.state(MenuBridge.find, in: window).enabled, "\(from) → \(to)")
            window.makeFirstResponder(nil)
            controller.findInScreen(nil)
            #expect(window.firstResponder === window, "\(from) → \(to)")
        }
    }

    // ⌘N on the Vault opens New Item for its host, whatever has the focus.
    @Test func newItemActsOnTheVault() async throws {
        let app = try app(VaultClient())
        app.place = .host(.vault)
        let controller = open(app)
        defer { controller.close() }
        controller.window?.makeFirstResponder(nil)
        await waitUntil { MenuBridge.state(MenuBridge.newItem, in: controller.window).enabled }
        MenuBridge.send(MenuBridge.newItem, in: controller.window)
        guard case let .vaultItem(editor)? = app.sheet else { Issue.record("no New Item: \(String(describing: app.sheet))"); return }
        #expect(editor.host.name == "studio")
        app.sheet = nil
    }

    // ⌘S saves the focused editor's draft only, and only while an edited
    // editor has the focus.
    @Test func saveIsTheFocusedRulesEditors() async throws {
        let client = RulesClient()
        let app = try app(client)
        app.place = .host(.rules)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        let every = AppModel.RulesKey(host: "studio", agent: nil)
        let scout = AppModel.RulesKey(host: "studio", agent: "ba3c0b3ad1eba7a9a1eb04b67c9db0dd")
        await waitUntil { textViews(in: window.contentView!).count >= 2 && app.savedRules(scout) != nil }
        app.typeRules("Agents may read ~/Projects.", at: every, from: app.savedRules(every))
        app.typeRules("scout may run make test.", at: scout, from: app.savedRules(scout))
        #expect(!MenuBridge.state(MenuBridge.save, in: window).enabled)
        await waitUntil { textViews(in: window.contentView!).contains { $0.string == "scout may run make test." } }
        let scoutEditor = try #require(textViews(in: window.contentView!).first { $0.string == "scout may run make test." })
        window.makeFirstResponder(scoutEditor)
        await waitUntil { MenuBridge.state(MenuBridge.save, in: window).enabled }
        MenuBridge.send(MenuBridge.save, in: window)
        await waitUntil { !client.sets.isEmpty }
        #expect(client.sets.map(\.agent) == [scout.agent])
        #expect(app.rulesDrafts[every] != nil)
        // The focus elsewhere: nothing to save.
        window.makeFirstResponder(nil)
        await waitUntil { !MenuBridge.state(MenuBridge.save, in: window).enabled }
    }

    // Agents › Rules lands in that agent's editor, with the keyboard focus.
    @Test func anAgentsRulesTakeTheFocus() async throws {
        let app = try app(RulesClient())
        app.place = .account(.agents)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        let studio = try #require(app.host)
        await waitUntil { app.agents != nil }
        let scout = AppModel.RulesKey(host: "studio", agent: "ba3c0b3ad1eba7a9a1eb04b67c9db0dd")
        app.openRules(studio, agent: try #require(scout.agent))
        await waitUntil { app.rulesFocus == nil && window.firstResponder is NSTextView }
        let editor = try #require(window.firstResponder as? NSTextView)
        let saved = try #require(app.savedRules(scout))
        #expect(editor.string == saved.text)
    }

    // Tab from nothing focused goes into the window: to the sidebar or the
    // bar's search, whichever AppKit puts first by where they sit. The rest
    // of the round, which needs keyboard navigation on, is the VM's to show.
    @Test func tabGoesIntoTheWindow() async throws {
        let app = try app(VaultClient())
        app.place = .host(.home)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        app.place = .host(.vault)
        await waitUntil { AppModel.held(app.summaries["studio"]?.vault) }
        window.makeFirstResponder(nil)
        window.selectNextKeyView(nil)
        let reached = window.firstResponder
        #expect(reached is NSOutlineView || ((reached as? NSTextView)?.delegate as? NSSearchField)?.placeholderString == "Search items",
                "\(String(describing: reached))")
    }

    // An account page hides the bar whole: Tab never lands in it, nor does
    // VoiceOver find its tabs or the search field the last host screen left.
    @Test func aHiddenBarIsOutOfTheKeyLoopAndAccessibility() async throws {
        let app = try app(LiveClient())
        app.place = .host(.activity)
        let controller = open(app)
        defer { controller.close() }
        let window = try #require(controller.window)
        let bar = try #require(views(in: window.contentView!).first { $0 is ContentBar })
        // A responder or accessibility element in the bar: a view, the
        // field editor of its search field, or a cell or segment of one.
        func inBar(_ object: Any?) -> Bool {
            var object = object
            for _ in 0..<8 {
                if let editor = object as? NSTextView, editor.isFieldEditor { object = editor.delegate }
                if let view = object as? NSView { return view.isDescendant(of: bar) }
                object = (object as? NSCell)?.controlView ?? (object as? NSAccessibilityProtocol)?.accessibilityParent()
            }
            return false
        }
        func reachable(_ element: Any, depth: Int = 0) -> [Any] {
            guard depth < 40, let element = element as? NSAccessibilityProtocol else { return [] }
            return [element] + (element.accessibilityChildren() ?? []).flatMap { reachable($0, depth: depth + 1) }
        }
        await waitUntil { searchFields(in: bar).first?.placeholderString == "Search requests" }
        #expect(reachable(window).contains(where: inBar))
        for page in [AccountPage.agents, .connections] {
            app.place = .account(page)
            await waitUntil { window.title == page.title }
            window.recalculateKeyViewLoop()
            window.makeFirstResponder(nil)
            var reached: [Any?] = []
            for _ in 0..<40 {
                window.selectNextKeyView(nil)
                reached.append(window.firstResponder)
            }
            #expect(!reached.contains(where: inBar), "\(page): \(reached.filter(inBar))")
            #expect(!reachable(window).contains(where: inBar), "\(page)")
        }
    }

    // The bar's search field holds each screen's own search.
    @Test func eachScreenKeepsItsOwnSearch() async throws {
        let app = try app(LiveClient())
        app.place = .host(.activity)
        let controller = open(app)
        defer { controller.close() }
        let field = try #require(searchFields(in: controller.window!.contentView!.superview!).first)
        func type(_ text: String) {
            field.stringValue = text
            NSApp.sendAction(field.action!, to: field.target, from: field)
        }
        type("exec")
        app.place = .host(.vault)
        await waitUntil { field.placeholderString == "Search items" }
        #expect(field.stringValue == "")
        type("card")
        app.place = .host(.rules)
        await waitUntil { field.isHidden }
        app.place = .host(.activity)
        await waitUntil { field.placeholderString == "Search requests" }
        #expect(field.stringValue == "exec")
        #expect(app.activitySearch == "exec" && app.vaultSearch == "card")
    }
}

@MainActor
private func searchFields(in view: NSView) -> [NSSearchField] {
    (view as? NSSearchField).map { [$0] } ?? view.subviews.flatMap(searchFields)
}

@MainActor
private func views(in view: NSView) -> [NSView] {
    [view] + view.subviews.flatMap(views)
}

@MainActor
func buttons(in view: NSView) -> [NSButton] {
    (view as? NSButton).map { [$0] } ?? view.subviews.flatMap(buttons)
}

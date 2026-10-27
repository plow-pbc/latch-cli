import AppKit
import SwiftUI

/// The main window, in AppKit: its toolbar, fixed at launch; a split view
/// of the sidebar, the place showing (under a bar of the host's screen tabs
/// and search), and the inspector on screens that have one. Each screen,
/// the sidebar and the inspector's content are SwiftUI, on the one
/// `AppModel`. While it's open it runs what the model reads and follows
/// for the window: this Mac's status, Plow's hosts, the reads of the place
/// showing (cancelled when it changes) and the selected host's activity;
/// closing it ends them. It presents the model's sheets and questions.
final class MainWindowController: NSWindowController, NSWindowDelegate, NSToolbarDelegate, NSUserInterfaceValidations {
    /// The open main window; the app has at most one.
    private(set) static var shown: MainWindowController?

    /// Opens the main window, or brings it forward.
    static func open(_ app: AppModel) {
        if let shown { return shown.showWindow(nil) }
        let controller = MainWindowController(app: app)
        shown = controller
        controller.showWindow(nil)
        // Nothing has the keyboard focus at first, not the bar's search field
        // (the first in the key view loop).
        controller.window?.makeFirstResponder(nil)
    }

    static func dismiss() {
        shown?.close()
    }

    let app: AppModel
    private let split: MainSplitController
    private let screens: ScreenController
    private let bar = ContentBar()
    private let barItem = NSSplitViewItemAccessoryViewController()
    private let inspectorShows = WindowInspector.Shown()
    private let inspectorItem: NSSplitViewItem
    let sheets: WindowSheets
    /// Where the window keeps its place, host and inspectors; nil for one
    /// that keeps nothing (a still, a test).
    private let defaults: UserDefaults?

    /// What the window runs for the model while it's open.
    private var tasks: [Task<Void, Never>] = []
    private var entered: AppModel.Entry?
    private var entering: Task<Void, Never>?
    private var following: ActivityFeed?
    private var activation: NSObjectProtocol?
    private var closed = false

    /// `window` is the one to build in, if not a new one (a still's, off
    /// every display); `presenter` shows its sheets (a test's answers them).
    init(app: AppModel, window: NSWindow? = nil, defaults: UserDefaults? = .standard, presenter: any SheetPresenting = AppKitSheets()) {
        self.app = app
        self.defaults = defaults
        split = MainSplitController(app: app)
        screens = ScreenController(app: app, autosave: defaults != nil && !app.still)
        sheets = WindowSheets(app: app, presenter: presenter)
        let own = window == nil
        let window = window ?? NSWindow(contentRect: CGRect(x: 0, y: 0, width: 1120, height: 700),
                                        styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                                        backing: .buffered, defer: false)
        // Where it was left, before anything is built on it.
        if let defaults, !app.still {
            app.restore(place: defaults.string(forKey: Self.placeKey) ?? "", host: defaults.string(forKey: Self.hostKey))
            if let inspecting = defaults.stringArray(forKey: Self.inspectingKey) {
                app.inspecting = Set(inspecting.compactMap(HostScreen.init(rawValue:)))
            }
        }

        let sidebar = NSSplitViewItem(sidebarWithViewController: Self.hosting(Sidebar().installWindowActions(app), app))
        sidebar.minimumThickness = 210
        sidebar.maximumThickness = 320
        let content = NSSplitViewItem(viewController: screens)
        content.automaticallyAdjustsSafeAreaInsets = true
        barItem.view = bar
        content.addTopAlignedAccessoryViewController(barItem)
        inspectorItem = NSSplitViewItem(inspectorWithViewController: Self.hosting(WindowInspector(shown: inspectorShows), app))
        inspectorItem.minimumThickness = 260
        inspectorItem.maximumThickness = 360
        [sidebar, content, inspectorItem].forEach(split.addSplitViewItem)

        super.init(window: window)
        window.delegate = self
        window.title = "Latch"
        window.toolbarStyle = .unified
        // The only window of its kind: no tab bar, nor its menu items.
        window.tabbingMode = .disallowed
        // Tab goes through the screen showing, made after the window was.
        window.autorecalculatesKeyViewLoop = true
        window.contentViewController = split
        window.contentMinSize = CGSize(width: 1040, height: 440)
        if own {
            // Its content sized it to fit: back to its default size, then
            // its frame and column widths as AppKit kept them.
            window.setContentSize(CGSize(width: 1120, height: 700))
            window.center()
            if defaults != nil, !app.still {
                window.setFrameAutosaveName("Main")
                split.splitView.autosaveName = "Main"
            }
        }
        let toolbar = NSToolbar(identifier: "Main")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        toolbar.allowsUserCustomization = false
        window.toolbar = toolbar

        bar.pick = { [app] screen in app.place = .host(screen) }
        bar.pageTarget = screens
        bar.search = { [app] text in
            switch app.place {
            case .host(.activity): app.activitySearch = text
            case .host(.vault): app.vaultSearch = text
            default: break
            }
        }
        if !app.still { begin() }
        follow()
    }

    required init?(coder: NSCoder) { nil }

    private static let placeKey = "MainWindowPlace", hostKey = "MainWindowHost", inspectingKey = "MainWindowInspecting"

    /// A SwiftUI view in the window: it sizes itself only by its minimum,
    /// and leaves the window's title and toolbar to the window.
    static func hosting(_ view: some View, _ app: AppModel) -> NSViewController {
        let controller = NSHostingController(rootView: view.environment(app))
        controller.sceneBridgingOptions = []
        controller.sizingOptions = [.minSize]
        return controller
    }

    // MARK: The window's life

    /// This Mac's status, then following it; Plow's hosts, which can be
    /// slow and don't hold the status back; and a read again of what can
    /// change while the app is elsewhere, each time it comes back.
    private func begin() {
        tasks.append(Task { [app] in
            await app.refreshStatus()
            app.leaveMainIfLoggedOut()
            await app.followStatus()
        })
        tasks.append(Task { [app] in await app.refreshAccount() })
        activation = NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil,
                                                            queue: .main) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, !self.closed else { return }
                self.tasks.append(Task { [app = self.app] in await app.activated() })
            }
        }
    }

    /// Everything the window ran stops: the reads, the status stream and
    /// every activity feed, now rather than after their grace.
    private func end() {
        closed = true
        activation.map(NotificationCenter.default.removeObserver)
        activation = nil
        tasks.forEach { $0.cancel() }
        tasks = []
        entering?.cancel()
        entering = nil
        following = nil
        app.endFeeds()
        // What was over the window goes with it, unanswered.
        if let window { sheets.end(over: window) }
        app.sheet = nil
        app.endGoogleSignIn()
    }

    func windowWillClose(_ notification: Notification) {
        end()
        if Self.shown === self { Self.shown = nil }
    }

    /// Follows the model: what shows, the title, the bar, the inspector,
    /// and what the window reads and follows for the place showing.
    private func follow() {
        withObservationTracking { apply() } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, !self.closed else { return }
                self.follow()
            }
        }
    }

    private func apply() {
        guard let window else { return }
        let route = app.route
        screens.show(app.place)

        let title: String, subtitle: String
        switch route {
        case let .account(page): (title, subtitle) = (page.title, "Your account")
        case let .host(_, host): (title, subtitle) = (host?.name ?? "No host", host?.subtitle(local: app.status) ?? "")
        }
        if window.title != title { window.title = title }
        if window.subtitle != subtitle { window.subtitle = subtitle }

        let onHost = if case .host = app.place { true } else { false }
        if barItem.isHidden == onHost { Self.unanimated { barItem.isHidden = !onHost } }

        // The inspector on the screens that have one, as each was left;
        // shown or hidden by a change of place at once, not slid.
        let inspecting = if case let .host(screen) = app.place { HostScreen.inspected.contains(screen) && app.inspecting.contains(screen) }
            else { false }
        if inspectorItem.isCollapsed == inspecting {
            Self.unanimated { inspectorItem.isCollapsed = !inspecting }
            // Tab reaches what the inspector shows only once the window
            // has counted it in again: AppKit leaves out a collapsed one.
            window.recalculateKeyViewLoop()
        }
        if case let .host(screen, _) = route, HostScreen.inspected.contains(screen), inspectorShows.route != route { inspectorShows.route = route }

        if !app.still {
            // Each place's reads, for the route that keyed them: a change of
            // what they depend on cancels the last one's.
            let entry = app.entry
            if entry != entered {
                entered = entry
                entering?.cancel()
                entering = Task { [app] in await app.enter(route) }
            }
            // Home and Activity share the selected host's feed: going between
            // them keeps it running.
            let feed = app.activityFeed
            if feed !== following {
                feed?.start()
                following?.stop()
                following = feed
            }
        }
        if let defaults, !app.still {
            defaults.set(app.place.rawValue, forKey: Self.placeKey)
            defaults.set(app.chosenHost, forKey: Self.hostKey)
            defaults.set(app.inspecting.map(\.rawValue).sorted(), forKey: Self.inspectingKey)
        }
        // The host's screen tabs, the screen's own buttons and its search;
        // gone, not greyed, on a page of the account's. Last: what lays the
        // window out above can draw the screen, which sets its buttons'
        // actions, and a change made during this pass isn't followed.
        if case let .host(screen) = app.place {
            let search: (String, String)? = switch screen {
            case .activity: ("Search requests", app.activitySearch)
            case .vault: ("Search items", app.vaultSearch)
            default: nil
            }
            let actions = screens.actions
            let pages: [(PageButton, Bool)] = switch screen {
            case .vault: [(.newItem, actions?.newItem != nil), (.importItems, actions?.importItems != nil)]
            default: []
            }
            bar.show(screen, pages: pages, search: search)
        }
        sheets.update(over: split)
        window.toolbar?.validateVisibleItems()
        MenuBridge.shared.setNeedsRefresh()
    }

    /// Find (⌘F): into the bar's search field, on a screen that has one.
    @objc func findInScreen(_ sender: Any?) {
        guard searches else { return }
        bar.focusSearch()
    }

    func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        item.action == #selector(findInScreen(_:)) ? searches : true
    }

    /// The place showing has a search: Activity's requests, Vault's items.
    /// The place's, not the bar's field: an account page hides the bar
    /// whole, leaving the field as the last host screen had it.
    private var searches: Bool { app.place == .host(.activity) || app.place == .host(.vault) }

    /// The screen's commands answer from anywhere in the window, as a
    /// document's do: New Item works with the focus in the sidebar, and
    /// Save asks the screen, which has one only for a focused editor.
    override func supplementalTarget(forAction action: Selector, sender: Any?) -> Any? {
        screens.responds(to: action) ? screens : super.supplementalTarget(forAction: action, sender: sender)
    }

    private static func unanimated(_ change: () -> Void) {
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0
            context.allowsImplicitAnimation = false
            change()
        }
    }

    // MARK: The toolbar: the same items on every place

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        // The second flexible space keeps the inspector's toggle at the
        // window's trailing edge whether the inspector is open or not.
        [.toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, .inspectorTrackingSeparator, .flexibleSpace, .toggleInspector]
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        toolbarDefaultItemIdentifiers(toolbar)
    }

    /// Every item is the system's; the toolbar asks for them all the same,
    /// and shows none without this answer.
    func toolbar(_ toolbar: NSToolbar, itemForItemIdentifier id: NSToolbarItem.Identifier,
                 willBeInsertedIntoToolbar flag: Bool) -> NSToolbarItem? {
        nil
    }
}

/// The split view: the inspector's toggle opens and closes it on the
/// screens that have one, and the model keeps which are open.
final class MainSplitController: NSSplitViewController, NSToolbarItemValidation {
    let app: AppModel

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { nil }

    private var inspected: HostScreen? {
        guard case let .host(screen) = app.place, HostScreen.inspected.contains(screen) else { return nil }
        return screen
    }

    override func toggleInspector(_ sender: Any?) {
        guard let screen = inspected else { return }
        super.toggleInspector(sender)
        view.window?.recalculateKeyViewLoop()
        if app.inspecting.contains(screen) { app.inspecting.remove(screen) } else { app.inspecting.insert(screen) }
    }

    override func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        guard item.action == #selector(toggleInspector(_:)) else { return super.validateUserInterfaceItem(item) }
        let open = inspected.map(app.inspecting.contains) ?? false
        (item as? NSMenuItem)?.title = open ? "Hide Inspector" : "Show Inspector"
        return inspected != nil
    }

    /// What the toolbar's toggle asks, as the menu's asks the method above:
    /// greyed, not hidden, where there's no inspector, as Finder greys Show
    /// Preview where there's no preview.
    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        validateUserInterfaceItem(item)
    }
}

/// The content column: the place showing, made as it's shown and released
/// when another replaces it. What must outlive it is the model's.
final class ScreenController: NSViewController, NSUserInterfaceValidations {
    let app: AppModel
    /// Screens keep what AppKit keeps of them (Activity's columns and sort)
    /// across launches; not a still's or a test's.
    let autosave: Bool
    private var shown: (place: Place, controller: NSViewController, actions: ScreenActions)?
    /// What the screen showing can run.
    var actions: ScreenActions? { shown?.actions }

    init(app: AppModel, autosave: Bool) {
        self.app = app
        self.autosave = autosave
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { nil }

    override func loadView() { view = NSView() }

    func show(_ place: Place) {
        guard shown?.place != place else { return }
        let actions = ScreenActions()
        // Activity's table is AppKit's; every other screen is SwiftUI.
        let controller = place == .host(.activity) ? ActivityController(app: app, autosaveName: autosave ? "Activity" : nil)
            : MainWindowController.hosting(Self.screen(place).environment(actions), app)
        addChild(controller)
        controller.view.frame = view.bounds
        controller.view.autoresizingMask = [.width, .height]
        view.addSubview(controller.view)
        shown?.controller.view.removeFromSuperview()
        shown?.controller.removeFromParent()
        shown = (place, controller, actions)
        MenuBridge.shared.setNeedsRefresh()
    }

    // MARK: The screen's commands, from the menu bar

    @objc func newDocument(_ sender: Any?) { shown?.actions.newItem?() }
    @objc func importItems(_ sender: Any?) { shown?.actions.importItems?() }
    @objc func saveDocument(_ sender: Any?) { shown?.actions.save?() }

    func validateUserInterfaceItem(_ item: NSValidatedUserInterfaceItem) -> Bool {
        let actions = shown?.actions
        switch item.action {
        case #selector(newDocument(_:)): return actions?.newItem != nil
        case #selector(importItems(_:)): return actions?.importItems != nil
        case #selector(saveDocument(_:)): return actions?.save != nil
        default: return true
        }
    }

    @ViewBuilder private static func screen(_ place: Place) -> some View {
        switch place {
        case .account(.agents): AgentsView()
        case .account(.connections): ConnectionsView()
        case .host(.home): HomeView()
        case .host(.activity): EmptyView()
        case .host(.rules): HostRulesView()
        case .host(.vault): VaultView()
        case .host(.settings): HostSettingsView()
        }
    }
}

/// The host's screen tabs. Like a toolbar control, a click leaves the
/// keyboard focus where it was (a segmented control would take it), unless
/// keyboard navigation is on: then Tab reaches them, as any control.
final class ScreenTabs: NSSegmentedControl {
    override var acceptsFirstResponder: Bool { NSApp.isFullKeyboardAccessEnabled && super.acceptsFirstResponder }
}

/// A screen's own button in the bar: the command its File menu item
/// sends, to the same screen.
struct PageButton {
    var title: String
    var symbol: String
    var help: String
    var action: Selector

    static let newItem = PageButton(title: "New Item", symbol: "plus", help: "Add a login, card, identity or note",
                                    action: MenuBridge.newItem)
    static let importItems = PageButton(title: "Import…", symbol: "square.and.arrow.down",
                                        help: "Import another password manager's export", action: MenuBridge.importItems)
    /// Vault's, the most any screen has.
    static let vault = [newItem, importItems]
}

/// The bar over a host's screen: its screen tabs, the screen's own
/// buttons (Vault's New Item and Import…), and the search of the screen
/// showing, on Activity (its requests) and Vault (its items).
final class ContentBar: NSView {
    var pick: (HostScreen) -> Void = { _ in }
    var search: (String) -> Void = { _ in }
    /// Where the screen's buttons send their commands: the content column.
    weak var pageTarget: AnyObject?
    private let tabs = ScreenTabs(labels: HostScreen.allCases.map(\.title), trackingMode: .selectOne, target: nil, action: nil)
    private let pages = NSStackView()
    private var shownPages: [Selector] = []
    private let field = NSSearchField()

    init() {
        super.init(frame: .zero)
        tabs.target = self
        tabs.action = #selector(picked)
        field.target = self
        field.action = #selector(searched)
        pages.orientation = .horizontal
        pages.spacing = 8
        pages.setHuggingPriority(.required, for: .horizontal)
        pages.setContentCompressionResistancePriority(.required, for: .horizontal)
        // As wide on every screen as Vault's buttons: the bar is as narrow
        // on each, so the window's narrowest doesn't change with the screen.
        let widest = NSStackView(views: PageButton.vault.map { Self.button($0, target: nil) })
        widest.spacing = pages.spacing
        for view in [tabs, pages, field] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        // 200 points where there's room, down to 120 before it would reach
        // the screen's buttons or the tabs: it gives way first to a sidebar
        // or inspector dragged wide in a narrow window.
        let preferred = field.widthAnchor.constraint(equalToConstant: 200)
        preferred.priority = .defaultLow
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: 40),
            tabs.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 20),
            tabs.centerYAnchor.constraint(equalTo: centerYAnchor),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -20),
            field.centerYAnchor.constraint(equalTo: centerYAnchor),
            pages.centerYAnchor.constraint(equalTo: centerYAnchor),
            pages.leadingAnchor.constraint(greaterThanOrEqualTo: tabs.trailingAnchor, constant: 12),
            pages.widthAnchor.constraint(greaterThanOrEqualToConstant: widest.fittingSize.width),
            field.leadingAnchor.constraint(equalTo: pages.trailingAnchor, constant: 8),
            field.widthAnchor.constraint(lessThanOrEqualToConstant: 200),
            field.widthAnchor.constraint(greaterThanOrEqualToConstant: 120),
            preferred,
        ])
    }

    required init?(coder: NSCoder) { nil }

    /// `pages`: the screen's buttons, each enabled while the screen can
    /// run it; `search`: the field's prompt and text, on a screen that has
    /// one.
    func show(_ screen: HostScreen, pages: [(PageButton, enabled: Bool)], search: (prompt: String, text: String)?) {
        let index = HostScreen.allCases.firstIndex(of: screen) ?? 0
        if tabs.selectedSegment != index { tabs.selectedSegment = index }
        if shownPages != pages.map(\.0.action) {
            shownPages = pages.map(\.0.action)
            self.pages.setViews(pages.map { Self.button($0.0, target: pageTarget) }, in: .leading)
        }
        for (button, page) in zip(self.pages.views.compactMap { $0 as? NSButton }, pages) where button.isEnabled != page.enabled {
            button.isEnabled = page.enabled
        }
        field.isHidden = search == nil
        guard let search else { return }
        if field.placeholderString != search.prompt { field.placeholderString = search.prompt }
        if field.stringValue != search.text { field.stringValue = search.text }
    }

    /// A screen's button: a symbol, as a toolbar button's, since the tabs
    /// and the search field leave no room for words beside them.
    private static func button(_ page: PageButton, target: AnyObject?) -> NSButton {
        let button = NSButton(image: NSImage(systemSymbolName: page.symbol, accessibilityDescription: page.title)!,
                              target: target, action: page.action)
        button.setAccessibilityLabel(page.title)
        button.toolTip = page.help
        return button
    }

    func focusSearch() {
        guard !field.isHidden else { return }
        window?.makeFirstResponder(field)
    }

    @objc private func picked() {
        pick(HostScreen.allCases[tabs.selectedSegment])
    }

    @objc private func searched() {
        search(field.stringValue)
    }
}

/// The window's inspector, for the last screen shown that has one. The
/// window sets it as it collapses or opens the inspector, so leaving
/// Activity or Vault never draws it empty before it's gone.
private struct WindowInspector: View {
    @Observable final class Shown {
        var route: Route?
    }

    let shown: Shown

    var body: some View {
        if let route = shown.route {
            Inspector(route: route)
        }
    }
}

/// The window's inspector: on Activity, the selected request; on Vault, the
/// selected item; each only while its host shows and the screen lists it.
struct Inspector: View {
    var route: Route
    @Environment(AppModel.self) private var app

    private var showing: Bool { app.place == .host(.vault) && app.inspecting.contains(.vault) }

    var body: some View {
        switch route {
        case let .host(.activity, host?):
            if let record = app.inspectedRecord(on: host) {
                RecordInspector(record: record, caller: record.callerLabel(hostKey: app.key(of: host), hostName: host.name,
                                                                          isThisMac: host.isThisMac), hostName: host.name,
                                host: host.isThisMac ? nil : host.name)
            } else {
                ContentUnavailableView("No Selection", systemImage: "doc.text.magnifyingglass",
                                       description: Text("Select a request to see who asked, why it was allowed or refused, and what ran."))
            }
        case let .host(.vault, host?):
            if let item = app.inspectedVaultItem(on: host) {
                VaultItemInspector(item: item, show: app.vaultShow(of: item, on: host), hostName: host.name,
                                   edit: { Task { await app.beginVaultEdit(item, on: host) } },
                                   remove: { app.ask(.removeVaultItem(item, on: host)) })
                    // Read only while it shows: a closed inspector, or one
                    // left behind another screen, stays built (AppKit keeps
                    // a collapsed item's view) but asks latch for nothing.
                    .task(id: "\(host.name)/\(item.item)@\(item.revision) \(showing)") {
                        guard !app.still, showing else { return }
                        await app.showVaultItem(item.item, on: host)
                    }
            } else {
                ContentUnavailableView("No Selection", systemImage: "lock.rectangle",
                                       description: Text("Select an item to see what it holds. Secret values are never shown."))
            }
        default:
            ContentUnavailableView("No Selection", systemImage: "sidebar.trailing")
        }
    }
}

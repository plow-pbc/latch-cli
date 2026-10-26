import AppKit
import SwiftUI

/// What the screen showing answers from the menu bar, as it changes: Save
/// for its focused rules editor, New Item and Import for a vault that can
/// change; nil where it has nothing to run. The screen sets them; the
/// window's content column (`ScreenController`) runs them.
@MainActor @Observable final class ScreenActions {
    var save: (() -> Void)? { didSet { MenuBridge.shared.setNeedsRefresh() } }
    var newItem: (() -> Void)? { didSet { MenuBridge.shared.setNeedsRefresh() } }
    var importItems: (() -> Void)? { didSet { MenuBridge.shared.setNeedsRefresh() } }
}

/// The menu bar's commands for the main window, sent along the key
/// window's responder chain as AppKit's own menu items are: each goes to
/// the first responder that answers it (the screen showing, the split
/// view, the window), and is enabled and titled as that responder
/// validates it. SwiftUI owns the menu bar and can't ask the chain when a
/// menu opens, so this asks after every event and whenever the window's
/// state changes, and the menu follows what it last found.
@MainActor @Observable final class MenuBridge {
    static let shared = MenuBridge()

    struct State: Equatable {
        var enabled = false
        var title: String?
    }

    static let newItem = #selector(ScreenController.newDocument(_:))
    static let importItems = #selector(ScreenController.importItems(_:))
    static let save = #selector(ScreenController.saveDocument(_:))
    static let find = #selector(MainWindowController.findInScreen(_:))
    static let toggleSidebar = #selector(NSSplitViewController.toggleSidebar(_:))
    static let toggleInspector = #selector(NSSplitViewController.toggleInspector(_:))
    private static let actions = [newItem, importItems, save, find, toggleSidebar, toggleInspector]

    private(set) var states: [Selector: State] = [:]
    @ObservationIgnored private var updates: NSObjectProtocol?
    @ObservationIgnored private var refreshing = false

    func state(_ action: Selector) -> State { states[action] ?? State() }

    /// Asks again after each event the app handles.
    func start() {
        guard updates == nil else { return }
        updates = NotificationCenter.default.addObserver(forName: NSApplication.didUpdateNotification, object: nil, queue: .main) { _ in
            MainActor.assumeIsolated { MenuBridge.shared.refresh() }
        }
        refresh()
    }

    /// Asks again once the change under way is done.
    func setNeedsRefresh() {
        guard !refreshing else { return }
        refreshing = true
        Task { @MainActor in
            refreshing = false
            refresh()
        }
    }

    func refresh() {
        let window = NSApp.keyWindow
        var new: [Selector: State] = [:]
        for action in Self.actions { new[action] = Self.state(action, in: window) }
        if new != states { states = new }
    }

    func send(_ action: Selector) {
        Self.send(action, in: NSApp.keyWindow)
        refresh()
    }

    /// The first in the window's responder chain to answer `action`:
    /// a responder that implements it, or the one it hands it to.
    static func target(_ action: Selector, in window: NSWindow?) -> AnyObject? {
        var responder = window?.firstResponder
        while let next = responder {
            if next.responds(to: action) { return next }
            if let target = next.supplementalTarget(forAction: action, sender: nil) { return target as AnyObject }
            responder = next.nextResponder
        }
        return nil
    }

    static func state(_ action: Selector, in window: NSWindow?) -> State {
        guard let target = target(action, in: window) else { return State() }
        let item = NSMenuItem(title: "", action: action, keyEquivalent: "")
        let enabled = if let target = target as? NSMenuItemValidation { target.validateMenuItem(item) }
            else if let target = target as? NSUserInterfaceValidations { target.validateUserInterfaceItem(item) }
            else { true }
        return State(enabled: enabled, title: item.title.isEmpty ? nil : item.title)
    }

    static func send(_ action: Selector, in window: NSWindow?) {
        guard state(action, in: window).enabled, let target = target(action, in: window) else { return }
        NSApp.sendAction(action, to: target, from: nil)
    }
}

/// File › New Item… (⌘N), Import… and Save (⌘S); Edit › Find… (⌘F); View ›
/// the sidebar and the inspector: each the main window's, by its responder
/// chain.
struct WindowCommands: Commands {
    let menu: MenuBridge

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            item("New Item…", MenuBridge.newItem, "n")
        }
        CommandGroup(replacing: .importExport) {
            item("Import…", MenuBridge.importItems)
        }
        CommandGroup(replacing: .saveItem) {
            item("Save", MenuBridge.save, "s")
        }
        CommandGroup(after: .textEditing) {
            item("Find…", MenuBridge.find, "f")
        }
        CommandGroup(replacing: .sidebar) {
            item("Show Sidebar", MenuBridge.toggleSidebar, "s", [.command, .control])
            item("Show Inspector", MenuBridge.toggleInspector, "i", [.command, .control])
        }
    }

    @ViewBuilder private func item(_ title: String, _ action: Selector, _ key: KeyEquivalent? = nil,
                                   _ modifiers: EventModifiers = .command) -> some View {
        let state = menu.state(action)
        let button = Button(state.title ?? title) { menu.send(action) }.disabled(!state.enabled)
        if let key { button.keyboardShortcut(key, modifiers: modifiers) } else { button }
    }
}

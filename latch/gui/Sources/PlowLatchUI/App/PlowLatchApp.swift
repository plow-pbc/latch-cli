import SwiftUI

/// latch's way in (src/gui.rs): called on the main thread with the path of
/// the bundle's own latch, the state folder, and which folder the launch
/// named (`LaunchedOn`); returns when the app quits.
@_cdecl("latch_gui_main")
nonisolated public func latchGUIMain(_ executable: UnsafePointer<CChar>, _ stateDir: UnsafePointer<CChar>, _ launched: Int32) -> Int32 {
    let latch = URL(filePath: String(cString: executable)), given = String(cString: stateDir)
    MainActor.assumeIsolated {
        let (state, isDefault) = LaunchedOn(rawValue: launched).map {
            StateFolder.choose(given: given, launched: $0, defaults: .standard)
        } ?? (given, false)
        let client = LiveLatchClient(executable: latch, stateDir: state)
        PlowLatchApp.model = AppModel(client: client, loginItemOffered: isDefault)
        #if LATCH_TEST_HOOKS
        TestHooks.install(PlowLatchApp.model)
        #endif
        PlowLatchApp.main()
    }
    return 0
}

/// Which state folder a launch named, as latch says (src/gui.rs `Launched`).
enum LaunchedOn: Int32 {
    /// A `--state-dir` other than latch's default.
    case other = 0
    /// No `--state-dir`.
    case unnamed = 1
    /// A `--state-dir` naming latch's default.
    case defaultFolder = 2
}

/// The state folder the app runs on. A launch with no arguments goes back
/// to the last `--state-dir` the app was opened with: macOS's Quit &
/// Reopen (after Full Disk Access is granted) relaunches with none. Naming
/// latch's default folder forgets it.
enum StateFolder {
    static let key = "StateDir"

    /// The folder, and whether it is latch's default.
    static func choose(given: String, launched: LaunchedOn, defaults: UserDefaults) -> (String, Bool) {
        switch launched {
        case .other:
            defaults.set(given, forKey: key)
            return (given, false)
        case .defaultFolder:
            defaults.removeObject(forKey: key)
            return (given, true)
        case .unnamed:
            if let remembered = defaults.string(forKey: key) { return (remembered, false) }
            return (given, true)
        }
    }
}

struct PlowLatchApp: App {
    /// Set by `latchGUIMain` before the app starts.
    static var model: AppModel!
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        let model = Self.model!
        // Which window opens follows what the app remembers, so neither
        // waits for latch; onboarding then asks latch where to resume.
        let finished = model.store.load().finished
        #if LATCH_TEST_HOOKS
        let windowed = !TestHooks.headless
        #else
        let windowed = true
        #endif
        Window("Welcome to Latch", id: AppModel.onboardingWindow) {
            OnboardingView()
                .environment(model)
                .installWindowActions(model)
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentSize)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(windowed && !finished ? .presented : .suppressed)

        Window("Latch", id: AppModel.mainWindow) {
            MainView()
                .environment(model)
                .installWindowActions(model)
        }
        .defaultSize(width: 1120, height: 700)
        .restorationBehavior(.disabled)
        .defaultLaunchBehavior(windowed && finished ? .presented : .suppressed)
        // One Save: the rules editor with the keyboard focus.
        .commands { RulesCommands() }

        Settings {
            AppSettingsView()
                .environment(model)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        LatchProcess.terminateAll()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

extension View {
    /// Hands the model SwiftUI's window actions, which only views get.
    func installWindowActions(_ model: AppModel) -> some View {
        modifier(WindowActions(model: model))
    }
}

private struct WindowActions: ViewModifier {
    var model: AppModel
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow

    func body(content: Content) -> some View {
        content
            .onAppear {
                model.openWindow = { openWindow(id: $0) }
                model.dismissWindow = { dismissWindow(id: $0) }
            }
            .background(WindowReader { FirstWindow.bringForward($0) })
    }
}

/// Opened by the installer from a terminal, the app's first window can
/// open behind it: once that window is on screen, it is brought forward.
private enum FirstWindow {
    static var done = false

    static func bringForward(_ window: NSWindow) {
        guard !done else { return }
        done = true
        NSApp.activate()
        window.makeKeyAndOrderFront(nil)
        window.orderFrontRegardless()
    }
}

/// Hands over the window a view is in, once it is in one.
private struct WindowReader: NSViewRepresentable {
    var found: (NSWindow) -> Void

    func makeNSView(context: Context) -> NSView { Probe(found: found) }
    func updateNSView(_ nsView: NSView, context: Context) {}

    private final class Probe: NSView {
        let found: (NSWindow) -> Void
        init(found: @escaping (NSWindow) -> Void) {
            self.found = found
            super.init(frame: .zero)
        }
        required init?(coder: NSCoder) { nil }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            DispatchQueue.main.async { [found] in found(window) }
        }
    }
}

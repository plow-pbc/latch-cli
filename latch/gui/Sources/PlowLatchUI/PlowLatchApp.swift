import SwiftUI

/// The latch the app runs for every answer, and the state folder it gives
/// each run.
struct Latch: Sendable {
    let executable: URL
    let stateDir: String
}

/// latch's way in (src/gui.rs): called on the main thread with the path of
/// the bundle's own latch and the state folder; returns when the app quits.
@_cdecl("latch_gui_main")
nonisolated public func latchGUIMain(_ executable: UnsafePointer<CChar>, _ stateDir: UnsafePointer<CChar>) -> Int32 {
    let latch = Latch(executable: URL(filePath: String(cString: executable)), stateDir: String(cString: stateDir))
    MainActor.assumeIsolated {
        PlowLatchApp.latch = latch
        PlowLatchApp.main()
    }
    return 0
}

struct PlowLatchApp: App {
    /// Set by `latchGUIMain` before the app starts.
    static var latch: Latch!

    var body: some Scene {
        Window("Latch", id: "main") {
            Color.clear.frame(minWidth: 480, minHeight: 320)
        }
        .defaultSize(width: 720, height: 480)
    }
}

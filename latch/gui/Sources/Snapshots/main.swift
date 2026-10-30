// Renders every onboarding screen and state, and the main window, light and
// dark, on a fake latch, to PNGs: `swift run Snapshots OUTPUT_DIR [PREFIX…]`,
// only the stills whose names start with a PREFIX when any are given.
import AppKit
import PlowLatchUI

/// Draws as the app in front, as people see it in use, without taking the
/// front from whatever has it: nothing here shows on a display.
final class FrontApplication: NSApplication {
    override var isActive: Bool { true }
}

FrontApplication.shared.setActivationPolicy(.accessory)
let arguments = CommandLine.arguments.dropFirst()
try await SnapshotRunner.run(into: URL(filePath: arguments.first ?? "snapshots"), only: Array(arguments.dropFirst()))

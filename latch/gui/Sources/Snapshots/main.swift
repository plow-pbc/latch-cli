// Renders every onboarding screen and state, and the main window, light and
// dark, on a fake latch, to PNGs: `swift run Snapshots OUTPUT_DIR [PREFIX…]`,
// only the stills whose names start with a PREFIX when any are given.
import AppKit
import PlowLatchUI

NSApplication.shared.setActivationPolicy(.accessory)
let arguments = CommandLine.arguments.dropFirst()
try await SnapshotRunner.run(into: URL(filePath: arguments.first ?? "snapshots"), only: Array(arguments.dropFirst()))

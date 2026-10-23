// Renders every onboarding screen and state, and the main window, light and
// dark, on a fake latch, to PNGs: `swift run Snapshots OUTPUT_DIR`.
import AppKit
import PlowLatchUI

NSApplication.shared.setActivationPolicy(.accessory)
try await SnapshotRunner.run(into: URL(filePath: CommandLine.arguments.dropFirst().first ?? "snapshots"))

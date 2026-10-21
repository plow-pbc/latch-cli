// Renders every screen and empty/error state on the sample latch to PNGs:
//   swift run Snapshots OUTPUT_DIR
// Each view draws its own layer tree into a bitmap, so no screen-recording
// permission is needed. The main window's sidebar column (a glass panel on
// macOS 26) does not draw this way and comes out blank.
import AppKit
import PlowLatchKit
import SwiftUI

let output = URL(filePath: CommandLine.arguments.dropFirst().first ?? "snapshots")
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
NSApplication.shared.setActivationPolicy(.accessory)

@MainActor
func render(_ name: String, _ scenario: FakeLatchClient.Scenario, dark: Bool = false,
            size: CGSize = CGSize(width: 900, height: 600), _ view: some View) async throws {
    let model = await AppModel.sample(scenario)
    let host = NSHostingView(rootView: view.environment(model))
    let window = NSWindow(
        contentRect: CGRect(origin: CGPoint(x: 0, y: 0), size: size),
        styleMask: [.titled, .fullSizeContentView], backing: .buffered, defer: false)
    window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
    window.contentView = host
    window.alphaValue = 0.01
    window.orderFrontRegardless()
    // Let each screen's .task fetch from the sample latch and lay out.
    try await Task.sleep(for: .milliseconds(700))
    host.layoutSubtreeIfNeeded()
    let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds)!
    let context = NSGraphicsContext(bitmapImageRep: rep)!
    NSGraphicsContext.current = context
    window.effectiveAppearance.performAsCurrentDrawingAppearance {
        NSColor.windowBackgroundColor.setFill()
        host.bounds.fill()
    }
    let cg = context.cgContext
    cg.translateBy(x: 0, y: host.bounds.height)
    cg.scaleBy(x: 1, y: -1)
    host.layer!.render(in: cg)
    NSGraphicsContext.current = nil
    let file = output.appending(path: "\(name)\(dark ? "-dark" : "").png")
    try rep.representation(using: .png, properties: [:])!.write(to: file)
    window.orderOut(nil)
    print(file.path)
}

for dark in [false, true] {
    try await render("overview-serving", .serving, dark: dark, MainView(screen: .overview))
    try await render("activity-serving", .serving, dark: dark, MainView(screen: .activity))
}
try await render("overview-sharing-stopped", .stopped, MainView(screen: .overview))
try await render("overview-not-logged-in", .loggedOut, MainView(screen: .overview))
try await render("overview-host-error", .hostError, MainView(screen: .overview))
try await render("overview-cant-reach-latch", .unreachable, MainView(screen: .overview))
try await render("activity-no-activity", .noActivity, MainView(screen: .activity))
try await render("activity-not-logged-in", .loggedOut, MainView(screen: .activity))
try await render("activity-cant-reach-latch", .unreachable, MainView(screen: .activity))
try await render("rules", .serving, MainView(screen: .rules))
try await render("rules-none", .noActivity, MainView(screen: .rules))
try await render("rules-not-logged-in", .loggedOut, MainView(screen: .rules))
try await render("rules-cant-reach-latch", .unreachable, MainView(screen: .rules))
let settings = CGSize(width: 480, height: 320)
for pane in SettingsView.Pane.allCases {
    try await render("settings-\(pane.rawValue)", .serving, size: settings, SettingsView(pane: pane))
}
// The menu bar extra's menu items, laid out as a stack (a native menu only
// draws while open), and its icon for each state.
for scenario in [FakeLatchClient.Scenario.serving, .stopped, .hostError, .loggedOut, .unreachable] {
    try await render("menu-\(scenario.rawValue)", scenario, size: CGSize(width: 320, height: 260),
                     VStack(alignment: .leading, spacing: 8) { StatusMenu() }.padding())
    try await render("menu-icon-\(scenario.rawValue)", scenario, size: CGSize(width: 220, height: 44),
                     MenuIconSample())
}

struct MenuIconSample: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack {
            Image(systemName: model.menuState.symbol).font(.system(size: 15))
            Text(model.menuState.title)
        }
        .padding(10)
    }
}

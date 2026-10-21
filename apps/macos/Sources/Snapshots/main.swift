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

// Onboarding, drawn by ImageRenderer (it is all SwiftUI, so blur and glow
// render as on screen).
@MainActor
func renderOnboarding(_ name: String, step: Onboarding.Step = .welcome, signIn: Onboarding.SignIn = .starting,
                      folders: [URL] = [], sharing: Bool = false, error: String? = nil,
                      time: Double? = nil, reduceMotion: Bool = false, into dir: URL = output) async throws {
    let fake = FakeLatchClient(.loggedOut)
    let app = AppModel(client: fake, sharing: fake)
    let flow = Onboarding(app: app) { _ in }
    flow.stage(step, signIn: signIn, folders: folders, sharing: sharing, error: error)
    let view = OnboardingView(still: flow, entranceTime: time)
        .environment(app)
        .environment(\.reduceMotionOverride, reduceMotion)
    let renderer = ImageRenderer(content: view)
    renderer.scale = 2
    try await Task.sleep(for: .milliseconds(50))
    guard let image = renderer.cgImage else { throw CocoaError(.fileWriteUnknown) }
    let file = dir.appending(path: "\(name).png")
    try NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])!.write(to: file)
    print(file.path)
}

let entrance = output.appending(path: "welcome-entrance")
try FileManager.default.createDirectory(at: entrance, withIntermediateDirectories: true)
// The same moments the old app's frames were taken at.
for ms in [0, 100, 250, 400, 550, 700, 850, 1000, 1150, 1300, 1450, 1600, 1800, 2100, 2600] {
    try await renderOnboarding(String(format: "t%04d", ms), time: Double(ms) / 1000, into: entrance)
}
try await renderOnboarding("onboarding-welcome-reduce-motion", reduceMotion: true)
let code = Fixtures.loginCode
let peer = LocalStatus.Peer(name: "mac-host", key: "k7f3a9c2e1d04b58a6f2c9e3d1b7a4f0", kind: "device")
try await renderOnboarding("onboarding-signin-starting", step: .signIn)
try await renderOnboarding("onboarding-signin-waiting", step: .signIn, signIn: .waiting(code))
try await renderOnboarding("onboarding-signin-expired", step: .signIn, signIn: .expired(code))
try await renderOnboarding("onboarding-signin-error", step: .signIn,
                           signIn: .failed("this sign-in code is no longer valid: get a new one"))
try await renderOnboarding("onboarding-signin-verified", step: .signIn, signIn: .verified(peer))
try await renderOnboarding("onboarding-linked", step: .linked, signIn: .verified(peer))
try await renderOnboarding("onboarding-keep-running-empty", step: .keepRunning, signIn: .verified(peer))
let picked = [URL(filePath: "/Users/owner/work"), URL(filePath: "/Users/owner/Documents/shared")]
try await renderOnboarding("onboarding-keep-running-folders", step: .keepRunning, signIn: .verified(peer), folders: picked)
try await renderOnboarding("onboarding-keep-running-error", step: .keepRunning, signIn: .verified(peer), folders: picked,
                           error: "Background sharing isn't in this build yet.")
try await renderOnboarding("onboarding-done-sharing", step: .done, signIn: .verified(peer), folders: picked, sharing: true)
try await renderOnboarding("onboarding-done-not-now", step: .done, signIn: .verified(peer), folders: picked)

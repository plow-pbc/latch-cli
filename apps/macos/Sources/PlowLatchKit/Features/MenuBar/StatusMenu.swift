import SwiftUI

/// The menu bar extra's menu: a plain menu, per the HIG.
public struct StatusMenu: View {
    @Environment(AppModel.self) private var model

    public init() {}

    public var body: some View {
        Text(statusLine)
        Divider()
        if model.menuState == .needsApproval {
            Button("Open Login Items…") { BackgroundSharing.openLoginItems() }
        }
        if model.canStopSharing {
            Button("Stop Sharing") { Task { await model.stopSharing() } }
        } else {
            Button("Start Sharing") { Task { await model.startSharing() } }
                .disabled(!model.canStartSharing)
        }
        Divider()
        Button("Open Plow Latch…") { WindowRouter.shared.showHome(onboarding: model.needsOnboarding) }
        Button("Settings…") { WindowRouter.shared.showSettings() }
            .keyboardShortcut(",")
        Divider()
        Button("Quit Plow Latch") { NSApplication.shared.terminate(nil) }
            .keyboardShortcut("q")
    }

    private var statusLine: String {
        switch model.menuState {
        case let .error(message): "Sharing failed: \(message)"
        case .outside: "Sharing from a latch serve started outside the app"
        case .serving:
            if let name = model.status.value?.peer?.name { "Sharing as \(name)" } else { "Sharing" }
        case let state: state.title
        }
    }
}

/// The menu bar icon: a template symbol for the host's state. It also hands
/// the router SwiftUI's window actions, since it lives as long as the app.
public struct StatusIcon: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    public init() {}

    public var body: some View {
        Image(systemName: model.menuState.symbol)
            .accessibilityLabel("Plow Latch: \(model.menuState.title)")
            .onAppear { WindowRouter.shared.install(openWindow: openWindow, openSettings: openSettings) }
    }
}

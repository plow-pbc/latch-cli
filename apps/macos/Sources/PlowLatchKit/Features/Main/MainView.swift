import SwiftUI

public enum Screen: String, CaseIterable, Identifiable, Sendable {
    case overview, activity, rules
    public var id: Self { self }

    var title: String {
        switch self {
        case .overview: "Overview"
        case .activity: "Activity"
        case .rules: "Rules"
        }
    }

    var symbol: String {
        switch self {
        case .overview: "desktopcomputer"
        case .activity: "list.bullet.rectangle"
        case .rules: "text.page"
        }
    }
}

/// The main window: a sidebar of screens and the selected one.
public struct MainView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings
    private let initialScreen: Screen?

    /// Shows `screen`, or else the one the app last showed.
    public init(screen: Screen? = nil) {
        initialScreen = screen
    }

    public var body: some View {
        @Bindable var model = model
        let screen = model.screen
        return NavigationSplitView {
            List(Screen.allCases, selection: Binding($model.screen)) { screen in
                Label(screen.title, systemImage: screen.symbol)
            }
            .navigationSplitViewColumnWidth(min: 160, ideal: 180)
        } detail: {
            switch screen {
            case .overview: OverviewView()
            case .activity: ActivityView { model.screen = .overview }
            case .rules: RulesView()
            }
        }
        .frame(minWidth: 640, minHeight: 420)
        .onAppear {
            if let initialScreen { model.screen = initialScreen }
            WindowRouter.shared.install(openWindow: openWindow, openSettings: openSettings)
        }
        .focusedSceneValue(\.screen, $model.screen)
    }
}

extension FocusedValues {
    /// The main window's selected screen, for the View menu.
    @Entry public var screen: Binding<Screen>?
}

/// View ▸ Overview / Activity / Rules, with ⌘1–⌘3.
public struct ScreenCommands: Commands {
    @FocusedValue(\.screen) private var screen

    public init() {}

    public var body: some Commands {
        CommandGroup(before: .sidebar) {
            ForEach(Array(Screen.allCases.enumerated()), id: \.element) { index, item in
                Button(item.title) { screen?.wrappedValue = item }
                    .keyboardShortcut(KeyEquivalent(Character("\(index + 1)")), modifiers: .command)
                    .disabled(screen == nil)
            }
            Divider()
        }
    }
}

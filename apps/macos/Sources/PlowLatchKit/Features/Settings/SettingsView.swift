import ServiceManagement
import SwiftUI

/// Settings: few, infrequently changed options.
public struct SettingsView: View {
    public enum Pane: String, CaseIterable, Sendable { case general, commandLine, about }

    @State private var pane: Pane

    public init(pane: Pane = .general) {
        _pane = State(initialValue: pane)
    }

    public var body: some View {
        TabView(selection: $pane) {
            Tab("General", systemImage: "gearshape", value: .general) { GeneralPane() }
            Tab("Command Line", systemImage: "terminal", value: .commandLine) { CommandLinePane() }
            Tab("About", systemImage: "info.circle", value: .about) { AboutPane() }
        }
        .frame(width: 480)
    }
}

private struct GeneralPane: View {
    @AppStorage(WindowRouter.keepInMenuBarKey) private var keepInMenuBar = false
    @State private var launchAtLogin = SMAppService.mainApp.status == .enabled
    @State private var loginError: String?
    @Environment(\.theme) private var theme

    var body: some View {
        Form {
            Toggle("Launch at login", isOn: $launchAtLogin)
                .onChange(of: launchAtLogin) { _, on in setLaunchAtLogin(on) }
            if let loginError {
                Text(loginError).font(theme.type.detail).foregroundStyle(theme.colors.error)
            }
            Toggle("Keep in menu bar when closed", isOn: $keepInMenuBar)
            Text("With every window closed, Plow Latch leaves the Dock and stays in the menu bar.")
                .font(theme.type.detail)
                .foregroundStyle(theme.colors.secondaryText)
        }
        .formStyle(.grouped)
    }

    private func setLaunchAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            loginError = nil
        } catch {
            loginError = error.localizedDescription
            launchAtLogin = SMAppService.mainApp.status == .enabled
        }
    }
}

private struct CommandLinePane: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme

    var body: some View {
        Form {
            LabeledContent("Bundled latch", value: model.status.value?.version ?? "Unknown")
            LabeledContent("State folder") {
                Text(model.status.value?.stateDir ?? "Unknown")
                    .font(theme.type.mono)
                    .textSelection(.enabled)
            }
            if case let .failed(error, _) = model.status {
                Text(error.message).font(theme.type.detail).foregroundStyle(theme.colors.error)
            }
        }
        .formStyle(.grouped)
        .task { await model.refreshStatus() }
    }
}

private struct AboutPane: View {
    @Environment(\.theme) private var theme

    var body: some View {
        VStack(spacing: theme.space.small) {
            Image(systemName: MenuState.serving.symbol).font(theme.type.hero)
            Text("Plow Latch").font(theme.type.title)
            Text("Version \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev")")
                .foregroundStyle(theme.colors.secondaryText)
            Text("See what this Mac shares, review what callers did, and change what agents may do.")
                .multilineTextAlignment(.center)
                .font(theme.type.detail)
        }
        .padding(theme.space.large)
        .frame(maxWidth: .infinity)
    }
}

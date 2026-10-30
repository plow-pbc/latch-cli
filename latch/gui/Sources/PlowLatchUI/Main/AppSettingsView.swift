import SwiftUI

/// The app's Settings window (⌘,): this Mac's Latch updates, and logging
/// this Mac out of the account.
struct AppSettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var loggingOut = false
    @State private var logOutError: LatchError?

    var body: some View {
        Form {
            Section("Updates") {
                HStack {
                    LabeledContent("Latch", value: updateText)
                    if app.updateAvailable, !app.updateInstalled { InstallUpdateButton() }
                    Button(app.checkingUpdate ? "Checking…" : "Check Now") { Task { await app.checkForUpdate() } }
                        .disabled(app.checkingUpdate || app.installingUpdate)
                }
                if case .failure? = app.update {
                    Text("Couldn't check for updates. Try again in a moment.").foregroundStyle(.red)
                }
                UpdateProgress()
                Toggle("Check for updates automatically", isOn: checksForUpdates)
            }
            Section {
                if let name = app.status?.loggedInAs {
                    HStack {
                        LabeledContent("Logged in as", value: name)
                        Button("Log Out…") { loggingOut = true }
                    }
                    if let logOutError { ErrorText(error: logOutError).foregroundStyle(.red) }
                } else {
                    LabeledContent("Logged in as", value: "Not logged in")
                }
            } header: {
                Text("Account")
            } footer: {
                Text("Logging out stops Latch on this Mac and takes it out of your Plow account.").foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 520)
        .fixedSize(horizontal: false, vertical: true)
        .confirmationDialog("Log out this Mac?", isPresented: $loggingOut) {
            Button("Log Out", role: .destructive) {
                Task {
                    do {
                        try await app.logOut()
                    } catch {
                        logOutError = .from(error)
                    }
                }
            }
        } message: {
            Text("Latch stops here and this Mac leaves your Plow account. Its vault and activity stay, readable after you log in again.")
        }
        .alert("Still in your Plow account", isPresented: logOutWarningBinding) {
            Button("OK") { app.acknowledgeLogOutWarning() }
        } message: {
            Text(app.logOutWarning ?? "")
        }
        // Logged out, onboarding takes over: Settings closes with the main window.
        .onChange(of: app.onboarding != nil) { _, onboarding in
            if onboarding { dismiss() }
        }
    }

    private var logOutWarningBinding: Binding<Bool> {
        Binding { app.logOutWarning != nil } set: { if !$0 { app.acknowledgeLogOutWarning() } }
    }

    private var checksForUpdates: Binding<Bool> {
        Binding { app.checksForUpdates } set: { app.checksForUpdates = $0 }
    }

    private var updateText: String {
        switch app.update {
        case let .success(check)?:
            if check.available { return "Build \(check.latest?.build.map(String.init) ?? "?") is available" }
            guard let current = check.current else { return "A local build · latest is \(check.latest?.tag ?? "unknown")" }
            return "Up to date · build \(current)"
        default:
            if app.updateAvailable { return "Build \(app.availableBuild.map(String.init) ?? "?") is available" }
            return "Not checked yet"
        }
    }
}

/// Installs the newest build with `latch update`; again after a failure.
struct InstallUpdateButton: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Button(app.installingUpdate ? "Updating…" : app.installUpdateError == nil ? "Install Update" : "Try Again") {
            Task { await app.installUpdate() }
        }
        .disabled(app.installingUpdate)
    }
}

/// An install of the update: while latch installs it, how it failed in
/// latch's words, or that it's in and this window still runs the old one.
struct UpdateProgress: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if app.installingUpdate {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text("Updating… Latch restarts on this Mac, and this window opens again on the new version.")
                    .foregroundStyle(.secondary)
            }
        } else if let error = app.installUpdateError {
            ErrorText(error: error, said: "Latch couldn't install the update.").foregroundStyle(.red)
        } else if app.updateInstalled {
            Text("The update is installed. Quit Latch and open it again to use it.").foregroundStyle(.secondary)
        }
    }
}

/// The toolbar's pill while a newer Latch waits: one click installs it,
/// then it says so until latch quits this window for the install.
struct UpdatePill: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Button { Task { await app.installUpdate() } } label: {
            HStack(spacing: 5) {
                if app.installingUpdate {
                    ProgressView().controlSize(.mini).tint(.white)
                } else {
                    Image(systemName: app.installUpdateError == nil ? "arrow.down.circle.fill" : "exclamationmark.circle.fill")
                }
                Text(app.installingUpdate ? "Updating…" : app.installUpdateError == nil ? "Update Available" : "Update Failed · Try Again")
            }
            .font(.callout.weight(.semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(app.installUpdateError == nil ? Color.accentColor : Color.red, in: Capsule())
        }
        .buttonStyle(.plain)
        .disabled(app.installingUpdate)
        .help(app.availableBuild.map { "Install Latch build \($0)" } ?? "Install the newest Latch")
        .accessibilityLabel(app.installingUpdate ? "Updating Latch" : "Install Latch update")
    }
}

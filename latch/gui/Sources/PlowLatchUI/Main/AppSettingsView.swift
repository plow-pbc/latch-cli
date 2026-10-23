import SwiftUI

/// The app's Settings window (⌘,): this Mac's Latch updates, and logging
/// this Mac out of the account.
struct AppSettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var loggingOut = false
    @State private var logOutError: String?

    var body: some View {
        Form {
            Section("Updates") {
                HStack {
                    LabeledContent("Latch", value: updateText)
                    Button(app.checkingUpdate ? "Checking…" : "Check Now") { Task { await app.checkForUpdate() } }
                        .disabled(app.checkingUpdate)
                }
                if case .failure? = app.update {
                    Text("Couldn't check for updates. Try again in a moment.").foregroundStyle(.red)
                }
                if available {
                    Text("Installing it from the app isn't available yet: update Latch from Terminal, and it restarts what was running.")
                        .foregroundStyle(.secondary)
                }
            }
            Section {
                if let me = app.status?.peer {
                    HStack {
                        LabeledContent("Logged in as", value: me.name)
                        Button("Log Out…") { loggingOut = true }
                    }
                    if let logOutError { Text(logOutError).foregroundStyle(.red) }
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
                        logOutError = error.localizedDescription
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

    private var available: Bool {
        if case let .success(check)? = app.update { return check.available }
        return app.status?.update?.available == true
    }

    private var updateText: String {
        switch app.update {
        case let .success(check)?:
            if check.available { return "Build \(check.latest?.build.map(String.init) ?? "?") is available" }
            guard let current = check.current else { return "A local build · latest is \(check.latest?.tag ?? "unknown")" }
            return "Up to date · build \(current)"
        default:
            if let update = app.status?.update, update.available { return "Build \(update.build.map(String.init) ?? "?") is available" }
            return "Not checked yet"
        }
    }
}

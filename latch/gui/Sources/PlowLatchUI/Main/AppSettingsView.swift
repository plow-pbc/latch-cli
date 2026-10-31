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
                Toggle("Check automatically", isOn: checksForUpdates)
            }
            Section {
                if let name = app.status?.loggedInAs {
                    HStack {
                        LabeledContent("Name", value: app.account.map { AccountFacts($0).name } ?? "")
                        Button("Log Out…") { loggingOut = true }
                    }
                    if let logOutError { ErrorText(error: logOutError).foregroundStyle(.red) }
                    if let facts = app.account.map({ AccountFacts($0) }) {
                        if let phones = facts.phones { LabeledContent("Phone", value: phones) }
                        if let google = facts.google { LabeledContent("Google", value: google) }
                        if let credit = facts.credit {
                            LabeledContent("Credit") {
                                Text(credit).foregroundStyle(facts.overdrawn ? .red : .secondary)
                            }
                        }
                    }
                    LabeledContent("This Mac", value: app.viewerOnly ? "\(name) · viewing only" : name)
                    if let hosts = app.account?.hosts {
                        HStack {
                            LabeledContent("Hosts", value: AccountFacts.counted(hosts.count, "host", online: hosts.count { $0.online == true }))
                            Button("Show") { app.showInMainWindow(.host(.home)) }
                                .disabled(hosts.isEmpty)
                        }
                    }
                    if let agents = app.agents {
                        HStack {
                            LabeledContent("Agents", value: AccountFacts.counted(agents.count, "agent"))
                            Button("Show") { app.showInMainWindow(.account(.agents)) }
                        }
                    }
                    if let plow = app.account.flatMap({ AccountFacts($0).plow }) { LabeledContent("Plow", value: plow) }
                    if let error = app.accountError {
                        HStack {
                            Text("Couldn't reach Plow").foregroundStyle(.red).help(error.details ?? "")
                            Spacer()
                            Button("Retry") { Task { await app.refreshAccount() } }
                        }
                    }
                } else {
                    LabeledContent("Logged in as", value: "Not logged in")
                }
            } header: {
                Text("Account")
            } footer: {
                if let read = app.accountReadAt, app.accountError == nil {
                    Text("Updated \(read.formatted(date: .omitted, time: .shortened))").foregroundStyle(.secondary)
                }
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
        // Plow's account and agents, read again each time Settings opens.
        .task {
            guard !app.still, app.status?.loggedInAs != nil else { return }
            await app.refreshAccountIfStale()
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
            if check.available { return "Build \(check.latest?.build.map(String.init) ?? "?") available" }
            guard let current = check.current else { return "A local build · latest is \(check.latest?.tag ?? "unknown")" }
            return "Up to date · build \(current)"
        default:
            if app.updateAvailable { return "Build \(app.availableBuild.map(String.init) ?? "?") available" }
            return "Not checked yet"
        }
    }
}

/// Installs the newest build with `latch update`; again after a failure.
struct InstallUpdateButton: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Button(app.installingUpdate ? "Updating…" : app.installUpdateError == nil ? "Install Update" : "Retry") {
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
                Text("Latch will restart.")
                    .foregroundStyle(.secondary)
            }
        } else if let error = app.installUpdateError {
            ErrorText(error: error, said: "Couldn't update.").foregroundStyle(.red)
        } else if app.updateInstalled {
            Text("Installed. Reopen Latch.").foregroundStyle(.secondary)
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
                Text(app.installingUpdate ? "Updating…" : app.installUpdateError == nil ? "Update" : "Update failed · Retry")
            }
            .font(.callout.weight(.semibold))
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(app.installUpdateError == nil ? Color.accentColor : Color.red, in: Capsule())
        }
        .buttonStyle(.plain)
        .disabled(app.installingUpdate)
        .help(app.availableBuild.map { "Install build \($0)" } ?? "Install update")
        .accessibilityLabel(app.installingUpdate ? "Updating Latch" : "Update Latch")
    }
}

/// What Settings says of the account, from `latch status`, in words.
struct AccountFacts: Equatable {
    var name: String
    var phones: String?
    var google: String?
    var credit: String?
    /// The credit is spent past zero.
    var overdrawn = false
    /// The Plow this Mac asks, as its address reads.
    var plow: String?

    init(name: String, phones: String? = nil, google: String? = nil, credit: String? = nil, overdrawn: Bool = false, plow: String? = nil) {
        (self.name, self.phones, self.google, self.credit, self.overdrawn, self.plow) = (name, phones, google, credit, overdrawn, plow)
    }

    init(_ status: AccountStatus, locale: Locale = .current) {
        let region = locale.region?.identifier
        let account = status.account
        name = account?.displayName ?? "Not set"
        phones = account?.phones.flatMap { $0.isEmpty ? nil : $0.map { PhoneNumber($0)?.formatted(region: region) ?? $0 }.joined(separator: ", ") }
        google = account?.emails.flatMap { $0.isEmpty ? nil : $0.joined(separator: ", ") }
        if let text = account?.availableBalanceUsd, let usd = Decimal(string: text, locale: Locale(identifier: "en_US_POSIX")) {
            credit = usd.formatted(.currency(code: "USD").locale(locale))
            overdrawn = usd < 0
        }
        plow = status.apiUrl.flatMap { url in url.host().map { host in url.port.map { "\(host):\($0)" } ?? host } }
    }

    /// "3 hosts · 2 online", "1 agent", "None".
    static func counted(_ count: Int, _ noun: String, online: Int? = nil) -> String {
        guard count > 0 else { return "None" }
        let counted = "\(count) \(noun)\(count == 1 ? "" : "s")"
        return online.map { "\(counted) · \($0) online" } ?? counted
    }
}

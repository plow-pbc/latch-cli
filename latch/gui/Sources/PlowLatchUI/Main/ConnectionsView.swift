import SwiftUI

/// The Google accounts connected in Plow, as onboarding and Connections
/// both list them, and which sign-in each offers.
nonisolated struct GoogleAccounts: Equatable {
    var accounts: [String]
    /// Those Google wants the owner to sign in with again.
    var needsSignIn: [String] = []

    init(accounts: [String], needsSignIn: [String] = []) {
        self.accounts = accounts
        self.needsSignIn = needsSignIn
    }

    /// From `setup --status`'s Google step, which lists every account,
    /// those needing a sign-in again among them; nil when Plow didn't say.
    init?(_ step: SetupStatus.Step?) {
        guard let accounts = step?.accounts else { return nil }
        self.init(accounts: accounts, needsSignIn: step?.needsSignIn ?? [])
    }

    /// From latch's enable, which lists the accounts that work and names
    /// the rest apart.
    init(_ enabled: GoogleEnabled) {
        let again = enabled.needsSignIn ?? []
        self.init(accounts: enabled.accounts + again.filter { !enabled.accounts.contains($0) }, needsSignIn: again)
    }

    func needsSignIn(_ account: String) -> Bool { needsSignIn.contains(account) }

    /// The accounts that work.
    var working: [String] { accounts.filter { !needsSignIn($0) } }

    /// One account works, so a sign-in adds another (`--add-account`);
    /// otherwise it connects the first, or signs in again with them all.
    var addsAccount: Bool { !working.isEmpty }

    /// None of them works: each one's row offers to sign in again, which
    /// waits until they all work.
    var noneWorks: Bool { !accounts.isEmpty && !addsAccount }

    /// Google's state, by its accounts.
    var state: String { accounts.isEmpty ? "Not connected" : noneWorks ? "Needs sign-in" : "Connected" }

    /// The section's own button, while some or no account works.
    var buttonTitle: String? {
        if noneWorks { return nil }
        return addsAccount ? "Add Another Account…" : "Connect Google…"
    }

    /// Whether the account's row offers Sign In Again….
    func signsInAgain(_ account: String) -> Bool { noneWorks && needsSignIn(account) }

    /// Some work, so latch turns Google on without waiting for this one:
    /// the owner reconnects it in Plow, as latch tells agents.
    func reconnectsInPlow(_ account: String) -> Bool { !noneWorks && needsSignIn(account) }

    static let reconnect = "Reconnect it on Plow's Connections page."
}

/// What Connections shows of Google: the accounts connected in Plow, or
/// that Plow couldn't say.
nonisolated enum Connections: Equatable {
    case google(GoogleAccounts)
    case unknown

    init(_ setup: SetupStatus?) {
        guard let accounts = GoogleAccounts(setup?.step("google")) else {
            self = .unknown
            return
        }
        self = .google(accounts)
    }
}

/// Google in a grouped Form, the same in onboarding and Connections: its
/// state, every account connected in Plow, and the one sign-in it offers.
/// `detail` is the screen's own lines: a wait, a problem, a retry.
struct GoogleAccountsSection<Detail: View>: View {
    var state: String
    var stateColor: Color = .secondary
    var accounts: GoogleAccounts
    /// False while a check or a sign-in is under way.
    var canSignIn: Bool
    /// The sign-in `accounts` offers.
    var signIn: () -> Void
    @ViewBuilder var detail: Detail

    var body: some View {
        Section {
            LabeledContent("Google") { Text(state).foregroundStyle(stateColor) }
            ForEach(accounts.accounts, id: \.self) { account in
                LabeledContent {
                    if accounts.needsSignIn(account) {
                        HStack(spacing: 10) {
                            Text("Needs sign-in").foregroundStyle(.secondary)
                            if accounts.signsInAgain(account) {
                                Button("Sign In Again…", action: signIn).disabled(!canSignIn)
                            }
                        }
                    }
                } label: {
                    Label {
                        Text(account)
                        if accounts.reconnectsInPlow(account) {
                            Text(GoogleAccounts.reconnect).foregroundStyle(.secondary)
                        }
                    } icon: {
                        Image(systemName: "envelope")
                    }
                }
            }
            detail
            if canSignIn, let title = accounts.buttonTitle {
                HStack {
                    Spacer()
                    Button(title, action: signIn)
                }
            }
        } footer: {
            Text("Gmail and Google Calendar, for every host where the Google plugin is on. You sign in with Google in your browser.")
                .foregroundStyle(.secondary)
        }
    }
}

/// Connections' sheet while latch waits for a Google sign-in: connecting a
/// first account or adding another, and why it ended without one.
struct GoogleSignIn: Equatable {
    var adding: Bool
    var failure: String?

    static let stopped = "Signing in stopped before Google answered. Try again."

    /// latch's refusal, said plainly: decided on its code, never shown.
    static func problem(_ error: LatchError, adding: Bool) -> String {
        switch error.code {
        case "sign_in_timeout":
            adding ? "No new Google account showed up in time. An account that's already connected isn't added again."
                : "No Google account showed up in time. If you finished signing in, try again."
        case "plow_unreachable": "Plow can't be reached right now. Check your internet connection, then try again."
        case "not_logged_in": "This Mac isn't logged in to Plow."
        case "gog_not_configured": Onboarding.toolsMissing
        default: "Couldn't connect Google just now. Try again."
        }
    }
}

/// Your account › Connections: the accounts connected in Plow, the same
/// whatever host is picked, and the account's logged-in devices. The
/// window's title is the page's.
struct ConnectionsView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            googleSection
            devicesSection
        }
        .formStyle(.grouped)
    }

    private var googleSection: some View {
        let accounts = if case let .google(accounts)? = app.connections { accounts } else { GoogleAccounts(accounts: []) }
        return GoogleAccountsSection(state: googleState, accounts: accounts, canSignIn: app.connections != .unknown && app.connections != nil,
                                     signIn: { app.connectGoogle(adding: accounts.addsAccount) }) {
            switch app.connections {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case .unknown?:
                HStack {
                    Text("Plow couldn't say which Google accounts are connected.").foregroundStyle(.secondary)
                    Spacer()
                    Button("Check Again") { Task { await app.refreshConnections() } }
                }
            case .google?:
                EmptyView()
            }
        }
    }

    private var googleState: String {
        switch app.connections {
        case nil: "Checking…"
        case .unknown?: "Couldn't reach Plow"
        case let .google(accounts)?: accounts.state
        }
    }

    @ViewBuilder
    private var devicesSection: some View {
        Section {
            switch app.devices {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case .failure?:
                Text("Plow couldn't list your logged-in devices.").foregroundStyle(.secondary)
            case .success?:
                let devices = app.loggedInDevices ?? []
                if devices.isEmpty {
                    Text("None").foregroundStyle(.secondary)
                }
                ForEach(devices) { device in
                    let thisMac = device.key == app.status?.thisMachine.key
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(thisMac ? "\(device.name) (this Mac)" : device.name)
                            Text(seen(device)).font(.callout).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Remove…") { app.ask(.removeDevice(device)) }
                            .accessibilityLabel("Remove \(device.name)…")
                            .disabled(thisMac)
                            .help(thisMac ? "Log out this Mac in Settings instead" : "Remove \(device.name) from your account")
                    }
                }
            }
            if let error = app.deviceRemoveError { ErrorText(error: error, place: .devices).foregroundStyle(.red) }
        } header: {
            Text("Logged-in devices")
        } footer: {
            Text("Logged-in devices can send requests as you but don't host.").foregroundStyle(.secondary)
        }
    }

    private func seen(_ device: DeviceList.Device) -> String {
        let state = device.online == true ? "Online" : "Offline"
        guard let seen = device.lastSeenAt else { return state }
        return "\(state) · seen \(seen.formatted(date: .abbreviated, time: .shortened))"
    }
}

/// While latch waits for a Google sign-in in the browser. It goes by itself
/// once Google is on; Cancel, or the sheet going any other way, stops the
/// wait.
struct GoogleSignInSheet: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        let sheet = app.googleSignIn ?? GoogleSignIn(adding: false)
        VStack(alignment: .leading, spacing: 16) {
            Text(sheet.adding ? "Add a Google Account" : "Connect a Google Account").font(.headline)
            if let failure = sheet.failure {
                Text(failure).fixedSize(horizontal: false, vertical: true)
            } else {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    ProgressView().controlSize(.small).accessibilityHidden(true)
                    Text("Finish signing in with Google in your browser. This closes by itself when you're done.")
                        .fixedSize(horizontal: false, vertical: true)
                }
                if sheet.adding {
                    Text("Choose an account that isn't connected yet.").font(.callout).foregroundStyle(.secondary)
                }
            }
            HStack {
                if sheet.failure == nil {
                    Button("Open the Sign-In Page Again") { app.openGoogleSignInAgain() }
                        .disabled(!app.canOpenGoogleSignInAgain)
                }
                Spacer()
                if sheet.failure != nil {
                    Button("Close") { app.endGoogleSignIn() }.keyboardShortcut(.cancelAction)
                    Button("Try Again") { app.connectGoogle(adding: sheet.adding) }.keyboardShortcut(.defaultAction)
                } else {
                    Button("Cancel") { app.endGoogleSignIn() }.keyboardShortcut(.cancelAction)
                }
            }
        }
        .padding(20)
        .frame(width: 420)
        .onDisappear { app.endGoogleSignIn() }
    }
}

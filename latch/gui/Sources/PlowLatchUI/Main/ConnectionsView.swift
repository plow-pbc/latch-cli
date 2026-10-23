import SwiftUI

/// What Connections shows of Google: the accounts connected in Plow, or
/// that Plow couldn't say.
nonisolated enum Connections: Equatable {
    case google(accounts: [String])
    case unknown

    /// From `setup --status`'s Google step, which lists the accounts only
    /// when Plow answered.
    init(_ setup: SetupStatus?) {
        guard let accounts = setup?.step("google")?.accounts else {
            self = .unknown
            return
        }
        self = .google(accounts: accounts)
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
/// whatever host is picked, and the account's logged-in devices.
struct ConnectionsView: View {
    @Environment(AppModel.self) private var app
    @State private var removingDevice: DeviceList.Device?

    var body: some View {
        Form {
            googleSection
            devicesSection
        }
        .formStyle(.grouped)
        .navigationTitle("Connections")
        .task {
            guard !app.still else { return }
            await app.refreshConnections()
            await app.refreshDevices()
        }
        .sheet(isPresented: signingInBinding) {
            GoogleSignInSheet()
        }
        .confirmationDialog("Remove “\(removingDevice?.name ?? "")” from your account?", isPresented: removingBinding,
                            presenting: removingDevice) { device in
            Button("Remove", role: .destructive) {
                Task { await app.removeDevice(device.name) }
            }
        } message: { device in
            Text("\(device.name) is logged out of Plow and can no longer send requests as you.")
        }
    }

    @ViewBuilder
    private var googleSection: some View {
        Section {
            switch app.connections {
            case nil:
                ProgressView().frame(maxWidth: .infinity)
            case .unknown?:
                HStack {
                    Text("Plow couldn't say which Google accounts are connected.").foregroundStyle(.secondary)
                    Spacer()
                    Button("Check Again") { Task { await app.refreshConnections() } }
                }
            case let .google(accounts)?:
                if accounts.isEmpty {
                    Text("No Google account is connected.").foregroundStyle(.secondary)
                }
                ForEach(accounts, id: \.self) { account in
                    Label(account, systemImage: "envelope")
                }
                HStack {
                    Spacer()
                    Button(accounts.isEmpty ? "Connect Google Account…" : "Add Another Account…") { app.connectGoogle() }
                }
            }
        } header: {
            Text("Google")
        } footer: {
            Text("Gmail and Google Calendar, for every host where the Google plugin is on. You sign in with Google in your browser.")
                .foregroundStyle(.secondary)
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
                    let thisMac = device.key == app.status?.peer?.key
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(thisMac ? "\(device.name) (this Mac)" : device.name)
                            Text(seen(device)).font(.callout).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Remove…") { removingDevice = device }
                            .disabled(thisMac)
                            .help(thisMac ? "Log out this Mac in Settings instead" : "Remove it from your account")
                    }
                }
            }
            if let error = app.deviceRemoveError { Text(error).foregroundStyle(.red) }
        } header: {
            Text("Logged-in devices")
        } footer: {
            Text("Logged-in devices can send requests as you but don't host.").foregroundStyle(.secondary)
        }
    }

    private var signingInBinding: Binding<Bool> {
        Binding { app.googleSignIn != nil } set: { if !$0 { app.endGoogleSignIn() } }
    }

    private var removingBinding: Binding<Bool> {
        Binding { removingDevice != nil } set: { if !$0 { removingDevice = nil } }
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
                    Button("Try Again") { app.connectGoogle() }.keyboardShortcut(.defaultAction)
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

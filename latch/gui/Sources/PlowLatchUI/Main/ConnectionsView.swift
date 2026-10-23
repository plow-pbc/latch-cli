import SwiftUI

/// What Connections shows of Google: the accounts connected in Plow and
/// where to connect one, or that Plow couldn't say.
nonisolated enum Connections: Equatable {
    case google(accounts: [String], connect: URL?)
    case unknown

    /// From `setup --status`'s Google step, which lists the accounts only
    /// when Plow answered.
    init(_ setup: SetupStatus?) {
        guard let step = setup?.step("google"), let accounts = step.accounts else {
            self = .unknown
            return
        }
        self = .google(accounts: accounts, connect: step.connectUrl)
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
            case let .google(accounts, connect)?:
                if accounts.isEmpty {
                    Text("No Google account is connected.").foregroundStyle(.secondary)
                }
                ForEach(accounts, id: \.self) { account in
                    Label(account, systemImage: "envelope")
                }
                if connect != nil {
                    HStack {
                        Spacer()
                        Button(accounts.isEmpty ? "Connect Google Account…" : "Add Another Account…") { app.connectGoogle() }
                    }
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

    private var removingBinding: Binding<Bool> {
        Binding { removingDevice != nil } set: { if !$0 { removingDevice = nil } }
    }

    private func seen(_ device: DeviceList.Device) -> String {
        let state = device.online == true ? "Online" : "Offline"
        guard let seen = device.lastSeenAt else { return state }
        return "\(state) · seen \(seen.formatted(date: .abbreviated, time: .shortened))"
    }
}

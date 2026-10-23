import SwiftUI

/// The main window until the main app lands: this Mac's latch as
/// `status --local` says, what onboarding set up, and Set up again.
struct MainView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            Section("This Mac") {
                LabeledContent("Mac name in Plow", value: failed ? "Couldn't check" : app.status?.peer?.name ?? "Not logged in")
                LabeledContent("Latch") {
                    Text(hostState).foregroundStyle(app.status?.serve.ready == true ? Brand.volt : .secondary)
                }
                if let blocked = app.status?.serve.blocked ?? app.status?.lastError {
                    Text(blocked.message).foregroundStyle(.red)
                }
                if let update = app.status?.update, update.available {
                    LabeledContent("Update", value: "Available")
                }
                if let error = app.statusError {
                    Text(error).foregroundStyle(.red)
                }
            }
            Section("Your setup") {
                ForEach(Array(Summary.rows(app: app, flow: nil).dropFirst().dropLast().enumerated()), id: \.offset) { _, row in
                    LabeledContent(row.title, value: row.state)
                }
                Button("Set up again…") { app.setUpAgain() }
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 420, minHeight: 360)
        .task {
            guard !app.still else { return }
            await app.refreshStatus()
            app.leaveMainIfLoggedOut()
            await app.refreshSetup()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task {
                await app.refreshStatus()
                app.leaveMainIfLoggedOut()
            }
        }
    }

    /// `status --local` failed: nothing about this Mac is known.
    private var failed: Bool { app.status == nil && app.statusError != nil }

    private var hostState: String {
        if failed { return "Couldn't check" }
        return switch app.status?.serve.state {
        case "serving": app.status?.serve.ready == true ? "Running" : "Not ready"
        case "starting": "Starting…"
        case "error": "Stopped after an error"
        case nil: "Checking…"
        default: "Not running"
        }
    }
}

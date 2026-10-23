import SwiftUI

/// The main window until the main app lands: this Mac's latch as
/// `status --local` says, what onboarding set up, and Set up again.
struct MainView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Form {
            Section("This Mac") {
                LabeledContent("Logged in as", value: app.status?.peer?.name ?? "Not logged in")
                LabeledContent("Latch") {
                    Text(hostState).foregroundStyle(app.status?.serve.state == "serving" ? Brand.volt : .secondary)
                }
                if let blocked = app.status?.serve.blocked ?? app.status?.lastError {
                    Text(blocked.message).foregroundStyle(.red)
                }
                if let update = app.status?.update, update.available {
                    LabeledContent("Update", value: update.build.map { "Build \($0) is available" } ?? "Available")
                }
                if let error = app.statusError {
                    Text(error).foregroundStyle(.red)
                }
            }
            Section("Set up") {
                ForEach(Array(Summary.rows(flow: nil, status: app.status, progress: app.progress).dropFirst().dropLast().enumerated()), id: \.offset) { _, row in
                    LabeledContent(row.title, value: row.state)
                }
                Button("Set up again…") { app.setUpAgain() }
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 420, minHeight: 360)
        .task { await app.refreshStatus() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            Task { await app.refreshStatus() }
        }
    }

    private var hostState: String {
        switch app.status?.serve.state {
        case "serving": "Running"
        case "starting": "Starting…"
        case "error": "Stopped after an error"
        case nil: "Checking…"
        default: "Not running"
        }
    }
}

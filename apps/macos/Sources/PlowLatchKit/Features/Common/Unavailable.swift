import SwiftUI

/// The app's empty and error states, each with its one action.
struct Unavailable: View {
    enum Kind {
        case notLoggedIn
        case sharingStopped
        case noActivity
        case unreachable(String)
    }

    var kind: Kind
    var action: () -> Void

    var body: some View {
        switch kind {
        case .notLoggedIn:
            ContentUnavailableView {
                Label("Not logged in", systemImage: "person.crop.circle.badge.questionmark")
            } description: {
                Text("Sign in to link this Mac to your Plow account.")
            } actions: {
                Button("Check Again", action: action)
            }
        case .sharingStopped:
            ContentUnavailableView {
                Label("Sharing is stopped", systemImage: "pause.circle")
            } description: {
                Text("Your devices and agents can't reach this Mac until sharing starts.")
            } actions: {
                Button("Start Sharing", action: action)
                    .disabled(true)
                    .help("Background sharing isn't available in this build yet.")
            }
        case .noActivity:
            ContentUnavailableView {
                Label("No activity yet", systemImage: "clock")
            } description: {
                Text("Requests from your devices and agents appear here as they happen.")
            } actions: {
                Button("Show Overview", action: action)
            }
        case let .unreachable(message):
            ContentUnavailableView {
                Label("Can't reach latch", systemImage: "exclamationmark.triangle")
            } description: {
                Text(message)
            } actions: {
                Button("Try Again", action: action)
            }
        }
    }
}

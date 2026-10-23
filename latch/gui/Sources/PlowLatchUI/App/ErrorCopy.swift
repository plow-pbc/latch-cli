import SwiftUI

/// latch's errors in the app's own words: each code the app knows gets a
/// sentence of its own; latch's message is only ever shown under Details.
nonisolated enum ErrorCopy {
    /// Where nothing else says what failed.
    static let fallback = "Latch couldn't do that."

    /// The app's sentence for `error`, naming `host` (the name the host
    /// picker shows) where it matters; nil when the app has none.
    static func sentence(_ error: LatchError, host: String? = nil) -> String? {
        let code: String
        switch error {
        case let .unreachable(sentence, _): return sentence
        case let .failed(failure): code = failure.code
        }
        let name = host ?? "the host"
        let Name = host ?? "The host"
        return switch code {
        case "host_unreachable": "Latch couldn't reach \(name), so nothing changed. Try again."
        case "host_offline": "\(Name) is offline, so nothing changed. Start Latch there, then try again."
        case "host_stopped": "Latch stopped on \(name). Try again once it's running."
        case "record_too_large": "Activity from \(name) stops at a record too large to send here. See the rest on \(name) itself."
        case "host_outdated":
            "\(Name)'s Latch can't check an import against its preview, so nothing was imported. Update Latch there, then import again."
        case "serve_required": "Latch on this Mac isn't answering. Start or restart it in Host Settings, then try again."
        case "other_version": "Another version of Latch is already running on this Mac. Stop it, then start this one."
        case "not_logged_in": "This Mac isn't logged in to Plow. Log in, then try again."
        case "plow_unreachable": "Plow can't be reached right now. Check your internet connection, then try again."
        case "plow_error": "Plow couldn't answer just now. Try again in a moment."
        case "plow_denied": "Plow didn't accept this Mac's login for that."
        case "gatekeeper_unavailable": "The Gatekeeper couldn't judge this. Try again in a moment."
        case "messages_not_installed": "Latch's Messages reader is unavailable. Install Latch again, then try again."
        default: nil
        }
    }

    /// The sentence, or the plain one where no title says what failed.
    static func said(_ error: LatchError, host: String? = nil) -> String {
        sentence(error, host: host) ?? fallback
    }

    /// Activity's host errors, which it retries on its own: the fact
    /// alone, without a step to take.
    static func fact(_ error: LatchError, host: String) -> String? {
        switch error.code {
        case "host_unreachable": "Latch couldn't reach \(host)."
        case "host_offline": "\(host) is offline."
        case "host_stopped": "Latch stopped on \(host)."
        default: nil
        }
    }

    /// For a single line: Activity's fact, or the sentence's first sentence.
    static func short(_ error: LatchError, host: String) -> String {
        if let fact = fact(error, host: host) { return fact }
        let said = said(error, host: host)
        guard let end = said.range(of: ". ") else { return said }
        return String(said[..<end.lowerBound]) + "."
    }
}

extension LatchError {
    /// Any error as one the app can say: latch's own, or the system's with
    /// its description as the sentence.
    static func from(_ error: any Error) -> LatchError {
        error as? LatchError ?? .unreachable(error.localizedDescription)
    }
}

/// An error as the app says it: its sentence (with none under a title that
/// already says what failed, when the app has no sentence of its own), then
/// latch's own words under a collapsed Details.
struct ErrorText: View {
    var error: LatchError
    var host: String?
    /// A title above already says what failed.
    var titled = false
    /// The screen's own words for it, instead of the shared sentence.
    var said: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let sentence = said ?? ErrorCopy.sentence(error, host: host) ?? (titled ? nil : ErrorCopy.fallback) {
                Text(sentence)
            }
            if let details = error.details {
                DisclosureGroup("Details") {
                    Text(details)
                        .font(.caption)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .font(.caption)
            }
        }
    }
}

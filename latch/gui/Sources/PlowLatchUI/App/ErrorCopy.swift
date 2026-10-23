import SwiftUI

/// latch's errors in the app's own words: each code the app knows gets a
/// sentence of its own; latch's message is only ever shown under Details.
nonisolated enum ErrorCopy {
    /// Where nothing else says what failed.
    static let fallback = "Latch couldn't do that."
    /// How the sentences name this Mac's own host.
    static let thisMac = "this Mac"

    /// Where the error is said, for codes whose sentence depends on it.
    enum Place: Equatable, Sendable {
        case anywhere
        /// Try a request, previewing a request kind on `connector` (the
        /// request's own op) when it names one.
        case tryRequest(connector: String? = nil)
        /// Removing one of the account's listed devices.
        case devices
    }

    /// The app's sentence for `error`, naming `host` (the name the host
    /// picker shows, or `thisMac`) where it matters; nil when the app has
    /// none.
    static func sentence(_ error: LatchError, host: String? = nil, place: Place = .anywhere) -> String? {
        let failure: LatchFailure
        switch error {
        case let .unreachable(sentence, _): return sentence
        case let .failed(failed): failure = failed
        }
        let name = host ?? "the host"
        let Name = host == thisMac ? "This Mac" : host ?? "The host"
        if case let .tryRequest(op) = place, let said = trying(failure, connector: failure.plugin ?? op, host: host) {
            return said
        }
        if place == .devices, failure.code == "not_found" { return "That device isn't in your account any more." }
        return switch failure.code {
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
        // Rules.
        case "rules_too_long":
            "These rules are too long to save. The rules for every agent and any one agent's own rules must fit within 8 KB together. Shorten them, then save again."
        case "bad_rules": "Latch can't read the rules saved on \(name). Check Details."
        case "not_previewable": "Only exec, file, google, messages and contacts requests can be tried here."
        // Vault.
        case "bad_item": "This item wasn't saved because Latch can't use one of its values. Check Details."
        case "bad_values": "Latch couldn't read this item's fields. Nothing was saved."
        case "stdin_too_big": "This item's fields are too large to save. Use less text, then try again."
        case "no_such_item": "That item isn't in the vault on \(name) any more."
        case "bad_import":
            "Latch can't read this file as a passwords export. It reads exports from Apple Passwords, 1Password, Chrome and Bitwarden; Details says what's wrong with this one."
        case "file_too_large": "This file is too large to import. Choose a smaller export file."
        case "local_file": "Latch couldn't read this file. Choose it again, or pick another."
        case "bad_pick": "The selected rows no longer match this export, so nothing was imported. Choose the file again."
        case "import_expired": "The import timed out before anything was saved. Start the import again."
        // Agents, hosts and devices.
        case "bad_name": "Give the agent a name without control characters, such as tabs or line breaks."
        case "line_taken": "That phone line already has an agent. Choose a free line."
        case "no_such_line": "That phone line couldn't be found. Choose another line."
        case "unknown_cloud_kind": "That cloud agent isn't available. Choose another cloud agent."
        case "token_file_exists":
            "A file with that name is already there, and Latch saves a key only to a new file, so no agent was made. Choose a new name."
        case "token_file_unwritable": "Latch couldn't create the key file there, so no agent was made. Choose another folder."
        case "invalid_request": "Plow didn't accept these details. Check them, then try again."
        case "no_such_agent": "That agent isn't in your account any more."
        case "unknown_host": "\(Name) isn't one of your hosts any more."
        case "own_machine": "This is the Mac you're using. Log out in Settings to remove it."
        // Logging in and starting.
        case "not_configured": "Latch is missing information it needs to log in."
        case "nothing_to_start_again": "Latch has no saved start settings. Try starting Latch again."
        default: nil
        }
    }

    /// Try a request's own sentences: a request it can't try as typed, and
    /// the host's refusals in its verdict.
    private static func trying(_ failure: LatchFailure, connector: String?, host: String?) -> String? {
        let name = host ?? "the host"
        let which = connector.map { "The \(PluginCopy.title($0)) connector" } ?? "This connector"
        return switch failure.code {
        case "usage", "bad_request": "Latch can't try this request as typed. Details says what to change."
        case "exec_denied": "This agent can't run programs on \(name)."
        case "not_enabled" where host == thisMac: "\(which) is off on this Mac. Turn it on in Host Settings."
        case "not_enabled": "\(which) is off on \(name). Turn it on there."
        case "bad_path": "Latch can't use that path on \(name). Check Details."
        default: nil
        }
    }

    /// The sentence, or the plain one where no title says what failed.
    static func said(_ error: LatchError, host: String? = nil, place: Place = .anywhere) -> String {
        sentence(error, host: host, place: place) ?? fallback
    }

    /// A sentence's first sentence, for a single line.
    static func first(_ said: String) -> String {
        guard let end = said.range(of: ". ") else { return said }
        return String(said[..<end.lowerBound]) + "."
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
        fact(error, host: host) ?? first(said(error, host: host))
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
    var place = ErrorCopy.Place.anywhere

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let sentence = said ?? ErrorCopy.sentence(error, host: host, place: place) ?? (titled ? nil : ErrorCopy.fallback) {
                Text(sentence)
            }
            ErrorDetails(error: error)
        }
    }
}

/// latch's own words for an error, collapsed under Details; nothing when
/// it said none.
struct ErrorDetails: View {
    var error: LatchError

    var body: some View {
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

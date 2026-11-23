import Foundation
import Testing
@testable import PlowLatchUI

/// latch's errors in the app's words: no command, flag, URL or pid ever
/// reaches the sentence; latch's own message is kept for Details.
nonisolated struct ErrorCopyTests {
    static let codes = ["host_unreachable", "host_offline", "host_stopped", "record_too_large", "host_outdated",
                        "serve_required", "other_version", "not_logged_in", "plow_unreachable", "plow_error",
                        "plow_denied", "gatekeeper_unavailable", "messages_not_installed",
                        // The person's own input, and things removed elsewhere.
                        "rules_too_long", "bad_rules", "bad_item", "bad_values", "stdin_too_big",
                        "no_such_item", "bad_import", "file_too_large", "local_file", "bad_pick", "import_expired",
                        "bad_name", "line_taken", "no_such_line", "unknown_cloud_kind", "credential_file_exists",
                        "credential_file_unwritable", "invalid_request", "no_such_agent", "unknown_host", "own_machine",
                        "not_configured", "nothing_to_start_again"]
    /// The device list's own codes.
    static let placed: [(String, ErrorCopy.Place)] = [("not_found", .devices)]
    static let banned = ["`", "latch ", "--", "http", "pid", "serve", "/Users", "~/"]

    private static func failed(_ code: String) -> LatchError {
        .failed(LatchFailure(code: code, message: "a host from another latch version is running (pid 4242); run `latch serve --foreground` at https://plow.co"))
    }

    @Test(arguments: codes)
    func everyCodeHasASentenceWithNoCommandIn(_ code: String) throws {
        for host in ["studio", nil] {
            let error = Self.failed(code)
            let sentence = try #require(ErrorCopy.sentence(error, host: host))
            let lines = [sentence, ErrorCopy.short(error, host: host ?? "studio")] + [ErrorCopy.fact(error, host: "studio")].compactMap { $0 }
            for line in lines {
                for banned in Self.banned {
                    #expect(!line.contains(banned), "\(code): \(line)")
                }
                #expect(line.hasSuffix("."))
            }
            #expect(error.details == Self.failed(code).message)
        }
    }

    @Test func theHostIsNamedOrCalledTheHost() {
        #expect(ErrorCopy.sentence(Self.failed("host_offline"), host: "pi-kitchen") ==
            "pi-kitchen is offline, so nothing changed. Start Latch there, then try again.")
        #expect(ErrorCopy.sentence(Self.failed("host_offline")) ==
            "The host is offline, so nothing changed. Start Latch there, then try again.")
        #expect(ErrorCopy.sentence(Self.failed("host_unreachable")) == "Latch couldn't reach the host, so nothing changed. Try again.")
    }

    @Test func anUnknownCodeHasNoSentenceAndKeepsLatchsWordsForDetails() {
        let error = Self.failed("state_busy")
        #expect(ErrorCopy.sentence(error, host: "studio") == nil)
        #expect(ErrorCopy.said(error) == "Latch couldn't do that.")
        #expect(ErrorCopy.short(error, host: "studio") == "Latch couldn't do that.")
        #expect(error.details?.contains("latch serve") == true)
    }

    @Test func theAppsOwnErrorsSayTheirSentenceAndKeepTheirDetail() {
        let error = LatchError.unreachable("Latch couldn't complete this step.", detail: "exit 101: thread 'main' panicked")
        #expect(ErrorCopy.said(error) == "Latch couldn't complete this step.")
        #expect(error.details == "exit 101: thread 'main' panicked")
        #expect(LatchError.unreachable("This code expired.").details == nil)
    }

    @Test func activitySaysTheFactAloneOnOneLine() {
        #expect(ErrorCopy.short(Self.failed("host_stopped"), host: "studio") == "Latch stopped on studio.")
        #expect(ErrorCopy.short(Self.failed("record_too_large"), host: "studio") ==
            "Activity from studio stops at a record too large to send here.")
    }

    @Test func placedCodesHaveSentencesOnlyWhereTheyMeanSomething() throws {
        for (code, place) in Self.placed {
            for host in ["studio", ErrorCopy.thisMac, nil] {
                let sentence = try #require(ErrorCopy.sentence(Self.failed(code), host: host, place: place), "\(code)")
                for banned in Self.banned { #expect(!sentence.contains(banned), "\(code): \(sentence)") }
                #expect(sentence.hasSuffix("."))
            }
            // Elsewhere, the same code has no sentence: it can mean something else there.
            #expect(ErrorCopy.sentence(Self.failed(code), host: "studio") == nil, "\(code)")
        }
    }

    @Test func thisMacIsCalledThisMac() {
        #expect(ErrorCopy.sentence(Self.failed("bad_rules"), host: ErrorCopy.thisMac) == "Latch can't read what agents are always denied on this Mac. Check Details.")
        #expect(ErrorCopy.sentence(Self.failed("host_outdated"), host: ErrorCopy.thisMac)?.hasPrefix("This Mac's Latch") == true)
    }
}

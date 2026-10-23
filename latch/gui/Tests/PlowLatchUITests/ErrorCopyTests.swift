import Testing
@testable import PlowLatchUI

/// latch's errors in the app's words: no command, flag, URL or pid ever
/// reaches the sentence; latch's own message is kept for Details.
nonisolated struct ErrorCopyTests {
    static let codes = ["host_unreachable", "host_offline", "host_stopped", "record_too_large", "host_outdated",
                        "serve_required", "other_version", "not_logged_in", "plow_unreachable", "plow_error",
                        "plow_denied", "gatekeeper_unavailable", "messages_not_installed"]

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
                for banned in ["`", "latch ", "--", "http", "pid", "serve"] {
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
        let error = Self.failed("bad_rules")
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
}

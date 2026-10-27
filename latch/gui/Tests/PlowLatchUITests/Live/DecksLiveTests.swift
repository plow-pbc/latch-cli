import Foundation
import Testing
@testable import PlowLatchUI

/// Every example on the rules screen, as the screen asks it, on a real
/// latch and a host it serves: `LATCH_LIVE_LATCH`, and `LATCH_LIVE_STATE`,
/// a logged-in host's state folder. latch must take each request and
/// answer a verdict; which one is the gatekeeper's call, not this test's.
/// Each is a billed judgment. Built only with `LATCH_LIVE_TESTS=1`; a
/// selected test without its inputs fails.
@MainActor
struct DecksLiveTests {
    nonisolated static let env = ProcessInfo.processInfo.environment

    @Test func everyExampleGetsAVerdict() async throws {
        let latch = URL(filePath: try #require(Self.env["LATCH_LIVE_LATCH"], "LATCH_LIVE_LATCH is required"))
        let state = try #require(Self.env["LATCH_LIVE_STATE"], "LATCH_LIVE_STATE is required")
        let client = LiveLatchClient(executable: latch, stateDir: state)
        for deck in Onboarding.decks {
            for example in deck.examples {
                let answer = await AppModel.result {
                    try await client.rulesTry(example.request, host: nil, agent: nil, newAgent: true, draft: deck.rules)
                }
                guard case let .success(tried)? = answer else {
                    Issue.record("\(example.title): \(String(describing: answer))")
                    continue
                }
                #expect(["allow", "not_covered", "owner_said_no", "host_refused"].contains(tried.verdict), "\(example.title): \(tried.verdict)")
            }
        }
    }
}

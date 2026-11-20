import Foundation
import Testing
@testable import PlowLatchUI

/// The rules screen's two defaults, as the owner reads them and as latch
/// is asked them.
struct DecksTests {
    @Test func bothDecksArePinned() throws {
        let today = try #require(Calendar.current.date(from: DateComponents(year: 2026, month: 10, day: 26, hour: 9)))
        let decks = Onboarding.decks(from: today)
        let days = ["--from", "2026-10-26", "--to", "2026-10-28"]
        #expect(decks.map(\.name) == ["Chief of Staff", "Developer"])
        #expect(decks[0].rules == "Let my chief of staff read email, check my calendar and draft replies. Never let it email outside the company.")
        #expect(decks[1].rules == "Let my developer run tests, review changes and open pull requests. Never let it push directly to main.")
        let rows = decks.flatMap(\.examples)
        #expect(rows.map(\.title) == [
            "Search inbox for the kickoff thread", "List events for the next two days", "Save the launch follow-up as a draft",
            "Reply-all to the launch update thread",
            "Run inbox.test.js with Jest", "Show the diff for PR 73", "Create a PR for retry-count rollback",
            "Push release/candidate to origin",
        ])
        #expect(rows.map(\.request) == [
            .words(["google", "--", "gmail", "search", "subject:Kickoff newer_than:7d", "--max", "20"]),
            .words(["google", "--", "calendar", "events", "primary"] + days),
            .words(["google", "--", "gmail", "drafts", "create", "--to", "teammate@studio.example", "--subject", "Re: Launch timing",
                    "--body", "Could you share an updated estimate?"]),
            .words(["google", "--", "gmail", "send", "--reply-all", "--to", "launch@studio.example", "--cc", "partner@vendor.example",
                    "--subject", "Re: Launch update"]),
            .words(["exec", "--", "npm", "test", "--", "--runInBand", "inbox.test.js"]),
            .words(["exec", "--network", "--", "gh", "pr", "diff", "73"]),
            .words(["exec", "--network", "--", "gh", "pr", "create", "--base", "main", "--head", "rollback/retry-count",
                    "--title", "Restore retry count", "--body", "Proposes reverting the retry-count change."]),
            .words(["exec", "--network", "--", "git", "push", "--force", "origin", "release/candidate:refs/heads/main"]),
        ])
        // What the open row says is a sentence, never latch's words.
        #expect(rows.allSatisfy { $0.detail.hasSuffix(".") && !$0.detail.contains("latch") && !$0.detail.contains("--") })
        #expect(Set(rows.map(\.id)).count == rows.count)
    }

    /// The calendar row asks about the two days from whatever day it is.
    @Test func datesFollowToday() throws {
        let today = try #require(Calendar.current.date(from: DateComponents(year: 2027, month: 2, day: 25, hour: 23)))
        let calendar = Onboarding.decks(from: today)[0].examples[1].request.words ?? []
        #expect(calendar.suffix(4) == ["--from", "2027-02-25", "--to", "2027-02-27"])
    }
}

import Foundation
import Testing
@testable import PlowLatchUI

/// A host's device rule as a test sets it; another device may change it
/// meanwhile.
final class RulesClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var show = try! Fixtures.decode(Fixtures.studioRules) as RulesShow
    private(set) var sets: [(text: String, host: String?)] = []
    /// Holds each save until opened.
    var holdSet: Gate?
    /// Saved by another device right after the next save.
    var thenElsewhere: String?
    /// The next `rules show` fails.
    var failNextShow = false

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow {
        if failNextShow {
            failNextShow = false
            throw LatchError.unreachable("timed out")
        }
        return show
    }
    /// As `rules set --revision R` does: refused with `stale_revision`
    /// unless the rule is still at R (nil: none); an empty text is saved.
    func rulesSet(_ text: String, host: String?, revision: String?) async throws {
        await holdSet?.wait()
        let current = show.deviceRule?.revision
        guard current == revision else {
            throw LatchError.failed(LatchFailure(code: "stale_revision", message: "the rule changed; it is at \(current ?? "none")"))
        }
        sets.append((text, host))
        show.deviceRule = .init(text: text, revision: RulesShow.Rule.revision(of: text))
        if let elsewhere = thenElsewhere {
            thenElsewhere = nil
            sets.append((elsewhere, host))
            show.deviceRule = .init(text: elsewhere, revision: RulesShow.Rule.revision(of: elsewhere))
        }
    }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func agents(host: String?) async throws -> AgentsList { try Fixtures.decode(host == nil ? Fixtures.agents : Fixtures.agentsStudio) }
    /// `audit suggest`'s answer for a record, as the test scripts it.
    var suggested: (String) throws -> String = { _ in #"{"suggestion":null}"# }
    func auditSuggest(_ record: String, host: String?) async throws -> AuditSuggest {
        try Fixtures.decode(try suggested(record))
    }
    func auditJudge(_ id: String, host: String?) async throws -> String {
        try Fixtures.keptCalls[id] ?? { throw LatchError.failed(LatchFailure(code: "no_such_request", message: "no \(id)")) }()
    }
    func audit(host: String?, before: String?) async throws -> AuditPage {
        try AuditPage.parse(Data((host == nil ? Fixtures.studioAudit : Fixtures.kitchenAudit).utf8))
    }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct RulesTests {
    private func app(_ client: RulesClient) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(.rules)
        await app.enter(.host(.rules, app.host))
        app.ruleSaveDelay = .seconds(600)
        return app
    }

    // Unanswered when the question was asked: an answer given meanwhile
    // on another device isn't replaced; it shows instead.
    @Test func aFirstAnswerNeverReplacesOneGivenMeanwhile() async throws {
        let client = RulesClient()
        client.show.deviceRule = nil
        let app = try await app(client)
        app.typeRule("Never send email.", on: "studio")
        try await client.rulesSet("Answered on the laptop.", host: nil, revision: nil)
        // Rules read again: the draft still began from no rule.
        await app.enter(app.route, refresh: true)
        await app.saveRuleDraft("studio")
        #expect(client.sets.map(\.text) == ["Answered on the laptop."])
        #expect(app.rulesDrafts["studio"] == nil && app.rulesErrors["studio"] == nil)
        #expect(app.savedRule("studio")?.text == "Answered on the laptop.")
    }

    // A latch from before device rules says nothing of one, where a
    // current latch says null when it was never set.
    @Test func anOlderLatchIsToldFromOneNeverAnswered() throws {
        let older: RulesShow = try Fixtures.decode(#"{"agents":{},"rules":[],"host":"studio"}"#)
        let unanswered: RulesShow = try Fixtures.decode(Fixtures.piRules)
        #expect(older.older && !unanswered.older && unanswered.deviceRule == nil)
    }

    @Test func aDraftSurvivesGoingToAnotherScreenAndHostAndBack() async throws {
        let client = RulesClient()
        let app = try await app(client)
        app.typeRule("Never touch ~/Taxes.", on: "studio")
        for (screen, host) in [(HostScreen.home, "studio"), (.rules, "pi-kitchen"), (.vault, "pi-kitchen"), (.rules, "studio")] {
            app.place = .host(screen)
            app.chosenHost = host
            await app.enter(app.route)
        }
        #expect(app.rulesDrafts["studio"]?.text == "Never touch ~/Taxes.")
        #expect(client.sets.isEmpty)
    }

    /// Typing pauses and the rule saves itself; typing on during that save
    /// is saved after it, over the revision it made.
    @Test func aRuleSavesWhenTypingPausesAndTypingDuringASaveIsSavedAfter() async throws {
        let client = RulesClient()
        let app = try await app(client)
        app.ruleSaveDelay = .zero
        let held = Gate()
        client.holdSet = held
        app.typeRule("Never touch ~/Taxes.", on: "studio")
        await waitUntil { app.savingRules.contains("studio") }
        app.typeRule("Never touch ~/Taxes or ~/Finance.", on: "studio")
        held.open()
        await waitUntil { app.rulesDrafts["studio"] == nil && !app.savingRules.contains("studio") }
        #expect(client.sets.map(\.text) == ["Never touch ~/Taxes.", "Never touch ~/Taxes or ~/Finance."])
        #expect(app.savedRule("studio")?.text == "Never touch ~/Taxes or ~/Finance.")
        #expect(app.rulesSaved.contains("studio"))
    }

    /// The typing behind a save is saved over the revision that save made,
    /// read back or not; a rule saved elsewhere meanwhile is refused as
    /// stale, and it shows instead.
    @Test(arguments: [false, true])
    func typingBehindASaveRespectsItsRevision(changedElsewhere: Bool) async throws {
        let client = RulesClient()
        let app = try await app(client)
        let held = Gate()
        client.holdSet = held
        if changedElsewhere { client.thenElsewhere = "Changed on the laptop." } else { client.failNextShow = true }
        app.typeRule("Never touch ~/Taxes.", on: "studio")
        let saving = Task { await app.saveRuleDraft("studio") }
        await waitUntil { app.savingRules.contains("studio") }
        app.typeRule("Never touch ~/Taxes or ~/Finance.", on: "studio")
        held.open()
        await saving.value
        let last = changedElsewhere ? "Changed on the laptop." : "Never touch ~/Taxes or ~/Finance."
        #expect(client.sets.map(\.text) == ["Never touch ~/Taxes.", last])
        #expect(app.rulesDrafts["studio"] == nil && app.savedRule("studio")?.text == last)
    }
}

/// Rules' last allowed and refused requests: the rule they were judged by,
/// in words, and the edit the judge says would rule each the other way.
@MainActor
struct RulesCoachTests {
    private func app(_ client: RulesClient) async throws -> (AppModel, HostEntry) {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(.rules)
        await app.enter(.host(.rules, app.host))
        let studio = try #require(app.host)
        app.feed(for: studio).stage(Fixtures.studioAudit)
        return (app, studio)
    }

    /// Rules presents latch's checked edit, and Apply saves it; an edit of
    /// a rule that has changed since isn't offered.
    @Test func latchsSuggestionIsShownAndApplySavesIt() async throws {
        let client = RulesClient()
        let (app, studio) = try await app(client)
        let allowed = try #require(app.lastJudged(on: studio, refused: false))
        #expect(allowed.command == "latch exec cat Projects/latch-demo/README.md")
        let never = "Never touch ~/Finance or my tax returns. Never run `cat Projects/latch-demo/README.md`."
        let revision = try #require(app.savedRule(studio.name)?.revision)
        client.suggested = { record in
            #expect(record == allowed.id)
            return #"{"suggestion":"rule","rule":"\#(never)","revision":"\#(revision)"}"#
        }
        await app.suggest(for: allowed, on: studio)
        #expect(app.suggestion(for: allowed, on: studio) == .rule(never))
        await app.applyRule(never, on: studio.name)
        #expect(client.sets.map(\.text) == [never])
        await app.suggest(for: allowed, on: studio)
        #expect(app.suggestion(for: allowed, on: studio) == .nothing)
    }

    /// A check that failed is asked again the next time, not kept as none.
    @Test func aFailedCheckIsAskedAgain() async throws {
        let client = RulesClient()
        let (app, studio) = try await app(client)
        let notCovered = try #require(app.feed(for: studio).records.last { $0.gate?.verdict == "not_covered" })
        client.suggested = { _ in throw LatchError.failed(LatchFailure(code: "plow_unreachable", message: "no Plow")) }
        await app.suggest(for: notCovered, on: studio)
        #expect(app.suggestion(for: notCovered, on: studio) == .nothing)
        client.suggested = { _ in #"{"suggestion":"ask_in_message"}"# }
        await app.suggest(for: notCovered, on: studio)
        #expect(app.suggestion(for: notCovered, on: studio) == .askInMessage)
    }

    /// A judgment names its rule by revision: the rule now when it's at that
    /// revision, else the text its save kept, else none.
    @Test func aJudgmentsRuleIsQuotedByItsRevision() async throws {
        let (app, studio) = try await app(RulesClient())
        app.feed(for: studio).stage(#"""
        {"cursor":"9f00","record":{"agent":null,"caller":"6b55a78f","caller_name":"studio","decision":"allow","details":{"args":{"after":"abc1230000000000ffff","before":null,"command":"set","text":"Never send email."}},"id":"9f00","op":"rules","outcome":"ok","time":"2026-10-23T04:50:00Z","command":"latch rules set"},"type":"record"}
        """#)
        #expect(app.ruleText("ea4ac6c6bd9184d7", on: studio) == "Never touch ~/Finance or my tax returns.")
        #expect(app.ruleText("abc1230000000000", on: studio) == "Never send email.")
        #expect(app.ruleText("0000000000000000", on: studio) == nil)
    }

    /// The window opens on the newest refusal of the last 15 minutes, on
    /// any host; its banner opens that host's Rules.
    @Test func aRecentRefusalOnAnyHostOpensItsRules() async throws {
        let (app, _) = try await app(RulesClient())
        let kitchen = try #require(try AuditPage.parse(Data(Fixtures.kitchenAudit.utf8)).records.last { $0.outcome == "gatekeeper_denied" })
        await app.noticeDenials(now: kitchen.time.addingTimeInterval(14 * 60))
        #expect(app.denial == Denial(host: "pi-kitchen", request: "latch exec rm recipes/old.txt"))
        app.reviewDenial()
        #expect(app.route == .host(.rules, app.hosts.first { $0.name == "pi-kitchen" }))
        #expect(app.denial == nil)
        await app.noticeDenials(now: kitchen.time.addingTimeInterval(16 * 60))
        #expect(app.denial == nil)
    }
}

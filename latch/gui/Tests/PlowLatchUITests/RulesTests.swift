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

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow {
        show
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
        show.deviceRule = .init(text: text, revision: "r\(sets.count)")
    }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func agents(host: String?) async throws -> AgentsList { try Fixtures.decode(host == nil ? Fixtures.agents : Fixtures.agentsStudio) }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct RulesTests {
    private func app(_ client: RulesClient) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(.activity)
        await app.enter(.host(.activity, app.host))
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
        // Activity read again: the draft still began from no rule.
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
        for (screen, host) in [(HostScreen.home, "studio"), (.activity, "pi-kitchen"), (.vault, "pi-kitchen"), (.activity, "studio")] {
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
}

import Foundation
import Testing
@testable import PlowLatchUI

/// A host's device rule as a test sets it; another device may change it
/// meanwhile.
final class RulesClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var show = try! Fixtures.decode(Fixtures.studioRules) as RulesShow
    private(set) var sets: [(text: String, host: String?)] = []
    /// `rules show` fails once a rule was set.
    var readFailsAfterSet = false

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow {
        if readFailsAfterSet, !sets.isEmpty { throw LatchError.failed(LatchFailure(code: "state_busy", message: "busy")) }
        return show
    }
    /// As `rules set --revision R` does: refused with `stale_revision`
    /// unless the rule is still at R (nil: none); an empty text is saved.
    func rulesSet(_ text: String, host: String?, revision: String?) async throws {
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
        return app
    }

    // A rule changed on another device since editing began is never
    // replaced: the save is refused and nothing is written.
    @Test func aStaleSaveNeverReplacesANewerRule() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let host = try #require(app.host)
        let started = client.show.deviceRule?.revision
        try await client.rulesSet("Changed on the laptop.", host: nil, revision: started)
        await #expect(throws: LatchError.unreachable(AppModel.changedElsewhere)) {
            try await app.saveRules("My edit.", on: host, revision: started)
        }
        #expect(client.sets.map(\.text) == ["Changed on the laptop."])
    }

    // Unanswered when the question was asked: an answer given meanwhile
    // on another device isn't replaced.
    @Test func aFirstAnswerNeverReplacesOneGivenMeanwhile() async throws {
        let client = RulesClient()
        client.show.deviceRule = nil
        let app = try await app(client)
        try await client.rulesSet("Answered on the laptop.", host: nil, revision: nil)
        await app.saveRuleDraft("studio", answer: true)
        #expect(client.sets.map(\.text) == ["Answered on the laptop."])
        #expect(app.rulesErrors["studio"]?.message == AppModel.changedElsewhere)
    }

    // The gatekeeper question is asked while this Mac's rule was never
    // set; a blank answer is saved, so it's never asked again.
    @Test func aBlankAnswerIsSavedAndTheQuestionNeverAskedAgain() async throws {
        let client = RulesClient()
        client.show.deviceRule = nil
        let app = try await app(client)
        await app.askGatekeeperIfUnanswered()
        let host = try #require(app.host)
        #expect(app.sheet == .gatekeeper(host))
        await app.saveRuleDraft("studio", answer: true)
        #expect(client.sets.map(\.text) == [""])
        #expect(app.savedRule("studio")?.text == "")
        #expect(app.sheet == nil)
        await app.askGatekeeperIfUnanswered()
        #expect(app.sheet == nil)
    }

    // Saved, the question goes even when the read after the save fails.
    @Test func anAnswerSavedClosesTheQuestionThoughTheReadAfterFails() async throws {
        let client = RulesClient()
        client.show.deviceRule = nil
        let app = try await app(client)
        await app.askGatekeeperIfUnanswered()
        client.readFailsAfterSet = true
        await app.saveRuleDraft("studio", answer: true)
        #expect(client.sets.map(\.text) == [""])
        #expect(app.sheet == nil && app.rulesErrors["studio"] == nil)
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
        #expect(app.ruleChanged("studio"))
        #expect(client.sets.isEmpty)
    }

    @Test func aDraftOverARuleChangedElsewhereStaysWithWhy() async throws {
        let client = RulesClient()
        let app = try await app(client)
        app.typeRule("Never touch ~/Taxes.", on: "studio")
        try await client.rulesSet("Changed on the laptop.", host: nil, revision: client.show.deviceRule?.revision)
        await app.saveRuleDraft("studio")
        #expect(client.sets.count == 1)
        #expect(app.rulesDrafts["studio"] != nil)
        #expect(app.rulesErrors["studio"]?.message == AppModel.changedElsewhere)
        await app.revertRule("studio")
        #expect(app.rulesDrafts["studio"] == nil && app.rulesErrors["studio"] == nil)
        #expect(app.savedRule("studio")?.text == "Changed on the laptop.")
    }
}

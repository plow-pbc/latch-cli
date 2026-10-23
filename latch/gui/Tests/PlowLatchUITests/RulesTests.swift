import Foundation
import Testing
@testable import PlowLatchUI

/// A host's rules as a test sets them; `rules set` replaces, `rules clear`
/// (empty text) removes, and another device may change them meanwhile.
final class RulesClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var show = try! Fixtures.decode(Fixtures.rulesStudio) as RulesShow
    private(set) var sets: [(text: String, host: String?, agent: String?)] = []
    private(set) var tries: [[String]] = []

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func rulesShow(host: String?) async throws -> RulesShow { show }
    func rulesSet(_ text: String) async throws {}
    func rulesSet(_ text: String, host: String?, agent: String?) async throws {
        sets.append((text, host, agent))
        if let agent {
            show.agents = (show.agents ?? []).filter { $0.agentUid != agent }
                + (text.isEmpty ? [] : [.init(agentName: nil, agentUid: agent, text: text, revision: "r\(sets.count)")])
        } else {
            show.everyAgent = text.isEmpty ? nil : .init(text: text, revision: "r\(sets.count)")
        }
    }
    func pluginsList(host: String?) async throws -> PluginsList { try Fixtures.decode(#"{"plugins":[]}"#) }
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func agents(host: String?) async throws -> AgentsList { try Fixtures.decode(host == nil ? Fixtures.agents : Fixtures.agentsStudio) }
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        tries.append(request)
        return try Fixtures.decode(Fixtures.tries["allow-owner"]!)
    }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct RulesTests {
    private func app(_ client: RulesClient) throws -> (AppModel, HostEntry) {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return (app, try #require(app.host))
    }

    @Test func readsEachAgentsRulesAndWhoIsRefusedExec() throws {
        let studio: RulesShow = try Fixtures.decode(Fixtures.rulesStudio)
        let scout = try #require(studio.agents?.first)
        #expect(scout.agentName == "scout" && scout.text?.contains("latch-demo") == true && scout.revision != nil)
        #expect(studio.everyAgent?.revision != nil)
        let kitchen: RulesShow = try Fixtures.decode(Fixtures.rulesKitchen)
        let helper = try #require((try Fixtures.decode(Fixtures.agents) as AgentsList).agents.first { $0.name == "helper" })
        #expect(kitchen.execBlocked(helper.uid))
        #expect(!studio.execBlocked(helper.uid))
    }

    @Test func standingsMarkOwnRulesAndExecRefusedPerHost() async throws {
        let (app, _) = try app(RulesClient())
        app.stage(agents: try Fixtures.decode(Fixtures.agents), standings: [
            "studio": .success(try Fixtures.decode(Fixtures.agentsStudio)),
            "pi-kitchen": .success(try Fixtures.decode(Fixtures.agentsKitchen)),
            "sidecar": .failure(.failed(LatchFailure(code: "host_offline", message: "offline"))),
        ])
        let scout = try #require(app.agents?.first { $0.name == "scout" })
        let helper = try #require(app.agents?.first { $0.name == "helper" })
        guard case let .success(studio)? = app.standings["studio"], case let .success(kitchen)? = app.standings["pi-kitchen"] else {
            Issue.record("standings missing"); return
        }
        #expect(studio[scout.uid]?.ownRules == true)
        #expect(kitchen[helper.uid]?.execBlocked == true)
        #expect(kitchen[scout.uid]?.ownRules == false)
    }

    @Test func savingAnAgentsRulesLeavesEveryAgentsAlone() async throws {
        let client = RulesClient()
        let (app, host) = try app(client)
        let scout = try #require(client.show.agents?.first)
        let everyAgent = client.show.everyAgent
        try await app.saveRules("scout may also run tests.", on: host, agent: scout.agentUid, revision: scout.revision)
        #expect(client.sets.count == 1)
        #expect(client.sets[0].agent == scout.agentUid && client.sets[0].host == nil)
        #expect(client.show.everyAgent == everyAgent)
        #expect(client.show.agent(scout.agentUid!)?.text == "scout may also run tests.")
    }

    // Rules changed on another device since editing began are never
    // replaced: the save is refused and nothing is written.
    @Test func aStaleSaveNeverReplacesNewerRules() async throws {
        let client = RulesClient()
        let (app, host) = try app(client)
        let started = client.show.everyAgent?.revision
        try await client.rulesSet("Changed on the laptop.", host: nil, agent: nil)
        await #expect(throws: LatchError.unreachable(AppModel.changedElsewhere)) {
            try await app.saveRules("My edit.", on: host, agent: nil, revision: started)
        }
        #expect(client.sets.count == 1)
        #expect(client.show.everyAgent?.text == "Changed on the laptop.")
    }

    @Test func requestsSplitAsAShellWould() {
        #expect(ShellWords.split(#"exec --write ~/site -- npm run build"#) == ["exec", "--write", "~/site", "--", "npm", "run", "build"])
        #expect(ShellWords.split(#"exec -- cat "My Notes/a b.txt" 'it''s'"#) == ["exec", "--", "cat", "My Notes/a b.txt", "its"])
        #expect(ShellWords.split(#"exec -- echo a\ b"#) == ["exec", "--", "echo", "a b"])
        #expect(ShellWords.split(#"exec -- echo "open"#) == nil)
    }

    @Test func everyVerdictHasItsWords() throws {
        var titles: [String: String] = [:]
        for (name, json) in Fixtures.tries {
            titles[name] = VerdictView(verdict: try Fixtures.decode(json)).title
        }
        #expect(titles["allow-owner"] == "Would be allowed")
        #expect(titles["not-covered"] == "Not covered: would be refused")
        #expect(titles["said-no"] == "You said no: would be refused")
        #expect(titles["no-intent"] == "No instructions from you: would be refused")
        #expect(titles["host-refused"] == "The host refuses it: would be refused")
        let refused: RulesTry = try Fixtures.decode(Fixtures.tries["host-refused"]!)
        #expect(refused.refused?.code == "exec_denied")
    }
}

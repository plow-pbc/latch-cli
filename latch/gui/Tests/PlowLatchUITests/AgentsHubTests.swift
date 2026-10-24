import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// Agents' Remove… and Rules buttons: a removal names the agent by uid, and
/// a request for an agent's rules is taken only by that host's editor.
@MainActor
struct AgentsHubTests {
    /// Two agents both called scout; records each removal's uid.
    final class TwinsClient: LatchClient, @unchecked Sendable {
        let stateDir = "/s"
        private let removed = Mutex<[String]>([])
        var removals: [String] { removed.withLock { $0 } }
        var refuse: LatchError?
        var listing: LatchError?
        static let twins = #"{"agents":[{"kind":"own","line":{"name":null,"number":"+15550000002"},"name":"scout","standing":null,"status":null,"uid":"00000000000000000000000000000001"},{"kind":"own","line":{"name":null,"number":"+15550000003"},"name":"scout","standing":null,"status":null,"uid":"00000000000000000000000000000002"}]}"#

        func agents(host: String?) async throws -> AgentsList {
            if let listing { throw listing }
            let listed: AgentsList = try Fixtures.decode(Self.twins)
            return AgentsList(agents: listed.agents.filter { !removals.contains($0.uid) })
        }
        func removeAgent(_ uid: String) async throws {
            if let refuse { throw refuse }
            removed.withLock { $0.append(uid) }
        }
    }

    private func app(_ client: TwinsClient) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        await app.refreshAgentList()
        return app
    }

    @Test func removingAnAgentNamesItByUidAndListsTheAgentsAgain() async throws {
        let client = TwinsClient()
        let app = try await app(client)
        let second = try #require(app.agents?.last)
        await app.removeAgent(second)
        #expect(client.removals == ["00000000000000000000000000000002"])
        #expect(app.agents?.map(\.uid) == ["00000000000000000000000000000001"])
        #expect(app.agentRemoveError == nil)
    }

    // Removed, then the list can't be read again: the removed agent isn't
    // still shown with its buttons.
    @Test func aRemovedAgentGoesEvenWhenTheListCantBeReadAgain() async throws {
        let client = TwinsClient()
        let app = try await app(client)
        client.listing = .unreachable("Plow didn't answer")
        await app.removeAgent(try #require(app.agents?.last))
        #expect(app.agents?.map(\.uid) == ["00000000000000000000000000000001"])
        #expect(app.agentRemoveError == nil)
    }

    @Test func aRefusedRemovalIsKeptToBeShownAndTheAgentStays() async throws {
        let client = TwinsClient()
        client.refuse = .failed(LatchFailure(code: "outcome_unknown", message: "Plow may or may not have removed scout"))
        let app = try await app(client)
        await app.removeAgent(try #require(app.agents?.first))
        #expect(app.agentRemoveError?.code == "outcome_unknown")
        #expect(app.agents?.count == 2)
    }

    // A request for helper's rules on pi-kitchen: taken only by
    // pi-kitchen's helper editor, once; gone if another host is picked
    // first, so it can't take the focus there or later.
    @Test func aRulesRequestIsTakenOnlyByItsHostsEditor() throws {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        let kitchen = try #require(app.hosts.first { $0.name == "pi-kitchen" })
        let helper = "3971582e59baad83250141c8dcc95855", scout = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd"
        app.openRules(kitchen, agent: helper)
        #expect(!app.takeRulesFocus(AgentTarget(host: "studio", agent: helper)))
        #expect(!app.takeRulesFocus(AgentTarget(host: "pi-kitchen", agent: scout)))
        #expect(app.takeRulesFocus(AgentTarget(host: "pi-kitchen", agent: helper)))
        #expect(!app.takeRulesFocus(AgentTarget(host: "pi-kitchen", agent: helper)))

        app.openRules(kitchen, agent: helper)
        app.showHost("studio")
        #expect(app.rulesFocus == nil)
        #expect(!app.takeRulesFocus(AgentTarget(host: "pi-kitchen", agent: helper)))
    }
}

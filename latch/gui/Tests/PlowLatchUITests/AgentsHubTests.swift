import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// Agents' Remove… and Rules buttons: a removal names the agent by uid, and
/// a request for an agent's rules is taken only by that host's editor. And
/// how Agents names an agent, writes its number and says its standing on
/// each host.
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

    // Removed, then listed again, or the list can't be read again: either
    // way the removed agent isn't still shown with its buttons.
    @Test(arguments: [false, true])
    func removingAnAgentNamesItsUidAndRemovesIt(_ listingFails: Bool) async throws {
        let client = TwinsClient()
        let app = try await app(client)
        client.listing = listingFails ? .unreachable("Plow didn't answer") : nil
        await app.removeAgent(try #require(app.agents?.last))
        #expect(client.removals == ["00000000000000000000000000000002"])
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

    // The one picked stays picked on a return to Agents; with none picked,
    // or the picked one gone, the first is shown.
    @Test func agentsShowsThePickedAgentWhileItsListed() async throws {
        let client = TwinsClient()
        let app = try await app(client)
        #expect(app.shownAgent?.uid == "00000000000000000000000000000001")
        app.selectedAgent = "00000000000000000000000000000002"
        app.place = .host(.home)
        app.place = .account(.agents)
        #expect(app.shownAgent?.uid == "00000000000000000000000000000002")
        await app.removeAgent(try #require(app.shownAgent))
        #expect(app.shownAgent?.uid == "00000000000000000000000000000001")
    }

    // A line's name in Plow, else the handle, else the number (nil: as
    // this Mac's region writes it).
    @Test(arguments: [
        (#"{"name":"Aspen","number":"+16505550136"}"#, "openclaw-test2", "Aspen"),
        (#"{"name":"  ","number":"+16505550136"}"#, "openclaw-test2", "openclaw-test2"),
        (#"{"name":null,"number":"+16505550136"}"#, "", nil),
    ])
    func anAgentIsCalledByItsLinesNameThenItsHandleThenItsNumber(_ line: String, _ handle: String, _ shown: String?) throws {
        let agent: AgentsList.Agent = try Fixtures.decode(#"{"uid":"u1","name":"\#(handle)","kind":"cloud","line":\#(line)}"#)
        #expect(agent.displayName == shown ?? PhoneNumber("+16505550136")?.formatted())
    }

    @Test func phoneNumbersAreWrittenAsTheirRegionWritesThem() {
        #expect(PhoneNumber("+16505550136")?.formatted(region: "US") == "(650) 555-0136")
        #expect(PhoneNumber("+16505550136")?.formatted(region: "GB") == "+1 650-555-0136")
        #expect(PhoneNumber("+442079460958")?.formatted(region: "US") == "+44 20 7946 0958")
        #expect(PhoneNumber("+442079460958")?.formatted(region: "GB") == "020 7946 0958")
        #expect(PhoneNumber("+5511987654321")?.formatted(region: "US") == "+55 11 98765-4321")
        #expect(PhoneNumber("+5511987654321")?.sms == URL(string: "sms:+5511987654321"))
        #expect(PhoneNumber("agent@plow.co") == nil)
        #expect(PhoneNumber("16505550136") == nil)
    }

    // Each host's row says what's known of the agent there: offline (this
    // Mac never is), not read yet, unreadable, or its rules and exec.
    @Test func eachHostRowSaysTheAgentsStandingThere() throws {
        let agent: AgentsList.Agent = try Fixtures.decode(#"{"uid":"u1","name":"scout","kind":"own"}"#)
        let pi = HostEntry(name: "pi", isThisMac: false, online: true), mac = HostEntry(name: "mac", isThisMac: true, online: false)
        func standing(own: Bool, blocked: Bool) -> Result<[String: AgentsList.Agent.Standing], LatchError> {
            .success(["u1": .init(ownRules: own, execBlocked: blocked)])
        }
        #expect(agent.standing(on: HostEntry(name: "pi", isThisMac: false, online: false), standing(own: true, blocked: false))
            == "Offline, so its rules there can't be checked")
        #expect(agent.standing(on: pi, nil) == "Checking…")
        #expect(agent.standing(on: mac, standing(own: false, blocked: false)) == "Shared rules only")
        #expect(agent.standing(on: pi, standing(own: true, blocked: false)) == "Shared rules + own rules")
        #expect(agent.standing(on: pi, standing(own: false, blocked: true)) == "Shared rules · can't run programs")
        #expect(agent.standing(on: pi, standing(own: true, blocked: true)) == "Shared rules + own rules · can't run programs")
        #expect(agent.standing(on: pi, .success([:])) == "Shared rules only")
        #expect(agent.standing(on: pi, .failure(.unreachable("no answer"))) == ErrorCopy.said(.unreachable("no answer"), host: "pi"))
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

/// New Agent starts an agent by text, elsewhere: the sheet reads the
/// account's agents until a new one appears, then closes on it.
@MainActor
struct NewAgentWaitTests {
    /// scout, then also a texted-for agent once it has been listed `after`
    /// times; counts the reads of the list (not of a host's standing).
    final class TextedClient: LatchClient, @unchecked Sendable {
        let stateDir = "/s"
        let after: Int
        private let reads = Mutex(0)
        var listed: Int { reads.withLock { $0 } }
        init(after: Int) { self.after = after }

        func agents(host: String?) async throws -> AgentsList {
            let count = host != nil ? listed : reads.withLock { $0 += 1; return $0 }
            let scout = #"{"kind":"own","line":{"name":null,"number":"+15550000002"},"name":"scout","uid":"00000000000000000000000000000001"}"#
            let willow = #"{"kind":"cloud","line":{"name":"Willow","number":"+15550000003"},"name":"willow","status":"provisioning","uid":"00000000000000000000000000000009"}"#
            return try Fixtures.decode(#"{"agents":[\#(scout)\#(count > after ? "," + willow : "")]}"#)
        }
        func agentStartByText() async throws -> AgentStartByText { try Fixtures.decode(Fixtures.startByText) }
    }

    private func app(_ client: TextedClient) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        await app.refreshAgentList()
        return app
    }

    @Test func theSheetClosesOnTheNewAgentOnceItIsListed() async throws {
        let client = TextedClient(after: 4)
        let app = try await app(client)
        let flow = NewAgent()
        app.sheet = .newAgent(flow)
        await flow.watch(app: app, waits: Array(repeating: .zero, count: 10))
        #expect(app.sheet == nil)
        #expect(app.selectedAgent == "00000000000000000000000000000009")
        #expect(app.shownAgent?.displayName == "Willow")
        // Found on the fourth read after the one Agents made, then read once
        // more with each host's standing; no more.
        #expect(client.listed == 5 + 1)
    }

    @Test func noNewAgentLeavesTheSheetOpenAndStopsReading() async throws {
        let client = TextedClient(after: .max)
        let app = try await app(client)
        let flow = NewAgent()
        app.sheet = .newAgent(flow)
        await flow.watch(app: app, waits: Array(repeating: .zero, count: 3))
        #expect(app.sheet == .newAgent(flow))
        #expect(app.selectedAgent == nil)
        #expect(client.listed == 1 + 3)
    }

    // Plow's own choice is offered first; the phrase and links are latch's.
    @Test func plowsDefaultKindIsPicked() async throws {
        let app = try await app(TextedClient(after: .max))
        let flow = NewAgent()
        await flow.load(app: app)
        #expect(flow.picked?.id == "exe:openclaw")
        flow.kind = "exe:life"
        #expect(flow.picked?.smsLink == "sms:+16504359440?&body=Set%20this%20up%20for%20me%3A%20aiworthusing.com%2Fagent-index%2Flife")
    }

    @Test func theWaitBacksOffAndEnds() {
        #expect(NewAgent.waits.first == .seconds(3) && NewAgent.waits.last == .seconds(10))
        #expect(NewAgent.waits.reduce(.zero, +) == .seconds(600))
    }
}

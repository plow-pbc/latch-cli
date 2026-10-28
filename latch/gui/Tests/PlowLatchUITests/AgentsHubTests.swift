import AppKit
import Synchronization
import SwiftUI
import Testing
@testable import PlowLatchUI

/// Agents' Remove… and Rules buttons: a removal names the agent by uid, and
/// a request for an agent's rules is taken only by that host's editor. And
/// Agents itself, back where it was left.
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

    // Left scrolled down, then returned to (a new view of it): Agents
    // opens where it was, once its rows are laid out, and stays there.
    @Test func agentsOpensWhereItWasLeft() async throws {
        let app = try await app(TwinsClient())
        app.agentsScrolled = 120
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 700, height: 300), styleMask: [.titled],
                               backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: AgentsView().environment(app))
        // Setting its content can move the window onto a display: back.
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        #expect(!NSScreen.screens.contains { $0.frame.intersects(window.frame) })
        func scrollers(_ view: NSView) -> [NSScrollView] { ((view as? NSScrollView).map { [$0] } ?? []) + view.subviews.flatMap(scrollers) }
        func shown() -> CGFloat? { scrollers(window.contentView!).first?.documentVisibleRect.minY }
        await waitUntil { shown() == 120 }
        try await Task.sleep(for: .milliseconds(300))
        #expect(shown() == 120)
        #expect(app.agentsScrolled == 120)
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

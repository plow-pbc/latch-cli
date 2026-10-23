import AppKit
import Foundation
import SwiftUI
import Synchronization
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
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { show }
    func rulesSet(_ text: String) async throws {}
    /// As `rules set|clear --revision R` does: refused with
    /// `stale_revision` unless the rules are still at R (nil: none).
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws {
        let current = agent.map { show.agent($0)?.revision } ?? show.everyAgent?.revision
        guard current == revision else {
            throw LatchError.failed(LatchFailure(code: "stale_revision", message: "the rules changed; they are at \(current ?? "none")"))
        }
        sets.append((text, host, agent))
        if let agent {
            show.agents = (show.agents ?? []).filter { $0.agentUid != agent }
                + (text.isEmpty ? [] : [.init(agentName: nil, agentUid: agent, text: text, revision: "r\(sets.count)")])
        } else {
            show.everyAgent = text.isEmpty ? nil : .init(text: text, revision: "r\(sets.count)")
        }
    }
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
        try await client.rulesSet("Changed on the laptop.", host: nil, agent: nil, revision: started)
        await #expect(throws: LatchError.unreachable(AppModel.changedElsewhere)) {
            try await app.saveRules("My edit.", on: host, agent: nil, revision: started)
        }
        #expect(client.sets.count == 1)
        #expect(client.show.everyAgent?.text == "Changed on the laptop.")
    }

    // An agent with no rules of its own when editing began: if some were
    // added meanwhile, the first save doesn't replace them.
    @Test func aFirstSaveNeverReplacesRulesAddedMeanwhile() async throws {
        let client = RulesClient()
        let (app, host) = try app(client)
        let helper = try #require((try Fixtures.decode(Fixtures.agents) as AgentsList).agents.first { $0.name == "helper" }).uid
        try await client.rulesSet("Added on the laptop.", host: nil, agent: helper, revision: nil)
        await #expect(throws: LatchError.unreachable(AppModel.changedElsewhere)) {
            try await app.saveRules("My first rules.", on: host, agent: helper, revision: nil)
        }
        #expect(client.show.agent(helper)?.text == "Added on the laptop.")
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
        #expect(titles["not-covered"] == "Needs your OK: would be refused")
        #expect(titles["said-no"] == "You said no: would be refused")
        #expect(titles["defaults"] == "Needs your OK: would be refused")
        #expect(titles["host-refused"] == "The host refuses it: would be refused")
        let refused: RulesTry = try Fixtures.decode(Fixtures.tries["host-refused"]!)
        #expect(refused.refused?.code == "exec_denied")
    }
}

@MainActor
struct RulesScrollTests {
    // A pill sets the agent before its host's rules have loaded; the scroll
    // waits for the editor rather than firing at nothing.
    @Test func aPillScrollsOnceTheRulesHaveLoaded() async throws {
        let client = RulesClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        let host = try #require(app.host)
        let scout = try #require(client.show.agents?.first?.agentUid)
        app.rulesFocus = scout
        #expect(app.rulesScrollTarget(on: host) == nil)
        await app.refreshRules(on: host)
        #expect(app.rulesScrollTarget(on: host) == nil)
        await app.refreshAgents()
        #expect(app.rulesScrollTarget(on: host) == scout)
    }
}

/// Rules being edited stay while the person goes elsewhere, and Save (⌘S)
/// saves the focused editor's only.
@MainActor
struct RulesDraftTests {
    private func app(_ client: RulesClient) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.section = .rules
        await app.enter(.rules, host: app.host)
        return app
    }

    @Test func aDraftSurvivesGoingToAnotherScreenAndHostAndBack() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let key = AppModel.RulesKey(host: "studio", agent: nil)
        let saved = try #require(app.savedRules(key))
        app.rulesDrafts[key] = .init(text: saved.text + " Never touch ~/Taxes.", base: saved.revision)
        for (section, host) in [(SidebarItem.home, "studio"), (.activity, "pi-kitchen"), (.rules, "pi-kitchen"), (.rules, "studio")] {
            app.section = section
            app.chosenHost = host
            await app.enter(section, host: app.host)
        }
        #expect(app.rulesDrafts[key]?.text.hasSuffix(" Never touch ~/Taxes.") == true)
        #expect(app.rulesChanged(key))
        #expect(client.sets.isEmpty)
    }

    // Typing changes the draft being typed in, not the map of drafts nor
    // another draft: only that editor redraws, and not the menu.
    @Test func aKeystrokeTouchesOnlyItsOwnDraft() async throws {
        let app = try await app(RulesClient())
        let every = AppModel.RulesKey(host: "studio", agent: nil)
        let scout = AppModel.RulesKey(host: "studio", agent: "ba3c0b3ad1eba7a9a1eb04b67c9db0dd")
        app.typeRules("Agents may read ~/Projects.", at: every, from: app.savedRules(every))
        app.typeRules("scout may run make test.", at: scout, from: app.savedRules(scout))
        let other = try #require(app.rulesDrafts[scout])
        let touched = Mutex<[String]>([])
        withObservationTracking { _ = app.rulesDrafts; _ = app.summaries } onChange: { touched.withLock { $0.append("model") } }
        withObservationTracking { _ = other.text } onChange: { touched.withLock { $0.append("scout's draft") } }
        app.typeRules("Agents may read ~/Projects and ~/Docs.", at: every, from: app.savedRules(every))
        #expect(touched.withLock { $0 }.isEmpty)
        #expect(app.rulesDrafts[every]?.text == "Agents may read ~/Projects and ~/Docs.")
    }

    // The menu's Save runs what the focused editor publishes: in a real
    // window, focusing the second of two edited editors makes Save its.
    @Test func saveIsTheFocusedEditorsOnly() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let host = try #require(app.host)
        let every = AppModel.RulesKey(host: "studio", agent: nil)
        let scout = AppModel.RulesKey(host: "studio", agent: "ba3c0b3ad1eba7a9a1eb04b67c9db0dd")
        let scoutAgent = try #require(app.agents?.first { $0.uid == scout.agent })
        app.typeRules("Agents may read ~/Projects.", at: every, from: app.savedRules(every))
        app.typeRules("scout may run make test.", at: scout, from: app.savedRules(scout))
        let seen = SavePublished()
        let window = NSWindow(contentRect: CGRect(x: -4000, y: -4000, width: 600, height: 500), styleMask: [.titled],
                              backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: Form {
            RulesEditor(title: "every", note: "", saved: app.savedRules(every), host: host, agent: nil)
            RulesEditor(title: "scout", note: "", saved: app.savedRules(scout), host: host, agent: scoutAgent)
            SaveProbe(seen: seen)
        }.environment(app))
        window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil) }
        await waitUntil { textViews(in: window.contentView!).count == 2 }
        #expect(seen.key == nil)
        window.makeFirstResponder(textViews(in: window.contentView!)[1])
        await waitUntil { seen.key == scout }
        seen.save?.run()
        await waitUntil { !client.sets.isEmpty }
        #expect(client.sets.map(\.agent) == [scout.agent])
        #expect(app.rulesDrafts[every] != nil)
    }

    @Test func aDraftOverRulesChangedElsewhereStaysWithWhy() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let key = AppModel.RulesKey(host: "studio", agent: nil)
        app.rulesDrafts[key] = .init(text: "Agents may read ~/Projects.", base: "an older revision")
        await app.saveRulesDraft(key)
        #expect(client.sets.isEmpty)
        #expect(app.rulesDrafts[key] != nil)
        #expect(app.rulesErrors[key]?.message == AppModel.changedElsewhere)
        await app.revertRules(key)
        #expect(app.rulesDrafts[key] == nil && app.rulesErrors[key] == nil)
    }
}

/// What the menu's Save would run, as a window's focus publishes it.
@MainActor
final class SavePublished {
    var key: AppModel.RulesKey?
    var save: RulesSave?
}

private struct SaveProbe: View {
    let seen: SavePublished
    @FocusedValue(\.rulesSave) private var save

    var body: some View {
        Color.clear.frame(height: 1).onChange(of: save?.key, initial: true) {
            seen.key = save?.key
            seen.save = save
        }
    }
}

@MainActor
private func textViews(in view: NSView) -> [NSTextView] {
    (view as? NSTextView).map { [$0] } ?? view.subviews.flatMap(textViews)
}

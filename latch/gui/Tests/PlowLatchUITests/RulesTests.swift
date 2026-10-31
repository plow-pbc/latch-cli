import AppKit
import Foundation
import SwiftUI
import Synchronization
import Testing
@testable import PlowLatchUI

/// The account's rules as latch keeps them for a test: `rules list` gives
/// them; `add`, `edit` and `remove` change them and answer with them, each
/// recorded in latch's words. A write can be refused, held, or reach only
/// some hosts.
final class RulesClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    private struct State {
        var rules: [Rule]
        var unreached: Set<String> = []
        var asked: [String] = []
        var refusal: LatchError?
        var tried: [(request: TryRequest, host: String?, agent: String?, newAgent: Bool)] = []
    }
    struct Rule {
        var id: String
        var text: String
        var agents: RulesList.Scope
        var hosts: RulesList.Scope
    }
    private let state: Mutex<State>
    /// Holds each write until opened.
    var writeGate: Gate?
    /// Holds each write's answer, once its change is made, until opened:
    /// latch reaching the other hosts.
    var deliveryGate: Gate?
    /// Holds each `rules list` until opened.
    var listGate: Gate?
    /// What each `rules try` answers, by the host asked (nil: this Mac).
    var tryAnswer: @Sendable (String?) async throws -> RulesTry = { _ in try Fixtures.decode(Fixtures.tries["allow-owner"]!) }

    init() {
        let list: RulesList = try! Fixtures.decode(Fixtures.rulesList())
        state = Mutex(State(rules: list.rules.map { Rule(id: $0.id, text: $0.text, agents: $0.agents, hosts: $0.hosts) }))
    }

    var asked: [String] { state.withLock { $0.asked } }
    /// The changes asked, without the reads between them.
    var writes: [String] { asked.filter { $0 != "rules list" } }
    var tried: [(request: TryRequest, host: String?, agent: String?, newAgent: Bool)] { state.withLock { $0.tried } }
    func refuse(_ error: LatchError?) { state.withLock { $0.refusal = error } }
    /// The hosts a write reaches no more: latch couldn't check them.
    func unreach(_ hosts: Set<String>) { state.withLock { $0.unreached = hosts } }

    private func write(_ words: String, kind: String, _ change: (inout [Rule]) -> String?) async throws -> RulesWrite {
        await writeGate?.wait()
        let json = try state.withLock { state -> String in
            state.asked.append(words)
            if let refusal = state.refusal { throw refusal }
            let id = change(&state.rules)
            return Self.json(state, change: #"{"kind":"\#(kind)","id":\#(id.map { "\"\($0)\"" } ?? "null"),"saved":true,"changed":true,"reason":null}"#)
        }
        await deliveryGate?.wait()
        return try Fixtures.decode(json)
    }

    private static func json(_ state: State, change: String? = nil) -> String {
        func scope(_ scope: RulesList.Scope, _ key: String) -> String {
            switch scope {
            case .all: #""all""#
            case let .only(ids): "[" + ids.map { #"{"\#(key)":"\#($0)"}"# }.joined(separator: ",") + "]"
            }
        }
        let rules = state.rules.map { rule in
            let text = String(decoding: try! JSONEncoder().encode(rule.text), as: UTF8.self)
            return #"{"id":"\#(rule.id)","text":\#(text),"agents":\#(scope(rule.agents, "uid")),"hosts":\#(scope(rule.hosts, "key")),"behind_hosts":[]}"#
        }
        let fixture = try! Fixtures.decode(Fixtures.rulesList()) as RulesList
        let hosts = fixture.hosts.map { host in
            let unknown = state.unreached.contains(host.name)
            return #"{"key":"\#(host.key)","name":"\#(host.name)","self":\#(host.thisMac),"delivery":"\#(unknown ? "unknown" : "current")","reason":\#(unknown ? #""host_offline""# : "null"),"budget":{"over_budget":false}}"#
        }
        return #"{"rules":[\#(rules.joined(separator: ","))],"hosts":[\#(hosts.joined(separator: ","))]"# + (change.map { #","change":\#($0)"# } ?? "") + "}"
    }

    /// The rules as they were when asked, answered once the gate opens.
    func rulesList() async throws -> RulesList {
        let json = state.withLock { state in
            state.asked.append("rules list")
            return Self.json(state)
        }
        await listGate?.wait()
        return try Fixtures.decode(json)
    }

    func rulesAdd(_ text: String, agents: RulesList.Scope, hosts: RulesList.Scope) async throws -> RulesWrite {
        try await write("rules add --file - --agents \(agents.argument) --hosts \(hosts.argument) <<< \(text)", kind: "add") { rules in
            rules.append(Rule(id: "rl_new", text: text, agents: agents, hosts: hosts))
            return "rl_new"
        }
    }

    func rulesEdit(_ id: String, text: String?, agents: RulesList.Scope?, hosts: RulesList.Scope?) async throws -> RulesWrite {
        let words = ["rules edit \(id)", text.map { _ in "--file -" }, agents.map { "--agents \($0.argument)" },
                     hosts.map { "--hosts \($0.argument)" }].compactMap { $0 }.joined(separator: " ") + (text.map { " <<< \($0)" } ?? "")
        return try await write(words, kind: "edit") { rules in
            guard let index = rules.firstIndex(where: { $0.id == id }) else { return id }
            if let text { rules[index].text = text }
            if let agents { rules[index].agents = agents }
            if let hosts { rules[index].hosts = hosts }
            return id
        }
    }

    func rulesRemove(_ id: String) async throws -> RulesWrite {
        try await write("rules remove \(id)", kind: "remove") { rules in
            rules.removeAll { $0.id == id }
            return id
        }
    }

    func rulesTry(_ request: TryRequest, host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        state.withLock { $0.tried.append((request, host, agent, newAgent)) }
        return try await tryAnswer(host)
    }

    func agents(host: String?) async throws -> AgentsList { try Fixtures.decode(host == nil ? Fixtures.agents : Fixtures.agentsStudio) }
}

@MainActor
struct RulesTests {
    private let scout = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd", helper = "3971582e59baad83250141c8dcc95855"
    private let studio = "6b55a78fcd1400f6ac14f129cd1825744125406e25a2892be4abda157baaa23f"
    private let kitchen = "78d8cd12e343e28d70b1da3ac8a0bb8c37593b5f8d535a58b716b776a91f2edc"

    private func app(_ client: RulesClient = RulesClient()) async throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .account(.rules)
        await app.enter(app.route)
        return app
    }

    // MARK: What the page shows

    // `rules list` in the agreed shape: a scope is "all" or uids or keys;
    // this Mac, each host's delivery and reason, and whether its rules are
    // too long.
    @Test func aRulesListDecodes() throws {
        let list: RulesList = try Fixtures.decode(Fixtures.rulesList(behind: true, unchecked: "host_outdated", over: true))
        #expect(list.rules.map(\.id) == ["rl_7f3a9c2e", "rl_1b84d0f6", "rl_c0e5a713", "rl_92d6be48", "rl_e41f07ad"])
        #expect(list.rules[0].agents == .all && list.rules[0].hosts == .all)
        #expect(list.rules[2].agents == .only([scout]) && list.rules[2].hosts == .only([studio]))
        #expect(list.rules[0].behindHosts == [kitchen] && list.rules[2].behindHosts.isEmpty)
        // A host that answered without a rule is named on that rule only; one
        // latch couldn't check, on none.
        #expect(list.rules.filter { !$0.behindHosts.isEmpty }.map(\.id) == ["rl_7f3a9c2e", "rl_92d6be48"])
        #expect(list.rules.allSatisfy { !$0.behindHosts.contains(list.hosts[2].key) })
        #expect(list.hosts.filter { $0.delivery == "unknown" }.map(\.name) == ["sidecar"])
        #expect(list.thisMac?.name == "studio")
        #expect(list.hosts.map(\.delivery) == ["current", "behind", "unknown"])
        #expect(list.hosts.map(\.reason) == [nil, "rules_io", "host_outdated"])
        #expect(list.hosts.map { $0.budget?.overBudget } == [false, true, false])
    }

    // What the real latch printed (`Fixtures.rulesListed` and the rest,
    // captured from the debug build): the list, an add's answer with the
    // list after it, and a guarded add skipped because rules apply here.
    @Test func latchsOwnAnswersDecode() throws {
        let key = "a20b57d5f009b01c518532544e3bb40c899ea121d2adec80ae03a153a55821ad"
        let list: RulesList = try Fixtures.decode(Fixtures.rulesListed)
        #expect(list.rules.map(\.hosts) == [.all, .only([key])])
        #expect(list.rules.allSatisfy { $0.agents == .all && $0.behindHosts.isEmpty })
        #expect(list.thisMac?.key == key && list.thisMac?.delivery == "current" && list.thisMac?.budget?.overBudget == false)
        let added: RulesWrite = try Fixtures.decode(Fixtures.ruleAdded)
        #expect(added.change == .init(kind: "add", id: "8840ab210a979e0ce2c104937758adb0", saved: true, reason: nil))
        #expect(added.list == list)
        let skipped: RulesWrite = try Fixtures.decode(Fixtures.ruleAddSkipped)
        #expect(skipped.change == .init(kind: "add", id: nil, saved: false, reason: "rules_exist"))
        #expect(skipped.list.rules.count == 2)
    }

    // latch's refusals of a change, as it printed them, each said in the
    // app's words.
    @Test func latchsRefusalsAreSaidInTheAppsWords() throws {
        struct Line: Decodable { var error: LatchFailure }
        func said(_ name: String) throws -> String? {
            let line: Line = try Fixtures.decode(try #require(Fixtures.ruleRefusals[name]))
            return ErrorCopy.sentence(.failed(line.error), host: "studio", place: .rules)
        }
        #expect(try said("no_such_rule removed") == "This rule was deleted, so your change wasn't saved.")
        #expect(try said("no_such_rule") == "This rule could not be found among the copies checked.")
        #expect(try said("rules_too_long")
            == "Your change wasn't saved because the rules on rules-mac-contract would be too long. Shorten this rule or delete another.")
        #expect(try said("no_such_agent") == "That agent isn't in your account any more.")
        #expect(try Fixtures.ruleRefusals.keys.allSatisfy { try said($0) != nil || $0 == "empty_rules" })
    }

    @Test func eachRuleSaysWhomItCovers() async throws {
        let app = try await app()
        let rules = try #require(app.rules?.rules)
        #expect(rules.map(app.scopeLine) == [
            "All agents on all hosts",
            "All agents on studio (this Mac)",
            "scout on studio (this Mac)",
            "helper on pi-kitchen",
            "scout and helper on all hosts",
        ])
    }

    // MARK: Filters

    // An agent and a host, each kept by uid or key: a rule for all of them
    // is kept for any one; both together narrow to what covers both.
    @Test func filtersKeepTheRulesCoveringTheirAgentAndHost() async throws {
        let app = try await app()
        let list = try #require(app.rules)
        #expect(app.ruleRows(list).count == 5)
        app.rulesFilter.agent = helper
        #expect(app.ruleRows(list).map(\.id) == ["rl_7f3a9c2e", "rl_1b84d0f6", "rl_92d6be48", "rl_e41f07ad"])
        app.rulesFilter.host = studio
        #expect(app.ruleRows(list).map(\.id) == ["rl_7f3a9c2e", "rl_1b84d0f6", "rl_e41f07ad"])
        app.rulesFilter = .init(agent: nil, host: kitchen)
        #expect(app.ruleRows(list).map(\.id) == ["rl_7f3a9c2e", "rl_92d6be48", "rl_e41f07ad"])
    }

    // MARK: New Rule and Edit

    @Test func addIsOffUntilTheRuleHasTextAndSomeoneToCover() {
        let editor = RuleEditor()
        #expect(editor.agents == .all && editor.hosts == .all)
        #expect(editor.problem == "Write the rule first.")
        editor.text = "  \n"
        #expect(editor.problem == "Write the rule first.")
        editor.text = "Ask me first."
        #expect(editor.problem == nil)
        editor.agents = .only([])
        #expect(editor.problem == "Choose at least one agent.")
        editor.agents = .all
        editor.hosts = .only([])
        #expect(editor.problem == "Choose at least one host.")
    }

    // Ticking a name leaves All; unticking the last leaves none, never All;
    // All again clears the names.
    @Test func unTickingTheLastNameLeavesNone() {
        var scope = RulesList.Scope.all
        #expect(!scope.picks(scout))
        scope = scope.picking(scout, true)
        #expect(scope == .only([scout]))
        scope = scope.picking(helper, true)
        #expect(scope == .only([scout, helper]))
        scope = scope.picking(scout, false).picking(helper, false)
        #expect(scope == .only([]))
    }

    // Add sends the text and both scopes; Edit sends only what changed, so
    // a scope left alone keeps whatever latch stores for it. Each saved
    // rule is held and selected at once from latch's answer.
    @Test func addAndEditSendWhatWasChosen() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let new = RuleEditor()
        new.text = "Ask me before deleting anything."
        new.agents = .only([helper])
        app.sheet = .rule(new)
        #expect(await new.save(app: app))
        #expect(client.writes.last == "rules add --file - --agents \(helper) --hosts all <<< Ask me before deleting anything.")
        #expect(app.rules?.rules.last?.text == "Ask me before deleting anything.")
        #expect(app.selectedRule == "rl_new" && app.sheet == nil)

        let rule = try #require(app.rules?.rules.first { $0.id == "rl_c0e5a713" })
        let edit = RuleEditor(editing: rule)
        edit.text += " Never on Sundays."
        #expect(await edit.save(app: app))
        #expect(client.writes.last == "rules edit rl_c0e5a713 --file - <<< \(rule.text) Never on Sundays.")
        let scoped = RuleEditor(editing: try #require(app.rules?.rules.first { $0.id == "rl_c0e5a713" }))
        scoped.hosts = .all
        #expect(await scoped.save(app: app))
        #expect(client.writes.last == "rules edit rl_c0e5a713 --hosts all")
        // Nothing changed: nothing to ask.
        let count = client.writes.count
        #expect(await RuleEditor(editing: try #require(app.rules?.rules.first)).save(app: app))
        #expect(client.writes.count == count)
    }

    // A refusal keeps the sheet and what's in it, with latch's reason in
    // the app's words, naming the agent and host.
    @Test func aRefusalKeepsTheDraft() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let editor = RuleEditor(editing: try #require(app.rules?.rules.first { $0.id == "rl_c0e5a713" }))
        editor.text = "scout may run anything at all, all day."
        editor.agents = .only([scout, helper])
        app.sheet = .rule(editor)
        client.refuse(.failed(LatchFailure(code: "rules_too_long", message: "Your change wasn't saved because the rules for scout on studio would exceed 8192 bytes.",
                                           hosts: [.init(name: "studio", agent: scout)])))
        #expect(await !editor.save(app: app))
        #expect(app.sheet == .rule(editor))
        #expect(editor.text == "scout may run anything at all, all day." && editor.agents == .only([scout, helper]))
        #expect(editor.refusal(app: app)
            == "Your change wasn't saved because the rules for scout on studio would be too long. Shorten this rule or delete another.")
        #expect(app.rules?.rules.first { $0.id == "rl_c0e5a713" }?.text != editor.text)
    }

    // A change latch refuses (here, a rule deleted on another device) reads
    // the rules again: the deleted rule leaves the list, and the sheet keeps
    // the draft and says why.
    @Test func aRefusedChangeReadsTheRulesAgain() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let editor = RuleEditor(editing: try #require(app.rules?.rules.first { $0.id == "rl_92d6be48" }))
        editor.text = "helper may read the list."
        app.sheet = .rule(editor)
        // Deleted elsewhere: latch's copies no longer hold it.
        _ = try await client.rulesRemove("rl_92d6be48")
        client.refuse(.failed(LatchFailure(code: "no_such_rule", message: "This rule was deleted, so your change wasn't saved.", removed: true)))
        let reads = client.asked.filter { $0 == "rules list" }.count
        #expect(await !editor.save(app: app))
        #expect(client.asked.filter { $0 == "rules list" }.count == reads + 1)
        #expect(app.rules?.rules.contains { $0.id == "rl_92d6be48" } == false)
        #expect(app.sheet == .rule(editor) && editor.text == "helper may read the list.")
        #expect(ErrorCopy.sentence(try #require(editor.error), place: .rules) == "This rule was deleted, so your change wasn't saved.")

        // From the list too: the delete's refusal is kept to say, over the
        // rules read again.
        let rule = try #require(app.rules?.rules.first)
        await app.deleteRule(rule)
        #expect(client.asked.filter { $0 == "rules list" }.count == reads + 2)
        #expect(app.ruleDeleteError?.code == "no_such_rule")
    }

    // Saved on this Mac but not every host reached: it's saved. The sheet
    // closes, the rule is listed, and the page says which host couldn't
    // be checked; nothing invites a second Add.
    @Test func aSaveThatReachedSomeHostsIsSaved() async throws {
        let client = RulesClient()
        let app = try await app(client)
        client.unreach(["sidecar"])
        let editor = RuleEditor()
        editor.text = "Ask me first."
        app.sheet = .rule(editor)
        #expect(await editor.save(app: app))
        #expect(app.sheet == nil)
        #expect(app.rules?.rules.contains { $0.text == "Ask me first." } == true)
        #expect(app.rules?.hosts.first { $0.name == "sidecar" }?.delivery == "unknown")
        #expect(client.asked.filter { $0.hasPrefix("rules add") }.count == 1)
    }

    // A delete that didn't reach a host the rule applied to is said until
    // a list says that host holds the newest rules; a host outside the
    // rule never applied it, so isn't named.
    @Test func aDeleteThatMissedAHostSaysSoUntilItCatchesUp() async throws {
        let client = RulesClient()
        let app = try await app(client)
        client.unreach(["pi-kitchen", "sidecar"])
        await app.deleteRule(try #require(app.rules?.rules.first { $0.id == "rl_92d6be48" }))
        #expect(app.deletesMissed == [kitchen])
        client.unreach(["sidecar"])
        await app.refreshRules()
        #expect(app.deletesMissed.isEmpty)
    }

    // A read begun before a save answers after it, with the rules as they
    // were: the save's rules stay.
    @Test func anOlderReadNeverUndoesASave() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let gate = Gate()
        client.listGate = gate
        let reading = Task { await app.refreshRules() }
        await waitUntil { client.asked.filter { $0 == "rules list" }.count == 2 }
        client.listGate = nil
        let editor = RuleEditor()
        editor.text = "Ask me first."
        app.sheet = .rule(editor)
        #expect(await editor.save(app: app))
        gate.open()
        await reading.value
        #expect(app.rules?.rules.contains { $0.text == "Ask me first." } == true)

        let removing = RuleEditor(editing: try #require(app.rules?.rules.first { $0.id == "rl_92d6be48" }))
        client.listGate = Gate()
        let stale = Task { await app.refreshRules() }
        await waitUntil { client.asked.filter { $0 == "rules list" }.count == 3 }
        let held = client.listGate
        client.listGate = nil
        await removing.delete(app: app)
        held?.open()
        await stale.value
        #expect(app.rules?.rules.contains { $0.id == "rl_92d6be48" } == false)
    }

    // A save left running behind a closed sheet answers with the rules as
    // it made them; a delete made meanwhile isn't undone by that answer.
    @Test func anEarlierSaveNeverUndoesALaterOne() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let gate = Gate()
        client.deliveryGate = gate
        let adding = RuleEditor()
        adding.text = "Ask me first."
        let saving = Task { await adding.save(app: app) }
        await waitUntil { client.writes.count == 1 }
        client.deliveryGate = nil
        let removing = Task { await app.deleteRule(try #require(app.rules?.rules.first { $0.id == "rl_92d6be48" })) }
        // Time for the delete to finish first, were it let; the add is held
        // till then either way.
        for _ in 0..<200 where client.writes.count < 2 || app.rules?.rules.contains(where: { $0.id == "rl_92d6be48" }) == true {
            try? await Task.sleep(for: .milliseconds(1))
        }
        gate.open()
        #expect(await saving.value)
        try await removing.value
        #expect(app.rules?.rules.contains { $0.text == "Ask me first." } == true)
        #expect(app.rules?.rules.contains { $0.id == "rl_92d6be48" } == false)
    }

    // A save that finishes after its sheet was closed, and another opened,
    // closes nothing.
    @Test func aLateSaveDoesntCloseAnotherSheet() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let gate = Gate()
        client.writeGate = gate
        let first = RuleEditor()
        first.text = "Ask me first."
        app.sheet = .rule(first)
        let saving = Task { await first.save(app: app) }
        await waitUntil { first.saving }
        let second = RuleEditor()
        app.sheet = .rule(second)
        gate.open()
        _ = await saving.value
        #expect(app.sheet == .rule(second))
    }

    // MARK: Delete

    // Delete… asks over the sheet; Cancel keeps it with what's typed and
    // chosen, asking nothing of latch. Delete removes the rule and closes it.
    @Test func deleteFromEditAsksOverTheSheet() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let editor = RuleEditor(editing: try #require(app.rules?.rules.first { $0.id == "rl_92d6be48" }))
        editor.text = "helper may read the list."
        editor.hosts = .all
        app.sheet = .rule(editor)
        editor.confirmingDelete = true
        editor.confirmingDelete = false
        #expect(app.sheet == .rule(editor) && editor.text == "helper may read the list." && editor.hosts == .all)
        #expect(!client.asked.contains { $0.hasPrefix("rules remove") })
        await editor.delete(app: app)
        #expect(client.writes.last == "rules remove rl_92d6be48")
        #expect(app.sheet == nil && app.rules?.rules.contains { $0.id == "rl_92d6be48" } == false)
    }

    // MARK: Try a Request

    // Try asks the host picked, as the caller picked, the request typed;
    // "A new agent" is sent as one.
    @Test func tryAsksTheHostCallerAndRequestPicked() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let trial = app.trial
        trial.host = "pi-kitchen"
        trial.caller = .agent(helper)
        trial.request = "exec -- ls ~/Kitchen"
        await trial.run(app: app)
        trial.host = "studio"
        trial.caller = .newAgent
        await trial.run(app: app)
        #expect(client.tried.map(\.host) == ["pi-kitchen", nil])
        #expect(client.tried.map(\.agent) == [helper, nil])
        #expect(client.tried.map(\.newAgent) == [false, true])
        #expect(client.tried.allSatisfy { $0.request == .words(["exec", "--", "ls", "~/Kitchen"]) })
    }

    // A verdict shows only while the host, caller and request are its own;
    // an answer for what's no longer picked is dropped.
    @Test func aVerdictBelongsToWhatProducedIt() async throws {
        let client = RulesClient()
        let app = try await app(client)
        let trial = app.trial
        trial.host = "studio"
        trial.request = "exec -- ls"
        await trial.run(app: app)
        #expect(trial.result != nil)
        trial.host = "pi-kitchen"
        #expect(trial.result == nil)
        trial.host = "studio"
        #expect(trial.result != nil)
        trial.request = "exec -- ls ~"
        #expect(trial.result == nil)

        let gate = Gate()
        client.tryAnswer = { _ in await gate.wait(); return try Fixtures.decode(Fixtures.tries["said-no"]!) }
        let late = Task { await trial.run(app: app) }
        await waitUntil { client.tried.count == 2 }
        trial.caller = .agent(scout)
        gate.open()
        await late.value
        trial.caller = .newAgent
        #expect(trial.result == nil)
    }

    // A host that can't answer shows its own error; no other host is
    // asked in its place.
    @Test func anUnreachableHostSaysSo() async throws {
        let client = RulesClient()
        client.tryAnswer = { host in
            guard host == nil else { throw LatchError.failed(LatchFailure(code: "host_offline", message: #"host "pi-kitchen" is offline"#)) }
            return try Fixtures.decode(Fixtures.tries["allow-owner"]!)
        }
        let app = try await app(client)
        let trial = app.trial
        trial.host = "pi-kitchen"
        trial.request = "exec -- ls"
        await trial.run(app: app)
        #expect(client.tried.map(\.host) == ["pi-kitchen"])
        guard case let .failure(error)? = trial.result else { Issue.record("no error: \(String(describing: trial.result))"); return }
        #expect(ErrorCopy.sentence(error, host: "pi-kitchen", place: .tryRequest()) == "pi-kitchen is offline. Start Latch there, then try again.")
    }

    @Test func requestsSplitAsAShellWould() {
        #expect(ShellWords.split(#"exec --write ~/site -- npm run build"#) == ["exec", "--write", "~/site", "--", "npm", "run", "build"])
        #expect(ShellWords.split(#"exec -- cat "My Notes/a b.txt" 'it''s'"#) == ["exec", "--", "cat", "My Notes/a b.txt", "its"])
        #expect(ShellWords.split(#"exec -- echo a\ b"#) == ["exec", "--", "echo", "a b"])
        #expect(ShellWords.split(#"exec -- echo "open"#) == nil)
    }

    /// Each of the Examples menu's items fills the request field with
    /// words that split back to the example's request.
    @Test func everyExampleFillsTheRequestWithItsWords() {
        // The Instacart sign-in is a saved login's fill, with no words.
        #expect(Trial.examples.map(\.title) == Onboarding.examples.map(\.title).filter { !$0.contains("Instacart") })
        #expect(Trial.examples.count == 9)
        let trial = Trial()
        for example in Trial.examples {
            trial.use(example)
            #expect(trial.words == example.request.words, "\(example.title): \(trial.request)")
        }
        let awkward = ["", "it's", #"say "hi""#, #"back\slash"#, "two\nlines", "plain"]
        #expect(ShellWords.split(ShellWords.join(awkward)) == awkward)
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

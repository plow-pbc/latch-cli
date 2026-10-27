import AppKit
import Synchronization
import Testing
@testable import PlowLatchUI

/// The synthetic latch, recording each change the app asks of it, in
/// latch's words and on which host; or refusing every change.
final class ChangesClient: LatchClient, @unchecked Sendable {
    let synthetic = SyntheticLatchClient(SyntheticLatchClient.Spec("hosts=3,vault=5,agents=2,connectors=20")!, stateDir: "/s")
    private let state = Mutex((asked: [String](), refusal: LatchError?.none))
    /// Every change asked, refused or not.
    var asked: [String] { state.withLock { $0.asked } }
    func refuse(_ error: LatchError?) { state.withLock { $0.refusal = error } }
    /// Holds each `vault add` until opened.
    var addGate: Gate?

    private func change(_ words: String, on host: String? = nil) throws {
        try state.withLock { state in
            state.asked.append(words + (host.map { " --host \($0)" } ?? ""))
            if let refusal = state.refusal { throw refusal }
        }
    }

    var stateDir: String { synthetic.stateDir }
    func localStatus() async throws -> LocalStatus { try await synthetic.localStatus() }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func accountStatus() async throws -> AccountStatus { try await synthetic.accountStatus() }
    func hostStatus(_ host: String) async throws -> LocalStatus { try await synthetic.hostStatus(host) }
    func agents(host: String?) async throws -> AgentsList { try await synthetic.agents(host: host) }
    func rulesShow(host: String?) async throws -> RulesShow { try await synthetic.rulesShow(host: host) }
    func vaultList(host: String?) async throws -> VaultList { try await synthetic.vaultList(host: host) }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { try await synthetic.vaultShow(item, host: host) }
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview { try await synthetic.vaultImportPreview(file, host: host) }
    /// This Mac's host isn't running: Start Hosting starts it.
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [], google: Fixtures.googleConnected(["morgan@example.com"]))) }
    func devices() async throws -> DeviceList { try await synthetic.devices() }
    func agentCatalog() async throws -> AgentCatalog { try await synthetic.agentCatalog() }
    func agentLines() async throws -> AgentLines { try await synthetic.agentLines() }
    func agentShow(_ uid: String) async throws -> AgentsList.Agent { try await synthetic.agentShow(uid) }

    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        await addGate?.wait()
        try change("vault add \(draft.title)", on: host)
        var item = try await synthetic.vaultShow("v0", host: host).item
        item.title = draft.title
        return item
    }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        try change("vault edit \(item) --revision \(revision) \(draft.title)", on: host)
        return try await synthetic.vaultShow(item, host: host).item
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws {
        try change("vault remove \(item) --revision \(revision)", on: host)
    }
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult {
        try change("vault import \(file.lastPathComponent) --pick \(picks.map(String.init).joined(separator: ","))", on: host)
        return try await synthetic.vaultImport(file, picks: picks, expect: expect, host: host)
    }
    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated {
        try change("agents new --name \(name) --line \(line)" + (cloud.map { " --cloud \($0)" } ?? "")
                   + (tokenFile.map { " --token-file \($0.path)" } ?? ""))
        let agent = #"{"kind":"own","line":{"name":null,"number":"+15550000002"},"name":"\#(name)","status":null,"uid":"00000000000000000000000000000009"}"#
        return try Fixtures.decode(#"{"agent":\#(agent),"token_file":"\#(tokenFile?.path ?? "")"}"#)
    }
    func removeAgent(_ uid: String) async throws { try change("agents remove \(uid) --yes") }
    func removeDevice(_ name: String) async throws { try change("devices remove \(name) --yes") }
    func stop() async throws { try change("stop") }
    func perform(_ args: [String]) async throws { try change(args.joined(separator: " ")) }
    func signInGoogle(addAccount: Bool) -> AsyncThrowingStream<GoogleSignInEvent, Error> {
        do {
            try change("plugins enable google --events" + (addAccount ? " --add-account" : ""))
        } catch {
            return AsyncThrowingStream { $0.finish(throwing: error) }
        }
        return synthetic.signInGoogle(addAccount: addAccount)
    }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { try await synthetic.enable(plugin) }
}

/// The window's sheets as a test has them: nothing shows; the test
/// answers in the person's place.
@MainActor final class SheetRecorder: SheetPresenting {
    private(set) var sheet: NSViewController?
    private(set) var alert: (alert: NSAlert, answered: (NSApplication.ModalResponse) -> Void)?
    private(set) var panel: (panel: NSSavePanel, over: NSWindow, chosen: (URL?) -> Void)?
    /// A sheet's view has a window, as AppKit's sheet would give it, but
    /// never on a display.
    private var sheetWindow: Offscreen?

    func present(_ sheet: NSViewController, over controller: NSViewController) {
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 100, height: 100), styleMask: [.titled],
                               backing: .buffered, defer: true)
        window.contentViewController = sheet
        sheetWindow = window
        self.sheet = sheet
    }

    func dismiss(_ sheet: NSViewController) {
        guard self.sheet === sheet else { return }
        self.sheet = nil
        sheetWindow = nil
    }

    func present(_ alert: NSAlert, over window: NSWindow, answered: @escaping (NSApplication.ModalResponse) -> Void) {
        self.alert = (alert, answered)
    }

    func choose(_ panel: NSSavePanel, over window: NSWindow, chosen: @escaping (URL?) -> Void) {
        self.panel = (panel, window, chosen)
    }

    func end(_ sheet: NSWindow, over window: NSWindow) {
        if let alert, alert.alert.window === sheet { answer(.abort) }
        if let panel, panel.panel === sheet { choose(nil) }
    }

    /// The person answers the alert: its first button, its action, or
    /// Cancel.
    func answer(_ response: NSApplication.ModalResponse) {
        guard let alert else { return }
        self.alert = nil
        alert.answered(response)
    }

    func choose(_ file: URL?) {
        guard let panel else { return }
        self.panel = nil
        panel.chosen(file)
    }

    /// The sheet's SwiftUI view, by its type's name.
    var sheetName: String? { sheet.map { String(describing: type(of: $0)) } }
}

/// Every sheet and question the main window shows: presented by the window
/// over itself, each completes on the host or item it was opened for,
/// cancels with nothing changed, and says latch's refusal.
@MainActor
struct SheetTests {
    private struct Opened {
        let app: AppModel
        let client: ChangesClient
        let controller: MainWindowController
        let sheets: SheetRecorder
    }

    private func open(_ place: Place, host: String = "studio") async throws -> Opened {
        let client = ChangesClient()
        let store = ProgressStore(memory: .init(), stateDir: "/s")
        store.save(PlowLatchUI.Progress(passed: [.welcome: .continued], finished: true))
        let app = AppModel(client: client, store: store)
        app.open = { _ in }
        app.reveal = { _ in }
        await app.refreshStatus()
        await app.refreshAccount()
        app.showHost(host)
        app.place = place
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700),
                               styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let sheets = SheetRecorder()
        let controller = MainWindowController(app: app, window: window, defaults: nil, presenter: sheets)
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        #expect(!NSScreen.screens.contains { $0.frame.intersects(window.frame) })
        return Opened(app: app, client: client, controller: controller, sheets: sheets)
    }

    private func host(_ app: AppModel, _ name: String) throws -> HostEntry { try #require(app.hosts.first { $0.name == name }) }

    private func vaultItem(_ app: AppModel, _ id: String, on host: String) async throws -> VaultList.Item {
        await waitUntil { AppModel.held(app.summaries[host]?.vault) }
        guard case let .success(vault)? = app.summaries[host]?.vault else { throw LatchError.unreachable("no vault on \(host)") }
        return try #require(vault.items.first { $0.item == id })
    }

    // MARK: Questions

    /// Each question, as the window asks it, and what its yes runs.
    private static let questions: [(place: Place, ask: @MainActor (AppModel) async throws -> PlowLatchUI.Confirmation, latch: String)] = [
        (.host(.home), { _ in .startHosting }, "serve -d --again"),
        (.host(.settings), { _ in .stopHost }, "stop"),
        (.account(.agents), { app in
            await waitUntil { app.agents != nil }
            return .removeAgent(try #require(app.agents?.last))
        }, "agents remove "),
        (.host(.vault), { app in
            await waitUntil { AppModel.held(app.summaries["pi-kitchen"]?.vault) }
            guard case let .success(vault)? = app.summaries["pi-kitchen"]?.vault else { throw LatchError.unreachable("no vault") }
            return .removeVaultItem(try #require(vault.items.first { $0.item == "v2" }), on: try #require(app.host))
        }, "vault remove v2 --revision 1 --host pi-kitchen"),
        (.account(.connections), { app in
            await waitUntil { app.loggedInDevices?.isEmpty == false }
            return .removeDevice(try #require(app.loggedInDevices?.first { $0.name == "laptop" }))
        }, "devices remove laptop --yes"),
    ]

    // Remove…, Stop… and Start Hosting… ask over the window, in an alert
    // that names what it acts on. Its action runs on that, and only that;
    // Cancel (or Escape) asks nothing of latch. Either way the question goes.
    @Test(arguments: 0..<5, [true, false]) func aQuestionIsAnswered(_ index: Int, yes: Bool) async throws {
        let question = Self.questions[index]
        let opened = try await open(question.place, host: question.place == .host(.vault) ? "pi-kitchen" : "studio")
        defer { opened.controller.close() }
        let confirmation = try await question.ask(opened.app)
        opened.app.ask(confirmation)
        await waitUntil { opened.sheets.alert != nil }
        let alert = try #require(opened.sheets.alert?.alert)
        #expect(alert.messageText == confirmation.question)
        #expect(alert.informativeText == confirmation.detail)
        #expect(alert.buttons.map(\.title) == [confirmation.action, "Cancel"])
        #expect(alert.buttons[0].hasDestructiveAction == confirmation.destructive)
        opened.sheets.answer(yes ? .alertFirstButtonReturn : .alertSecondButtonReturn)
        #expect(opened.app.sheet == nil && opened.sheets.alert == nil)
        if yes {
            await waitUntil { opened.client.asked.contains { $0.hasPrefix(question.latch) } }
            #expect(opened.client.asked.count == 1, "\(opened.client.asked)")
        } else {
            try await Task.sleep(for: .milliseconds(200))
            #expect(opened.client.asked.isEmpty)
        }
    }

    // latch refuses: each question's refusal is kept where its screen says it.
    @Test func aRefusedYesIsSaid() async throws {
        let refusal = LatchError.failed(LatchFailure(code: "plow_unreachable", message: "Plow can't be reached"))
        let saidBy: [(Int, @MainActor (AppModel) -> LatchError?)] = [
            (0, { $0.startHostError }), (1, { _ in nil }), (2, { $0.agentRemoveError }), (3, { $0.vaultRemoveError }),
            (4, { $0.deviceRemoveError }),
        ]
        for (index, said) in saidBy {
            let question = Self.questions[index]
            let opened = try await open(question.place, host: question.place == .host(.vault) ? "pi-kitchen" : "studio")
            defer { opened.controller.close() }
            let confirmation = try await question.ask(opened.app)
            opened.client.refuse(refusal)
            opened.app.ask(confirmation)
            await waitUntil { opened.sheets.alert != nil }
            opened.sheets.answer(.alertFirstButtonReturn)
            await waitUntil { !opened.client.asked.isEmpty }
            if case .stopHost = confirmation {
                // Host Settings says a failed stop beside its buttons.
                await waitUntil { opened.app.hostActionError?.code == "plow_unreachable" }
            } else {
                await waitUntil { said(opened.app)?.code == "plow_unreachable" }
            }
        }
    }

    // The question goes with the window, unanswered.
    @Test func closingTheWindowTakesAQuestionAway() async throws {
        let opened = try await open(.host(.settings))
        opened.app.ask(.stopHost)
        await waitUntil { opened.sheets.alert != nil }
        opened.controller.close()
        #expect(opened.sheets.alert == nil)
        #expect(opened.app.sheet == nil)
        try await Task.sleep(for: .milliseconds(100))
        #expect(opened.client.asked.isEmpty)
    }

    // MARK: Sheets

    // New Item (File › New Item…) on pi-kitchen's Vault: added there.
    // Cancel adds nothing; a refusal is said in the sheet, which stays.
    @Test func newItemAddsToTheHostItWasOpenedFor() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.controller.close() }
        let window = try #require(opened.controller.window)
        await waitUntil { MenuBridge.state(MenuBridge.newItem, in: window).enabled }
        func newItem() async throws -> VaultEditor {
            MenuBridge.send(MenuBridge.newItem, in: window)
            await waitUntil { opened.sheets.sheetName?.contains("VaultItemSheet") == true }
            guard case let .vaultItem(editor)? = opened.app.sheet else { throw LatchError.unreachable("no New Item") }
            return editor
        }
        // Cancel.
        _ = try await newItem()
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.isEmpty)
        // Refused: said, kept open.
        var editor = try await newItem()
        editor.title = "Router"
        editor.sites = "router.example"
        opened.client.refuse(.failed(LatchFailure(code: "host_offline", message: "pi-kitchen is offline")))
        #expect(await !editor.save(app: opened.app))
        #expect(editor.error?.code == "host_offline")
        #expect(opened.sheets.sheet != nil)
        opened.app.sheet = nil
        // Added.
        opened.client.refuse(nil)
        await waitUntil { opened.sheets.sheet == nil }
        editor = try await newItem()
        editor.title = "Router"
        editor.sites = "router.example"
        #expect(await editor.save(app: opened.app))
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.last == "vault add Router --host pi-kitchen")
        #expect(opened.app.selectedVaultItem?.host == "pi-kitchen")
    }

    // Cancelled while it saves, then another sheet opened: the save, once
    // done, closes nothing, and the sheet showing stays.
    @Test func aSaveFinishingAfterCancelLeavesTheNextSheet() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.controller.close() }
        let gate = Gate()
        opened.client.addGate = gate
        let editor = VaultEditor(on: try host(opened.app, "pi-kitchen"))
        editor.title = "Router"
        editor.sites = "router.example"
        opened.app.sheet = .vaultItem(editor)
        await waitUntil { opened.sheets.sheet != nil }
        let first = opened.sheets.sheet
        let saving = Task { await editor.save(app: opened.app) }
        await waitUntil { editor.saving }
        opened.app.sheet = nil
        let next = VaultEditor(on: try host(opened.app, "pi-kitchen"))
        opened.app.sheet = .vaultItem(next)
        await waitUntil { opened.sheets.sheet != nil && opened.sheets.sheet !== first }
        let shown = opened.sheets.sheet
        gate.open()
        #expect(await saving.value)
        try await Task.sleep(for: .milliseconds(100))
        #expect(opened.app.sheet == .vaultItem(next))
        #expect(opened.sheets.sheet === shown)
    }

    // An item's Edit… changes that item on its host.
    @Test func editChangesTheItemItWasOpenedFor() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.controller.close() }
        let item = try await vaultItem(opened.app, "v3", on: "pi-kitchen")
        await opened.app.beginVaultEdit(item, on: try host(opened.app, "pi-kitchen"))
        await waitUntil { opened.sheets.sheetName?.contains("VaultItemSheet") == true }
        guard case let .vaultItem(editor)? = opened.app.sheet else { Issue.record("no Edit"); return }
        editor.title = "Renamed"
        #expect(await editor.save(app: opened.app))
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked == ["vault edit v3 --revision 1 Renamed --host pi-kitchen"])
    }

    // Import… (File › Import…): the open panel over the window. Cancelled,
    // nothing is read; an export chosen is previewed and imported into the
    // host it was chosen for; a refused import is said in the sheet.
    @Test func importChoosesAnExportAndImportsIntoItsHost() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.controller.close() }
        let window = try #require(opened.controller.window)
        await waitUntil { MenuBridge.state(MenuBridge.importItems, in: window).enabled }
        MenuBridge.send(MenuBridge.importItems, in: window)
        await waitUntil { opened.sheets.panel != nil }
        #expect(opened.sheets.panel?.over === window)
        #expect(opened.sheets.panel?.panel is NSOpenPanel)
        opened.sheets.choose(nil)
        #expect(opened.app.sheet == nil)
        // Chosen.
        MenuBridge.send(MenuBridge.importItems, in: window)
        await waitUntil { opened.sheets.panel != nil }
        let export = URL(filePath: "/tmp/passwords.csv")
        opened.sheets.choose(export)
        await waitUntil { opened.sheets.sheetName?.contains("ImportSheet") == true }
        guard case let .vaultImport(importer)? = opened.app.sheet else { Issue.record("no import"); return }
        #expect(importer.host.name == "pi-kitchen" && importer.file == export)
        await waitUntil { importer.checkable }
        // Refused.
        opened.client.refuse(.failed(LatchFailure(code: "host_offline", message: "pi-kitchen is offline")))
        await importer.run(app: opened.app)
        guard case let .failure(error)? = importer.result else { Issue.record("no refusal: \(String(describing: importer.result))"); return }
        #expect(error.code == "host_offline")
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        // Imported, from a new choice.
        opened.client.refuse(nil)
        opened.app.beginImport(export, on: try host(opened.app, "pi-kitchen"))
        await waitUntil { opened.sheets.sheetName?.contains("ImportSheet") == true }
        guard case let .vaultImport(again)? = opened.app.sheet else { Issue.record("no import"); return }
        await waitUntil { again.checkable }
        await again.run(app: opened.app)
        guard case .success? = again.result else { Issue.record("not imported: \(String(describing: again.result))"); return }
        #expect(opened.client.asked.last == "vault import passwords.csv --pick 1,2 --host pi-kitchen")
    }

    // Import's Cancel imports nothing.
    @Test func importsCancelImportsNothing() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.controller.close() }
        opened.app.beginImport(URL(filePath: "/tmp/passwords.csv"), on: try host(opened.app, "pi-kitchen"))
        await waitUntil { opened.sheets.sheetName?.contains("ImportSheet") == true }
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.isEmpty)
    }

    // New Agent: Choose… asks where to save the key over the sheet, and the
    // agent is created with it; Cancel creates nothing; a refusal is said.
    @Test func newAgentCreatesAnAgent() async throws {
        let opened = try await open(.account(.agents))
        defer { opened.controller.close() }
        // Cancel.
        opened.app.sheet = .newAgent(NewAgent())
        await waitUntil { opened.sheets.sheetName?.contains("NewAgentSheet") == true }
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.isEmpty)

        let flow = NewAgent()
        opened.app.sheet = .newAgent(flow)
        await waitUntil { opened.sheets.sheetName?.contains("NewAgentSheet") == true }
        await flow.load(app: opened.app)
        flow.kind = .own
        flow.name = "tinker"
        opened.controller.sheets.chooseKeyFile(for: flow)
        let panel = try #require(opened.sheets.panel)
        #expect(panel.over === opened.sheets.sheet?.view.window)
        #expect(panel.over !== opened.controller.window)
        #expect(panel.panel.nameFieldStringValue == "tinker.latch-key")
        opened.sheets.choose(URL(filePath: "/tmp/tinker.latch-key"))
        #expect(flow.tokenFile == URL(filePath: "/tmp/tinker.latch-key"))
        // Refused: said in the sheet.
        opened.client.refuse(.failed(LatchFailure(code: "plow_unreachable", message: "Plow can't be reached")))
        await flow.create(app: opened.app)
        #expect(flow.error?.code == "plow_unreachable" && flow.created == nil)
        // Created.
        opened.client.refuse(nil)
        await flow.create(app: opened.app)
        #expect(flow.created?.agent.name == "tinker")
        #expect(opened.client.asked.last == "agents new --name tinker --line \(flow.line ?? "") --token-file /tmp/tinker.latch-key")
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
    }

    // Connect Google: the sign-in sheet over the window while latch waits,
    // gone once Google is on; Cancel stops the wait; a refusal is said in
    // the sheet.
    @Test func googleSignInShowsWhileItWaits() async throws {
        let opened = try await open(.account(.connections))
        defer { opened.controller.close() }
        opened.app.connectGoogle(adding: true)
        await waitUntil { opened.sheets.sheetName?.contains("GoogleSignInSheet") == true }
        await waitUntil { opened.app.googleSignIn == nil }
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked == ["plugins enable google --events --add-account"])
        // Cancel.
        opened.app.connectGoogle(adding: true)
        await waitUntil { opened.sheets.sheet != nil }
        opened.app.endGoogleSignIn()
        await waitUntil { opened.sheets.sheet == nil }
        // Refused.
        opened.client.refuse(.failed(LatchFailure(code: "plow_unreachable", message: "Plow can't be reached")))
        opened.app.connectGoogle(adding: false)
        await waitUntil { opened.app.googleSignIn?.failure != nil }
        #expect(opened.app.googleSignIn?.failure == GoogleSignIn.problem(.failed(LatchFailure(code: "plow_unreachable", message: "")), adding: false))
        #expect(opened.sheets.sheet != nil)
        opened.app.endGoogleSignIn()
        await waitUntil { opened.sheets.sheet == nil }
    }

    // One at a time: a sheet the model replaces goes before the next shows.
    @Test func aReplacedSheetGoesFirst() async throws {
        let opened = try await open(.host(.vault))
        defer { opened.controller.close() }
        opened.app.sheet = .newAgent(NewAgent())
        await waitUntil { opened.sheets.sheetName?.contains("NewAgentSheet") == true }
        opened.app.ask(.stopHost)
        await waitUntil { opened.sheets.alert != nil }
        #expect(opened.sheets.sheet == nil)
    }
}

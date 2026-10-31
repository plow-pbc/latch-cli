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
    func rulesList() async throws -> RulesList { try await synthetic.rulesList() }
    func vaultList(host: String?) async throws -> VaultList { try await synthetic.vaultList(host: host) }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { try await synthetic.vaultShow(item, host: host) }
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview { try await synthetic.vaultImportPreview(file, host: host) }
    /// This Mac's host isn't running: Start Hosting starts it.
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [], google: Fixtures.googleConnected(["morgan@example.com"]))) }
    func devices() async throws -> DeviceList { try await synthetic.devices() }
    func agentCatalog() async throws -> AgentCatalog { try await synthetic.agentCatalog() }
    func agentStartByText() async throws -> AgentStartByText { try await synthetic.agentStartByText() }

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
    func rulesRemove(_ id: String) async throws -> RulesWrite {
        try change("rules remove \(id)")
        return try Fixtures.decode(#"{"rules":[],"hosts":[],"change":{"kind":"remove","id":"\#(id)","saved":true,"changed":true,"reason":null}}"#)
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
        let sheets: SheetRecorder
        /// What presents them, and the window they're over.
        let presenting: WindowSheets
        let window: NSWindow
        let close: () -> Void
    }

    /// The model on `place`, with what that place reads read once (a still's
    /// model: nothing of its own runs, no status stream nor feed), and its
    /// sheets presented by the window's `WindowSheets` as the model changes.
    /// With `window`, inside the main window itself, for the menu's path
    /// to them and its close; otherwise over a bare view, sparing each test
    /// the window's screens (a real main window costs ~0.13 s of main-actor
    /// time to build and lay out).
    private func open(_ place: Place, host: String = "studio", window main: Bool = false) async throws -> Opened {
        let client = ChangesClient()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.open = { _ in }
        await app.refreshStatus()
        await app.refreshAccount()
        app.showHost(host)
        app.place = place
        await app.enter(app.route)
        let sheets = SheetRecorder()
        if main {
            let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700),
                                   styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            let controller = MainWindowController(app: app, window: window, defaults: nil, presenter: sheets)
            window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
            window.orderFrontRegardless()
            #expect(!NSScreen.screens.contains { $0.frame.intersects(window.frame) })
            return Opened(app: app, client: client, sheets: sheets, presenting: controller.sheets, window: window, close: controller.close)
        }
        let bare = BareWindow(app: app, presenter: sheets)
        return Opened(app: app, client: client, sheets: sheets, presenting: bare.sheets, window: bare.window, close: { bare.close() })
    }

    private func host(_ app: AppModel, _ name: String) throws -> HostEntry { try #require(app.hosts.first { $0.name == name }) }

    private func vaultItem(_ app: AppModel, _ id: String, on host: String) async throws -> VaultList.Item {
        await waitUntil { AppModel.held(app.summaries[host]?.vault) }
        guard case let .success(vault)? = app.summaries[host]?.vault else { throw LatchError.unreachable("no vault on \(host)") }
        return try #require(vault.items.first { $0.item == id })
    }

    // MARK: Questions

    /// Each question, as the window asks it, what its yes runs, and where
    /// its screen says latch's refusal.
    private static let questions: [(place: Place, ask: @MainActor (AppModel) async throws -> PlowLatchUI.Confirmation, latch: String,
                                     said: @MainActor (AppModel) -> LatchError?)] = [
        (.host(.home), { _ in .startHosting }, "serve -d --again", { $0.startHostError }),
        (.host(.settings), { _ in .stopHost }, "stop", { $0.hostActionError }),
        (.account(.agents), { app in
            await waitUntil { app.agents != nil }
            return .removeAgent(try #require(app.agents?.last))
        }, "agents remove ", { $0.agentRemoveError }),
        (.host(.vault), { app in
            await waitUntil { AppModel.held(app.summaries["pi-kitchen"]?.vault) }
            guard case let .success(vault)? = app.summaries["pi-kitchen"]?.vault else { throw LatchError.unreachable("no vault") }
            return .removeVaultItem(try #require(vault.items.first { $0.item == "v2" }), on: try #require(app.host))
        }, "vault remove v2 --revision 1 --host pi-kitchen", { $0.vaultRemoveError }),
        (.account(.connections), { app in
            await waitUntil { app.loggedInDevices?.isEmpty == false }
            return .removeDevice(try #require(app.loggedInDevices?.first { $0.name == "laptop" }))
        }, "devices remove laptop --yes", { $0.deviceRemoveError }),
        (.account(.rules), { app in
            await waitUntil { app.rules != nil }
            return .deleteRule(try #require(app.rules?.rules.last))
        }, "rules remove rl_", { $0.ruleDeleteError }),
    ]

    enum Answer: CaseIterable { case yes, cancel, refused }

    // Remove…, Delete…, Stop… and Start Hosting… ask over the window, in an alert
    // that names what it acts on. Its action runs on that, and only that;
    // Cancel (or Escape) asks nothing of latch; latch's refusal of a yes is
    // kept where the question's screen says it. Either way the question goes.
    @Test(arguments: 0..<6, Answer.allCases) func aQuestionIsAnswered(_ index: Int, answer: Answer) async throws {
        let question = Self.questions[index]
        let opened = try await open(question.place, host: question.place == .host(.vault) ? "pi-kitchen" : "studio")
        defer { opened.close() }
        let confirmation = try await question.ask(opened.app)
        if answer == .refused { opened.client.refuse(.failed(LatchFailure(code: "plow_unreachable", message: "Plow can't be reached"))) }
        opened.app.ask(confirmation)
        await waitUntil { opened.sheets.alert != nil }
        let alert = try #require(opened.sheets.alert?.alert)
        #expect(alert.messageText == confirmation.question)
        #expect(alert.informativeText == confirmation.detail)
        #expect(alert.buttons.map(\.title) == [confirmation.action, "Cancel"])
        #expect(alert.buttons[0].hasDestructiveAction == confirmation.destructive)
        opened.sheets.answer(answer == .cancel ? .alertSecondButtonReturn : .alertFirstButtonReturn)
        #expect(opened.app.sheet == nil && opened.sheets.alert == nil)
        switch answer {
        case .yes:
            await waitUntil { opened.client.asked.contains { $0.hasPrefix(question.latch) } }
            #expect(opened.client.asked.count == 1, "\(opened.client.asked)")
            #expect(question.said(opened.app) == nil)
        case .cancel:
            try await Task.sleep(for: .milliseconds(50))
            #expect(opened.client.asked.isEmpty)
        case .refused:
            await waitUntil { question.said(opened.app)?.code == "plow_unreachable" }
            #expect(opened.client.asked.count == 1, "\(opened.client.asked)")
        }
    }

    // The question goes with the window, unanswered.
    @Test func closingTheWindowTakesAQuestionAway() async throws {
        let opened = try await open(.host(.settings), window: true)
        opened.app.ask(.stopHost)
        await waitUntil { opened.sheets.alert != nil }
        opened.close()
        #expect(opened.sheets.alert == nil)
        #expect(opened.app.sheet == nil)
        try await Task.sleep(for: .milliseconds(100))
        #expect(opened.client.asked.isEmpty)
    }

    // MARK: Sheets

    // New Item (File › New Item…) on pi-kitchen's Vault: added there.
    // Cancel adds nothing; a refusal is said in the sheet, which stays.
    @Test func newItemAddsToTheHostItWasOpenedFor() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen", window: true)
        defer { opened.close() }
        let window = opened.window
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
        defer { opened.close() }
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
        #expect(opened.app.sheet == .vaultItem(next))
        #expect(opened.sheets.sheet === shown)
    }

    // An item's Edit… changes that item on its host.
    @Test func editChangesTheItemItWasOpenedFor() async throws {
        let opened = try await open(.host(.vault), host: "pi-kitchen")
        defer { opened.close() }
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
        let opened = try await open(.host(.vault), host: "pi-kitchen", window: true)
        defer { opened.close() }
        let window = opened.window
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
        defer { opened.close() }
        opened.app.beginImport(URL(filePath: "/tmp/passwords.csv"), on: try host(opened.app, "pi-kitchen"))
        await waitUntil { opened.sheets.sheetName?.contains("ImportSheet") == true }
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.isEmpty)
    }

    // New Agent shows what to text Plow, and changes nothing itself: no
    // agent is created from this Mac.
    @Test func newAgentCreatesNothing() async throws {
        let opened = try await open(.account(.agents))
        defer { opened.close() }
        let flow = NewAgent()
        opened.app.sheet = .newAgent(flow)
        await waitUntil { opened.sheets.sheetName?.contains("NewAgentSheet") == true }
        await flow.load(app: opened.app)
        #expect(flow.picked?.phrase == "Create an agent for me")
        opened.app.sheet = nil
        await waitUntil { opened.sheets.sheet == nil }
        #expect(opened.client.asked.isEmpty)
    }

    // Connect Google: the sign-in sheet over the window while latch waits,
    // gone once Google is on; Cancel stops the wait; a refusal is said in
    // the sheet.
    @Test func googleSignInShowsWhileItWaits() async throws {
        let opened = try await open(.account(.connections))
        defer { opened.close() }
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
        defer { opened.close() }
        opened.app.sheet = .newAgent(NewAgent())
        await waitUntil { opened.sheets.sheetName?.contains("NewAgentSheet") == true }
        opened.app.ask(.stopHost)
        await waitUntil { opened.sheets.alert != nil }
        #expect(opened.sheets.sheet == nil)
    }
}

/// `WindowSheets` over a bare view in a window that never shows, updated
/// on each change of the model as the main window updates it.
@MainActor private final class BareWindow {
    let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 200, height: 200), styleMask: [.titled],
                           backing: .buffered, defer: true)
    let sheets: WindowSheets
    private let host = NSViewController()
    private var closed = false

    init(app: AppModel, presenter: SheetRecorder) {
        sheets = WindowSheets(app: app, presenter: presenter)
        host.view = NSView()
        window.isReleasedWhenClosed = false
        window.contentViewController = host
        follow()
    }

    private func follow() {
        withObservationTracking { sheets.update(over: host) } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, !self.closed else { return }
                self.follow()
            }
        }
    }

    func close() {
        closed = true
        sheets.end(over: window)
    }
}

import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// What the main window shows over itself, one at a time: New Agent, a
/// vault item's New Item or Edit, an export chosen and imported, or a
/// question before an action. The model keeps it; the window presents it.
enum MainSheet: Equatable {
    case newAgent(NewAgent)
    case vaultItem(VaultEditor)
    /// Import…'s open panel, for an export to import into this host's vault.
    case choosingExport(HostEntry)
    case vaultImport(VaultImport)
    case confirm(Confirmation)

    /// The same sheet: the same flow, whatever it holds by now.
    static func == (a: MainSheet, b: MainSheet) -> Bool {
        switch (a, b) {
        case let (.newAgent(a), .newAgent(b)): a === b
        case let (.vaultItem(a), .vaultItem(b)): a === b
        case let (.choosingExport(a), .choosingExport(b)): a.name == b.name
        case let (.vaultImport(a), .vaultImport(b)): a === b
        case let (.confirm(a), .confirm(b)): a == b
        default: false
        }
    }
}

/// An action asked about before it's taken, naming what it acts on.
enum Confirmation: Equatable {
    case startHosting
    case stopHost
    case removeAgent(AgentsList.Agent)
    case removeVaultItem(VaultList.Item, on: HostEntry)
    case removeDevice(DeviceList.Device)

    var question: String {
        switch self {
        case .startHosting: "Start hosting on this Mac?"
        case .stopHost: "Stop Latch on this Mac?"
        case let .removeAgent(agent): "Remove \(agent.name)?"
        case let .removeVaultItem(item, host): "Remove “\(item.title)” from \(host.name)?"
        case let .removeDevice(device): "Remove “\(device.name)” from your account?"
        }
    }

    var detail: String {
        switch self {
        case .startHosting: "Latch runs in the background, and your agents can reach this Mac, judged by your rules."
        case .stopHost: "Your agents can't reach this Mac until you start it again."
        case let .removeAgent(agent):
            "Its credential stops working at once, its phone line is freed and its chats are closed."
                + (agent.kind == "cloud" ? "" : " The program you run for it keeps running until you stop it.")
        case .removeVaultItem: "Agents can no longer fill it. This can't be undone."
        case let .removeDevice(device): "\(device.name) is logged out of Plow and can no longer send requests as you."
        }
    }

    var action: String {
        switch self {
        case .startHosting: "Start Hosting"
        case .stopHost: "Stop"
        case .removeAgent, .removeVaultItem, .removeDevice: "Remove"
        }
    }

    var destructive: Bool { self != .startHosting }
}

/// How the window puts a sheet over itself and takes it away: AppKit's
/// way, or a test's, which shows nothing and answers in the person's place.
@MainActor protocol SheetPresenting {
    func present(_ sheet: NSViewController, over controller: NSViewController)
    func dismiss(_ sheet: NSViewController)
    func present(_ alert: NSAlert, over window: NSWindow, answered: @escaping (NSApplication.ModalResponse) -> Void)
    /// An open or save panel, over the window or over a sheet: the file
    /// chosen, or nil.
    func choose(_ panel: NSSavePanel, over window: NSWindow, chosen: @escaping (URL?) -> Void)
    /// An alert or panel taken away before it was answered.
    func end(_ sheet: NSWindow, over window: NSWindow)
}

struct AppKitSheets: SheetPresenting {
    func present(_ sheet: NSViewController, over controller: NSViewController) { controller.presentAsSheet(sheet) }
    func dismiss(_ sheet: NSViewController) { sheet.presentingViewController?.dismiss(sheet) }
    func present(_ alert: NSAlert, over window: NSWindow, answered: @escaping (NSApplication.ModalResponse) -> Void) {
        alert.beginSheetModal(for: window, completionHandler: answered)
    }
    func choose(_ panel: NSSavePanel, over window: NSWindow, chosen: @escaping (URL?) -> Void) {
        panel.beginSheetModal(for: window) { chosen($0 == .OK ? panel.url : nil) }
    }
    func end(_ sheet: NSWindow, over window: NSWindow) { window.endSheet(sheet, returnCode: .abort) }
}

/// The main window's sheets: the model's `sheet`, or Connections' Google
/// sign-in while it waits. A sheet is SwiftUI in AppKit's sheet; a
/// question is an alert; Import… is the open panel. Each goes when the
/// model's goes, or when it's answered.
@MainActor final class WindowSheets {
    private enum Shown: Equatable {
        case sheet(MainSheet)
        case googleSignIn
    }

    let app: AppModel
    let presenter: any SheetPresenting
    private var shown: (what: Shown, controller: NSViewController?, window: NSWindow?)?

    init(app: AppModel, presenter: any SheetPresenting) {
        self.app = app
        self.presenter = presenter
    }

    /// Over `controller`'s window, what the model asks for now.
    func update(over controller: NSViewController) {
        let wanted: Shown? = app.googleSignIn != nil ? .googleSignIn : app.sheet.map(Shown.sheet)
        guard wanted != shown?.what, let window = controller.view.window else { return }
        end(over: window)
        guard let wanted else { return }
        switch wanted {
        case .googleSignIn: present(GoogleSignInSheet(), wanted, over: controller)
        case let .sheet(.newAgent(flow)):
            present(NewAgentSheet(flow: flow, chooseKeyFile: { [weak self] in self?.chooseKeyFile(for: flow) }), wanted, over: controller)
        case let .sheet(.vaultItem(editor)): present(VaultItemSheet(editor: editor), wanted, over: controller)
        case let .sheet(.vaultImport(importer)): present(ImportSheet(importer: importer), wanted, over: controller)
        case let .sheet(.choosingExport(host)): chooseExport(on: host, wanted, over: window)
        case let .sheet(.confirm(confirmation)): ask(confirmation, wanted, over: window)
        }
    }

    /// Whatever shows goes, unanswered.
    func end(over window: NSWindow) {
        guard let shown else { return }
        self.shown = nil
        if let controller = shown.controller { presenter.dismiss(controller) }
        if let sheet = shown.window { presenter.end(sheet, over: window) }
    }

    private func present(_ view: some View, _ what: Shown, over controller: NSViewController) {
        let sheet = NSHostingController(rootView: view.environment(app))
        sheet.sceneBridgingOptions = []
        shown = (what, sheet, nil)
        presenter.present(sheet, over: controller)
    }

    private func ask(_ confirmation: Confirmation, _ what: Shown, over window: NSWindow) {
        let alert = NSAlert()
        alert.messageText = confirmation.question
        alert.informativeText = confirmation.detail
        alert.addButton(withTitle: confirmation.action).hasDestructiveAction = confirmation.destructive
        alert.addButton(withTitle: "Cancel")
        shown = (what, nil, alert.window)
        presenter.present(alert, over: window) { [weak self] response in
            guard let self, self.answered(what) else { return }
            if response == .alertFirstButtonReturn { Task { await self.app.confirmed(confirmation) } }
        }
    }

    private func chooseExport(on host: HostEntry, _ what: Shown, over window: NSWindow) {
        let panel = NSOpenPanel()
        panel.message = "Choose another password manager's export to import into \(host.name)'s vault."
        panel.prompt = "Choose"
        panel.allowedContentTypes = [.commaSeparatedText, .json, .data]
        shown = (what, nil, panel)
        presenter.choose(panel, over: window) { [weak self] file in
            guard let self, self.answered(what) else { return }
            if let file { self.app.beginImport(file, on: host) }
        }
    }

    /// New Agent's Choose…: where to save the key, over its sheet.
    func chooseKeyFile(for flow: NewAgent) {
        guard let sheet = shown?.controller?.view.window else { return }
        presenter.choose(NewAgentSheet.keyFilePanel(name: flow.name), over: sheet) { file in
            if let file { flow.tokenFile = file }
        }
    }

    /// An alert or panel answered: it went by itself, and so does the
    /// model's sheet. False if something else had taken its place.
    private func answered(_ what: Shown) -> Bool {
        guard shown?.what == what else { return false }
        shown = nil
        if case let .sheet(sheet) = what { app.close(sheet) }
        return true
    }
}

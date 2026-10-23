import SwiftUI
import UniformTypeIdentifiers

/// A host's vault: its items, never their secrets; an item opens in the
/// inspector, and items are added, changed, removed and imported in sheets.
struct VaultView: View {
    @Environment(AppModel.self) private var app
    @State private var search = ""
    @State private var inspecting = true
    @State private var choosingExport = false

    var body: some View {
        @Bindable var app = app
        Group {
            if let host = app.host {
                if !host.isThisMac, host.online == false {
                    ContentUnavailableView {
                        Label("\(host.name) is offline", systemImage: "wifi.slash")
                    } description: {
                        Text("Its vault is kept on \(host.name). Start `latch serve` there to see and change it.")
                    }
                } else {
                    HostVault(host: host, search: search, inspecting: $inspecting).id(host.id)
                }
            } else {
                ContentUnavailableView("No host", systemImage: "lock.rectangle.stack",
                                       description: Text("A vault is kept on each host. Pick one in the sidebar."))
            }
        }
        .navigationTitle("Vault")
        .navigationSubtitle(subtitle)
        .searchable(text: $search, placement: .toolbar, prompt: "Search items")
        .toolbar {
            ToolbarItemGroup {
                Button("New Item", systemImage: "plus") {
                    if let host = app.host { app.vaultSheet = .editor(VaultEditor(on: host)) }
                }
                    .help("Add a login, card, identity or note")
                    .disabled(!canChange)
                Button("Import…", systemImage: "square.and.arrow.down") { choosingExport = true }
                    .help("Import another password manager's export")
                    .disabled(!canChange)
            }
            ToolbarItem {
                Button("Inspector", systemImage: "sidebar.trailing") { inspecting.toggle() }
                    .help(inspecting ? "Hide the inspector" : "Show the inspector")
            }
        }
        .fileImporter(isPresented: $choosingExport, allowedContentTypes: [.commaSeparatedText, .json, .data]) { result in
            guard case let .success(file) = result, let host = app.host else { return }
            let importer = VaultImport(file: file, on: host)
            app.vaultSheet = .importer(importer)
            Task { await importer.load(app: app) }
        }
        // A sheet works on the host it was opened for, whatever the picker
        // shows meanwhile.
        .sheet(item: $app.vaultSheet) { sheet in
            switch sheet {
            case let .editor(editor): VaultItemSheet(editor: editor)
            case let .importer(importer): ImportSheet(importer: importer)
            }
        }
    }

    /// Items can be added only once the host's vault has been read.
    private var canChange: Bool {
        guard let host = app.host, case .success? = app.summaries[host.name]?.vault else { return false }
        return true
    }

    private var subtitle: String {
        guard let host = app.host else { return "" }
        guard case let .success(vault)? = app.summaries[host.name]?.vault else { return host.name }
        return "\(host.name) · \(vault.items.count) item\(vault.items.count == 1 ? "" : "s")"
    }
}

/// The sheet open over the Vault screen.
enum VaultSheet: Identifiable {
    case editor(VaultEditor)
    case importer(VaultImport)

    var id: ObjectIdentifier {
        switch self {
        case let .editor(editor): ObjectIdentifier(editor)
        case let .importer(importer): ObjectIdentifier(importer)
        }
    }
}

/// One host's items, and their states while there are none.
private struct HostVault: View {
    var host: HostEntry
    var search: String
    @Binding var inspecting: Bool
    @Environment(AppModel.self) private var app
    @State private var removing: VaultList.Item?

    var body: some View {
        @Bindable var app = app
        Group {
            switch app.summaries[host.name]?.vault {
            case nil:
                ProgressView("Reading the vault on \(host.name)…").frame(maxWidth: .infinity, maxHeight: .infinity)
            case let .failure(error)?:
                ContentUnavailableView {
                    Label("Couldn't read the vault on \(host.name)", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error.message)
                } actions: {
                    Button("Try Again") { Task { await app.refreshVault(on: host) } }
                }
            case let .success(vault)? where vault.items.isEmpty:
                ContentUnavailableView {
                    Label("No items on \(host.name)", systemImage: "lock.rectangle.stack")
                } description: {
                    Text("Agents fill logins and cards from here without ever seeing them. Add an item, or import another password manager's export.")
                } actions: {
                    Button("New Item…") { app.vaultSheet = .editor(VaultEditor(on: host)) }
                }
            case let .success(vault)?:
                List(rows(vault), selection: $app.selectedVaultItem) { item in
                    VaultRow(item: item)
                }
                .contextMenu(forSelectionType: VaultList.Item.ID.self) { ids in
                    if let item = item(ids, in: vault) {
                        Button("Edit…", systemImage: "pencil") { edit(item) }
                        Button("Remove…", systemImage: "trash", role: .destructive) { removing = item }
                    }
                } primaryAction: { ids in
                    if let item = item(ids, in: vault) { edit(item) }
                }
                .inspector(isPresented: $inspecting) {
                    Group {
                        if let item = vault.items.first(where: { $0.item == app.selectedVaultItem }) {
                            VaultItemInspector(item: item, show: app.vaultShows[host.name]?[item.item], hostName: host.name,
                                               edit: { edit(item) }, remove: { removing = item })
                                .task(id: "\(item.item)@\(item.revision)") {
                                    guard !app.still else { return }
                                    await app.showVaultItem(item.item, on: host)
                                }
                        } else {
                            ContentUnavailableView("No Selection", systemImage: "lock.rectangle",
                                                   description: Text("Select an item to see what it holds. Secret values are never shown."))
                        }
                    }
                    .inspectorColumnWidth(min: 260, ideal: 320, max: 460)
                }
            }
        }
        .alert("Couldn't remove the item", isPresented: removeErrorBinding) {
            Button("OK") { app.vaultRemoveError = nil }
        } message: {
            Text(app.vaultRemoveError ?? "")
        }
        .confirmationDialog("Remove “\(removing?.title ?? "")” from \(host.name)?", isPresented: removingBinding, presenting: removing) { item in
            Button("Remove", role: .destructive) {
                Task { await app.removeVaultItem(item, on: host) }
            }
        } message: { _ in
            Text("Agents can no longer fill it. This can't be undone.")
        }
        .task {
            guard !app.still else { return }
            await app.refreshVault(on: host)
        }
    }

    private var removeErrorBinding: Binding<Bool> {
        Binding { app.vaultRemoveError != nil } set: { if !$0 { app.vaultRemoveError = nil } }
    }

    private var removingBinding: Binding<Bool> {
        Binding { removing != nil } set: { if !$0 { removing = nil } }
    }

    private func item(_ ids: Set<VaultList.Item.ID>, in vault: VaultList) -> VaultList.Item? {
        guard ids.count == 1 else { return nil }
        return vault.items.first { ids.contains($0.item) }
    }

    private func edit(_ item: VaultList.Item) {
        Task { await app.beginVaultEdit(item, on: host) }
    }

    private func rows(_ vault: VaultList) -> [VaultList.Item] {
        let query = search.trimmingCharacters(in: .whitespaces).lowercased()
        guard !query.isEmpty else { return vault.items }
        return vault.items.filter { item in
            ([item.title, item.kind, item.username ?? ""] + item.sites).contains { $0.lowercased().contains(query) }
        }
    }
}

private struct VaultRow: View {
    var item: VaultList.Item

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: VaultKind(rawValue: item.kind)?.symbol ?? "questionmark.square.dashed")
                .font(.title3)
                .foregroundStyle(.secondary)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.title)
                Text(item.context).font(.callout).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer(minLength: 8)
            if item.totp {
                Text("2FA").font(.caption).foregroundStyle(.secondary)
                    .padding(.horizontal, 6).padding(.vertical, 1)
                    .overlay(Capsule().strokeBorder(.tertiary))
                    .help("Its authenticator code is filled at sign-in")
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
    }
}

extension VaultList.Item {
    /// "ada · shop.example", "Fills on shop.example" or "Fills on any site".
    var context: String {
        let kindTitle = VaultKind(rawValue: kind)?.title ?? kind
        switch kind {
        case "login": return ([username].compactMap { $0 } + [sites.joined(separator: ", ")]).filter { !$0.isEmpty }.joined(separator: " · ")
        case "note": return kindTitle
        default: return "\(kindTitle) · " + (sites.isEmpty ? "Fills on any site" : "Fills on \(sites.joined(separator: ", "))")
        }
    }
}

/// One item: its fields (secret ones only as saved, never shown), where it
/// fills, and Edit and Remove.
struct VaultItemInspector: View {
    var item: VaultList.Item
    /// What `vault show` said; nil while it's read.
    var show: Result<VaultShow, LatchError>?
    var hostName: String
    var edit: () -> Void
    var remove: () -> Void

    var body: some View {
        Form {
            Section {
                Label {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.title).font(.headline)
                        Text(VaultKind(rawValue: item.kind)?.title ?? item.kind).foregroundStyle(.secondary)
                    }
                } icon: {
                    Image(systemName: VaultKind(rawValue: item.kind)?.symbol ?? "questionmark.square.dashed")
                }
            }
            Section("Fields") {
                if shownFields.isEmpty {
                    Text("No fields").foregroundStyle(.secondary)
                }
                if case let .failure(error)? = show {
                    Label(error.message, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
                }
                ForEach(shownFields, id: \.label) { field in
                    LabeledContent(VaultKind.title(of: field.label)) {
                        if field.hidden {
                            Text("Saved, never shown").foregroundStyle(.secondary)
                        } else {
                            Text(value(field.label))
                                .multilineTextAlignment(.trailing)
                                .textSelection(.enabled)
                        }
                    }
                }
            }
            if item.kind != "note" {
                Section(item.kind == "login" ? "Fills on" : "Fills only on") {
                    if item.sites.isEmpty {
                        Text("Any site").foregroundStyle(.secondary)
                    }
                    ForEach(item.sites, id: \.self) { Text($0).font(.body.monospaced()) }
                    ForEach(item.otherTopSites ?? [], id: \.self) { site in
                        LabeledContent { Text("also inside").foregroundStyle(.secondary) } label: {
                            Text(site).font(.body.monospaced())
                        }
                    }
                }
            }
            if item.kind == "login" {
                Section {
                    LabeledContent("Two-factor", value: item.totp ? "Code filled at sign-in" : "None")
                }
            }
            Section {
                HStack {
                    Button("Remove…", role: .destructive, action: remove)
                    Spacer()
                    Button("Edit…", action: edit)
                }
            } footer: {
                Text("Kept on \(hostName). Latch never shows its secret values.")
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }

    /// The fields a person fills in, and any of the item's own: not those
    /// latch derives from them (a card's `expiry`).
    private var shownFields: [VaultList.Field] {
        guard let kind = VaultKind(rawValue: item.kind) else { return item.fields }
        let labels = Set(kind.entries.map(\.label) + ["notes"])
        return item.fields.filter { labels.contains($0.label) || $0.custom == true }
    }

    private func value(_ label: String) -> String {
        switch show {
        case let .success(show)?: show.values[label] ?? "—"
        case .failure?: "—"
        case nil: "…"
        }
    }
}

/// latch's item kinds and the fields a person fills in for each, by
/// latch's labels, with those latch holds secret.
enum VaultKind: String, CaseIterable, Identifiable {
    case login, card, identity, note

    struct Entry: Identifiable {
        var label: String
        var secret = false
        var id: String { label }
    }

    var id: String { rawValue }

    var title: String {
        switch self {
        case .login: "Login"
        case .card: "Card"
        case .identity: "Identity"
        case .note: "Note"
        }
    }

    var symbol: String {
        switch self {
        case .login: "key"
        case .card: "creditcard"
        case .identity: "person.text.rectangle"
        case .note: "note.text"
        }
    }

    /// Every kind's fields; `notes` comes last, on its own.
    var entries: [Entry] {
        switch self {
        case .login:
            [Entry(label: "username"), Entry(label: "password", secret: true), Entry(label: "totp", secret: true)]
        case .card:
            [Entry(label: "cardholder name"), Entry(label: "brand"), Entry(label: "number", secret: true),
             Entry(label: "expiry month"), Entry(label: "expiry year"), Entry(label: "code", secret: true)]
        case .identity:
            ["title", "first name", "middle name", "last name", "email", "phone", "company", "username",
             "address1", "address2", "address3", "city", "state", "postal code", "country", "date of birth"].map { Entry(label: $0) }
                + [Entry(label: "ssn", secret: true), Entry(label: "passport number", secret: true), Entry(label: "license number")]
        case .note:
            []
        }
    }

    /// A field's label in words.
    static func title(of label: String) -> String {
        switch label {
        case "totp": "Authenticator key"
        case "number": "Card number"
        case "code": "Security code"
        case "address1": "Address"
        case "address2": "Address line 2"
        case "address3": "Address line 3"
        case "ssn": "Social Security number"
        default: label.prefix(1).uppercased() + label.dropFirst()
        }
    }
}

/// The New Item and Edit sheet's state: what's typed, and saving it. A
/// secret is typed fresh, never read back: left empty, the saved one stays.
@Observable
final class VaultEditor {
    /// The host whose vault this changes: the one it was opened on.
    let host: HostEntry
    /// The item being changed, as `vault show` said; nil for a new one.
    let original: VaultShow?
    /// An item whose values couldn't be read: the sheet says so, nothing more.
    let unreadable: (item: VaultList.Item, message: String)?
    var kind: VaultKind
    var title: String
    /// Sites as typed: separated by commas or spaces.
    var sites: String
    var otherTopSites: String
    var values: [String: String]
    private(set) var saving = false
    var error: String?

    init(kind: VaultKind = .login, on host: HostEntry) {
        self.host = host
        original = nil
        unreadable = nil
        self.kind = kind
        title = ""
        sites = ""
        otherTopSites = ""
        values = [:]
    }

    init(editing show: VaultShow, on host: HostEntry) {
        self.host = host
        original = show
        unreadable = nil
        kind = VaultKind(rawValue: show.item.kind) ?? .note
        title = show.item.title
        sites = show.item.sites.joined(separator: ", ")
        otherTopSites = (show.item.otherTopSites ?? []).joined(separator: ", ")
        values = show.values
    }

    init(failed item: VaultList.Item, _ message: String, on host: HostEntry) {
        self.host = host
        original = nil
        unreadable = (item, message)
        kind = VaultKind(rawValue: item.kind) ?? .note
        title = item.title
        sites = ""
        otherTopSites = ""
        values = [:]
    }

    var isNew: Bool { original == nil }

    /// Whether a secret field holds a saved value the sheet won't show.
    func saved(_ label: String) -> Bool {
        original?.item.fields.contains { $0.label == label && $0.hidden } == true
    }

    func secret(_ entry: VaultKind.Entry) -> Bool {
        original?.item.fields.first { $0.label == entry.label }?.hidden ?? entry.secret
    }

    /// What stops a save, in words; nil when it can be saved.
    var problem: String? {
        if title.trimmingCharacters(in: .whitespaces).isEmpty { return "Give it a title." }
        if kind == .login, Self.split(sites).isEmpty { return "A login needs its website, or it can never be filled." }
        return nil
    }

    /// What `vault add` or `vault edit` is given: for an edit, only the
    /// values that changed, and a secret only when one was typed.
    var draft: VaultDraft {
        var changed: [String: String] = [:]
        for label in kind.entries.map(\.label) + ["notes"] {
            let typed = values[label] ?? ""
            if isNew || secret(.init(label: label)) {
                if !typed.isEmpty { changed[label] = typed }
            } else if typed != original?.values[label] ?? "" {
                changed[label] = typed
            }
        }
        return VaultDraft(kind: kind.rawValue, title: title.trimmingCharacters(in: .whitespaces), sites: kind == .note ? [] : Self.split(sites),
                          otherTopSites: kind == .login ? Self.split(otherTopSites) : [], values: changed)
    }

    /// Saves on its host; true once saved.
    func save(app: AppModel) async -> Bool {
        guard problem == nil, !saving else { return false }
        saving = true
        defer { saving = false }
        do {
            if let original {
                try await app.editVaultItem(original.item, draft, on: host)
            } else {
                try await app.addVaultItem(draft, on: host)
            }
            error = nil
            return true
        } catch {
            self.error = (error as? LatchError)?.message ?? error.localizedDescription
            return false
        }
    }

    static func split(_ text: String) -> [String] {
        text.split(whereSeparator: { $0 == "," || $0.isWhitespace }).map(String.init)
    }
}

/// New Item, or an item's Edit: a grouped form of its kind's fields.
struct VaultItemSheet: View {
    var editor: VaultEditor
    private var host: HostEntry { editor.host }
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var editor = editor
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text(editor.isNew ? "New Item" : "Edit “\(editor.original?.item.title ?? editor.title)”").font(.headline)
                Text("In \(host.name)'s vault").font(.callout).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding()
            Divider()
            Form {
                if let unreadable = editor.unreadable {
                    Section {
                        Label("Couldn't read “\(unreadable.item.title)”", systemImage: "exclamationmark.triangle")
                        Text(unreadable.message).foregroundStyle(.secondary)
                    }
                } else {
                    Section {
                        if editor.isNew {
                            Picker("Kind", selection: $editor.kind) {
                                ForEach(VaultKind.allCases) { Label($0.title, systemImage: $0.symbol).tag($0) }
                            }
                        }
                        TextField("Title", text: $editor.title, prompt: Text(editor.kind == .login ? "Shop" : "Required"))
                        if editor.kind == .login {
                            TextField("Website", text: $editor.sites, prompt: Text("shop.example"))
                            TextField("Also inside", text: $editor.otherTopSites, prompt: Text("Optional"))
                                .help("Other sites that show this sign-in page inside their own")
                        } else if editor.kind != .note {
                            TextField("Fills only on", text: $editor.sites, prompt: Text("Any site"))
                        }
                    } footer: {
                        Text(siteNote).foregroundStyle(.secondary)
                    }
                    if !editor.kind.entries.isEmpty {
                        Section("Fields") {
                            ForEach(editor.kind.entries) { entry in
                                field(entry)
                            }
                        }
                    }
                    Section(editor.kind == .note ? "Note" : "Notes") {
                        TextEditor(text: value("notes"))
                            .font(.body)
                            .frame(minHeight: editor.kind == .note ? 120 : 50)
                            .accessibilityLabel("Notes")
                    }
                }
            }
            .formStyle(.grouped)
            // What's typed during a save couldn't be saved by it.
            .disabled(editor.saving)
            Divider()
            HStack {
                // A refused save is said here, beside Save, never below the fold.
                if let error = editor.error {
                    Label(error, systemImage: "exclamationmark.triangle").font(.callout).foregroundStyle(.red)
                } else {
                    Text("Secrets go straight to \(host.name)'s vault and are never shown again.")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if editor.unreadable == nil {
                    Button(editor.saving ? "Saving…" : editor.isNew ? "Add" : "Save") {
                        Task { if await editor.save(app: app) { dismiss() } }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(editor.saving || editor.problem != nil)
                    .help(editor.problem ?? "")
                }
            }
            .padding()
        }
        .frame(width: 480, height: editor.kind == .identity ? 640 : 520)
    }

    private var siteNote: String {
        switch editor.kind {
        case .login: "Latch fills it only on this website (separate several with commas)."
        case .card, .identity: "Leave empty to let it fill on any site you send an agent to."
        case .note: "A note isn't tied to a site."
        }
    }

    @ViewBuilder
    private func field(_ entry: VaultKind.Entry) -> some View {
        let title = VaultKind.title(of: entry.label)
        if editor.secret(entry) {
            SecureField(title, text: value(entry.label),
                        prompt: Text(editor.saved(entry.label) ? "Saved · type to replace" : entry.label == "totp" ? "Optional" : ""))
        } else {
            TextField(title, text: value(entry.label))
        }
    }

    private func value(_ label: String) -> Binding<String> {
        Binding { editor.values[label] ?? "" } set: { editor.values[label] = $0 }
    }
}

/// Importing an export: what it holds (`vault import --preview`, no secret
/// shown), the rows picked, and what the import saved.
@Observable
final class VaultImport {
    let file: URL
    /// The host it imports into: the one it was opened on.
    let host: HostEntry
    private(set) var preview: Result<ImportPreview, LatchError>?
    /// Rows to import, by preview number; duplicates start unpicked.
    var picked: Set<Int> = []
    private(set) var importing = false
    private(set) var result: Result<ImportResult, LatchError>?
    /// Said over a preview read again because the last one went stale.
    private(set) var notice: String?

    init(file: URL, on host: HostEntry) {
        self.file = file
        self.host = host
    }

    func load(app: AppModel) async {
        preview = await AppModel.result { try await app.previewImport(self.file, on: host) }
        if case let .success(preview)? = preview { picked = Set(preview.items.filter { !$0.duplicate }.map(\.pick)) }
    }

    /// Imports the picked rows. If the vault or the export changed since the
    /// preview, nothing is saved: the preview is read again, the picks that
    /// still exist kept, for the person to check and import again.
    func run(app: AppModel) async {
        guard case let .success(shown)? = preview, let expect = shown.expect, !picked.isEmpty, !importing else { return }
        importing = true
        defer { importing = false }
        do {
            result = .success(try await app.importRows(picked.sorted(), expect: expect, from: file, on: host))
        } catch let error as LatchError where error.code == "stale_revision" {
            let kept = picked
            await load(app: app)
            if case let .success(fresh)? = preview { picked = kept.intersection(fresh.items.map(\.pick)) }
            notice = Self.changedSincePreview
        } catch {
            result = .failure(error as? LatchError ?? .unreachable(error.localizedDescription))
        }
    }

    /// The preview carries what an import is checked against.
    var checkable: Bool {
        if case let .success(preview)? = preview { return preview.expect != nil }
        return false
    }

    static let changedSincePreview = "The vault or the export changed since this preview, so nothing was imported. Here it is again: check the rows, then import."

    /// For snapshots: as latch said.
    func stage(preview: ImportPreview, result: ImportResult? = nil, notice: String? = nil) {
        self.preview = .success(preview)
        picked = Set(preview.items.filter { !$0.duplicate }.map(\.pick))
        self.result = result.map { .success($0) }
        self.notice = notice
    }
}

struct ImportSheet: View {
    var importer: VaultImport
    private var host: HostEntry { importer.host }
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                Text("Import into \(host.name)'s vault").font(.headline)
                Text(importer.file.lastPathComponent).font(.callout).foregroundStyle(.secondary)
            }
            .padding()
            Divider()
            content.frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                if case let .success(preview)? = importer.preview, importer.result == nil {
                    Text("\(importer.picked.count) of \(preview.items.count) picked · from \(preview.source)")
                        .font(.callout).foregroundStyle(.secondary)
                }
                Spacer()
                if importer.result == nil {
                    Button("Cancel") { dismiss() }
                        .keyboardShortcut(.cancelAction)
                    Button(importer.importing ? "Importing…" : "Import") { Task { await importer.run(app: app) } }
                        .keyboardShortcut(.defaultAction)
                        .disabled(importer.importing || importer.picked.isEmpty || !importer.checkable)
                } else {
                    Button("Done") { dismiss() }
                        .keyboardShortcut(.defaultAction)
                }
            }
            .padding()
        }
        .frame(width: 560, height: 460)
    }

    @ViewBuilder
    private var content: some View {
        switch (importer.preview, importer.result) {
        case (nil, _):
            ProgressView("Reading the export…").frame(maxWidth: .infinity, maxHeight: .infinity)
        case let (.failure(error)?, _):
            ContentUnavailableView {
                Label("Couldn't read this export", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error.message)
            }
        case let (_, .success(result)?):
            ContentUnavailableView {
                Label(Self.saved(result), systemImage: result.failed.isEmpty ? "checkmark.circle" : "exclamationmark.triangle")
            } description: {
                Text(result.failed.map { "\($0.title): \($0.reason)" }.joined(separator: "\n"))
            }
        case let (_, .failure(error)?):
            ContentUnavailableView {
                Label("Couldn't confirm the import", systemImage: "exclamationmark.triangle")
            } description: {
                Text(error.message)
            }
        case let (.success(preview)?, nil):
            List {
                if let notice = importer.notice {
                    Label(notice, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                }
                if preview.expect == nil {
                    Label("\(host.name)'s Latch can't check an import against its preview. Update Latch there, then import.",
                          systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                }
                Section {
                    ForEach(preview.items) { row in
                        Toggle(isOn: pick(row.pick)) {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(row.title)
                                Text(Self.detail(row)).font(.callout).foregroundStyle(.secondary).lineLimit(1)
                            }
                        }
                    }
                } footer: {
                    Text("Only the rows you pick are saved. Their secrets go straight to \(host.name) and are never shown.")
                        .foregroundStyle(.secondary)
                }
                if let skipped = preview.skipped, !skipped.isEmpty {
                    Section("Not imported") {
                        ForEach(skipped, id: \.title) { row in
                            LabeledContent(row.title) { Text(row.reason).foregroundStyle(.secondary).multilineTextAlignment(.trailing) }
                        }
                    }
                }
            }
            // The rows sent are the ones picked when Import was pressed.
            .disabled(importer.importing)
        }
    }

    private func pick(_ number: Int) -> Binding<Bool> {
        Binding { importer.picked.contains(number) } set: { on in
            if on { importer.picked.insert(number) } else { importer.picked.remove(number) }
        }
    }

    /// "Login · ada · shop.example/login · already in the vault".
    static func detail(_ row: ImportPreview.Row) -> String {
        var words = [VaultKind(rawValue: row.kind)?.title ?? row.kind]
        if let username = row.username, !username.isEmpty { words.append(username) }
        if let url = row.url, !url.isEmpty { words.append(url) }
        if row.duplicate { words.append("already in the vault") }
        if let changed = row.changed, !changed.isEmpty { words.append("updates \(changed.joined(separator: ", "))") }
        words += row.warnings ?? []
        return words.joined(separator: " · ")
    }

    static func saved(_ result: ImportResult) -> String {
        var parts = ["\(result.saved) saved"]
        if result.updated > 0 { parts.append("\(result.updated) updated") }
        if result.duplicates > 0 { parts.append("\(result.duplicates) already there") }
        if !result.failed.isEmpty { parts.append("\(result.failed.count) failed") }
        return parts.joined(separator: " · ")
    }
}

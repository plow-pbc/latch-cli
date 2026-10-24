import Foundation
import Testing
@testable import PlowLatchUI

/// A host's vault as a test scripts it: edits may be refused as stale.
final class VaultClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var staleEdits = false
    private(set) var lists = 0
    /// The host each edit was sent to (nil: this Mac's).
    private(set) var editedOn: [String?] = []
    /// Imports refused as stale before one goes through, and the tokens
    /// each import was checked against.
    var staleImports = 0
    private(set) var expected: [String] = []
    private(set) var previews = 0
    /// Holds each `vault show` until opened.
    var showGate: Gate?
    /// Each item was changed elsewhere since the app listed it: latch says
    /// so at the next revision.
    var editedElsewhere = false
    var removals = false

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(Fixtures.rulesStudio) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList {
        lists += 1
        var vault: VaultList = try Fixtures.decode(Fixtures.vaultItems)
        if editedElsewhere { for index in vault.items.indices { vault.items[index].revision += 1 } }
        return vault
    }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow {
        await showGate?.wait()
        var show: VaultShow = try Fixtures.decode(Fixtures.vaultShows[item]!)
        if editedElsewhere { show.item.revision += 1 }
        return show
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws {
        if !removals { throw LatchError.unreachable("not scripted") }
    }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        editedOn.append(host)
        if staleEdits {
            throw LatchError.failed(LatchFailure(code: "stale_revision", message: "item v1 is at revision 3, not 2; list it again"))
        }
        return (try Fixtures.decode(Fixtures.vaultShows[item]!) as VaultShow).item
    }
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview {
        previews += 1
        return (try Fixtures.decode(Fixtures.importPreview) as ImportPreviewFixture).preview
    }
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult {
        expected.append(expect)
        if staleImports > 0 {
            staleImports -= 1
            throw LatchError.failed(LatchFailure(code: "stale_revision", message: "the vault or the export changed since the preview; nothing was saved; preview it again"))
        }
        return try Fixtures.decode(Fixtures.importResult)
    }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct VaultTests {
    @Test func itemsDecodeWithWhichFieldsAreSecret() throws {
        let vault: VaultList = try Fixtures.decode(Fixtures.vaultItems)
        let shop = try #require(vault.items.first { $0.kind == "login" && $0.totp })
        #expect(shop.fields.filter(\.hidden).map(\.label) == ["password", "totp"])
        let grocer = try #require(vault.items.first { $0.otherTopSites?.isEmpty == false })
        #expect(grocer.otherTopSites == ["grocer-login.example"])
        let card: VaultShow = try Fixtures.decode(Fixtures.vaultShows["v3"]!)
        #expect(card.values["cardholder name"] == "Ada L" && card.values["number"] == nil)
    }

    private func app(_ client: VaultClient) throws -> (AppModel, HostEntry) {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return (app, try #require(app.host))
    }

    @Test func anEditSendsOnlyWhatChangedAndAnUntypedSecretStays() throws {
        let (_, studio) = try app(VaultClient())
        let editor = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!), on: studio)
        #expect(editor.draft.values.isEmpty)
        #expect(editor.saved("password") && editor.saved("totp"))
        editor.values["username"] = "ada2"
        editor.values["notes"] = "work account"
        #expect(editor.draft.values == ["username": "ada2", "notes": "work account"])
        editor.values["password"] = "new-secret"
        #expect(editor.draft.values["password"] == "new-secret")
        editor.values["username"] = ""
        #expect(editor.draft.values["username"] == "")
        #expect(editor.draft.sites == ["shop.example"])
        editor.sites = ""
        #expect(editor.problem != nil)
    }

    @Test func anImportIsCheckedAgainstItsPreviewAndAStaleOneIsPreviewedAgain() async throws {
        let client = VaultClient()
        client.staleImports = 1
        let (app, studio) = try app(client)
        let importer = VaultImport(file: URL(filePath: "/tmp/chrome.csv"), on: studio)
        await importer.load(app: app)
        let expect = try #require(try importer.preview?.get().expect)
        importer.picked = [2]
        await importer.run(app: app)
        #expect(importer.result == nil && importer.notice == VaultImport.changedSincePreview)
        #expect(client.previews == 2 && importer.picked == [2])
        await importer.run(app: app)
        #expect(client.expected == [expect, expect])
        guard case let .success(result)? = importer.result else { Issue.record("not imported"); return }
        #expect(result.saved == 1 && result.updated == 1)
    }

    @Test func aLoginTurnedNoteSendsNoSites() throws {
        let (_, studio) = try app(VaultClient())
        let editor = VaultEditor(on: studio)
        editor.title = "Gate code"
        editor.sites = "shop.example"
        editor.kind = .note
        #expect(editor.draft.sites.isEmpty)
    }

    @Test func aStaleEditIsRefusedInTheAppsWordsAndTheVaultIsReadAgain() async throws {
        let client = VaultClient()
        client.staleEdits = true
        let (app, host) = try app(client)
        let editor = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!), on: host)
        editor.values["username"] = "ada2"
        #expect(await editor.save(app: app) == false)
        #expect(editor.error == .unreachable(AppModel.itemChangedElsewhere))
        #expect(client.lists == 1)
    }

    @Test func anEditSavesOnTheHostItWasOpenedOnAfterTheSidebarMoves() async throws {
        let client = VaultClient()
        let (app, studio) = try app(client)
        let editor = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!), on: studio)
        editor.values["username"] = "ada2"
        app.chosenHost = "pi-kitchen"
        #expect(await editor.save(app: app))
        #expect(client.editedOn == [nil])
    }

    @Test func aRefusedRemovalIsKeptToBeShown() async throws {
        let (app, studio) = try app(VaultClient())
        let vault: VaultList = try Fixtures.decode(Fixtures.vaultItems)
        app.selectedVaultItem = OnHost(host: studio.name, id: vault.items[0].item)
        await app.removeVaultItem(vault.items[0], on: studio)
        #expect(app.vaultRemoveError == .unreachable("not scripted"))
        #expect(app.selectedVaultItem == OnHost(host: studio.name, id: vault.items[0].item))
    }

    @Test func aRemovedItemLeavesNoSelectionAndNoDetail() async throws {
        let client = VaultClient()
        client.removals = true
        let (app, studio) = try app(client)
        await app.refreshVault(on: studio)
        app.selectedVaultItem = OnHost(host: studio.name, id: "v1")
        let item = try #require(app.inspectedVaultItem(on: studio))
        await app.showVaultItem(item.item, on: studio)
        await app.removeVaultItem(item, on: studio)
        #expect(app.selectedVaultItem == nil)
        #expect(app.inspectedVaultItem(on: studio) == nil)
        #expect(app.vaultShow(of: item, on: studio) == nil)
    }

    // The sidebar moves to another host while an item's values are read:
    // the read, cancelled, leaves nothing, and a read that does answer is
    // its own host's, never shown for the other's item with that id.
    @Test func aDetailReadIsItsHostsAndACancelledOneLeavesNothing() async throws {
        let client = VaultClient()
        let gate = Gate()
        client.showGate = gate
        let (app, studio) = try app(client)
        let kitchen = try #require(app.hosts.first { $0.name == "pi-kitchen" })
        let item = (try Fixtures.decode(Fixtures.vaultShows["v1"]!) as VaultShow).item
        let kitchenItem = try #require((try Fixtures.decode(Fixtures.remoteVaultItems) as VaultList).items.first { $0.item == item.item })
        let read = Task { await app.showVaultItem(item.item, on: studio) }
        app.chosenHost = kitchen.name
        read.cancel()
        gate.open()
        await read.value
        #expect(app.vaultShow(of: item, on: studio) == nil)
        await app.showVaultItem(item.item, on: studio)
        #expect(app.vaultShow(of: item, on: studio) == .success(try Fixtures.decode(Fixtures.vaultShows["v1"]!)))
        #expect(app.vaultShow(of: kitchenItem, on: kitchen) == nil)
    }

    // The item changed since its values were read: the inspector waits for
    // the new revision's rather than showing the old ones as its.
    @Test func valuesReadAtAnOlderRevisionAreNotTheNewOnes() async throws {
        let (app, studio) = try app(VaultClient())
        var item = (try Fixtures.decode(Fixtures.vaultShows["v1"]!) as VaultShow).item
        await app.showVaultItem(item.item, on: studio)
        #expect(app.vaultShow(of: item, on: studio) != nil)
        item.revision += 1
        #expect(app.vaultShow(of: item, on: studio) == nil)
    }

    // The item was changed elsewhere since the list was read: its values
    // come at a newer revision, and the list is read again so they show.
    @Test func valuesReadAtANewerRevisionReadTheListAgain() async throws {
        let client = VaultClient()
        let (app, studio) = try app(client)
        await app.refreshVault(on: studio)
        app.selectedVaultItem = OnHost(host: studio.name, id: "v1")
        client.editedElsewhere = true
        await app.showVaultItem("v1", on: studio)
        let item = try #require(app.inspectedVaultItem(on: studio))
        #expect(client.lists == 2)
        #expect(app.vaultShow(of: item, on: studio) != nil)
    }

    /// The bundle's latch is given every value on stdin, never in argv.
    @Test func secretsGoOnStdinNeverInArgv() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let latch = folder.appending(path: "latch")
        let item = try #require(Fixtures.vaultShows["v1"]).replacingOccurrences(of: #","values":{"username":"ada"}"#, with: "")
        try """
            #!/bin/sh
            printf '%s\\n' "$@" >> "\(folder.path)/argv"
            cat > "\(folder.path)/stdin"
            echo '\(item)'
            """.write(to: latch, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: latch.path)
        let client = LiveLatchClient(executable: latch, stateDir: folder.path)
        let draft = VaultDraft(kind: "login", title: "Shop", sites: ["shop.example"], otherTopSites: [],
                               values: ["username": "ada", "password": "hunter2-test"])
        _ = try await client.vaultAdd(draft, host: "pi-kitchen")
        let added = try String(contentsOf: folder.appending(path: "argv"), encoding: .utf8)
        #expect(!added.contains("hunter2-test") && added.contains("--stdin") && added.contains("pi-kitchen"))
        let given = try JSONSerialization.jsonObject(with: Data(contentsOf: folder.appending(path: "stdin"))) as? [String: String]
        #expect(given == ["username": "ada", "password": "hunter2-test"])
        _ = try await client.vaultEdit("v1", revision: 2, VaultDraft(kind: "login", title: "Shop", sites: ["shop.example"],
                                                                     otherTopSites: [], values: ["password": "again-secret"]), host: nil)
        let edited = try String(contentsOf: folder.appending(path: "argv"), encoding: .utf8)
        #expect(!edited.contains("again-secret") && edited.contains("--no-other-top-sites"))
        #expect(try String(contentsOf: folder.appending(path: "stdin"), encoding: .utf8).contains("again-secret"))
    }
}

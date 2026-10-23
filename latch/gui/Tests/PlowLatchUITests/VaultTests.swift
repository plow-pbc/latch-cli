import Foundation
import Testing
@testable import PlowLatchUI

/// A host's vault as a test scripts it: edits may be refused as stale.
final class VaultClient: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    var staleEdits = false
    private(set) var lists = 0

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(Fixtures.rulesStudio) }
    func rulesSet(_ text: String) async throws {}
    func pluginsList(host: String?) async throws -> PluginsList { try Fixtures.decode(#"{"plugins":[]}"#) }
    func vaultList(host: String?) async throws -> VaultList {
        lists += 1
        return try Fixtures.decode(Fixtures.vaultItems)
    }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { try Fixtures.decode(Fixtures.vaultShows[item]!) }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        if staleEdits {
            throw LatchError.failed(LatchFailure(code: "stale_revision", message: "item v1 is at revision 3, not 2; list it again"))
        }
        return (try Fixtures.decode(Fixtures.vaultShows[item]!) as VaultShow).item
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
        let preview = (try Fixtures.decode(Fixtures.importPreview) as ImportPreviewFixture).preview
        #expect(preview.source == "Chrome" && preview.items.map(\.pick) == [1, 2] && preview.skipped?.count == 1)
    }

    @Test func anEditSendsOnlyWhatChangedAndAnUntypedSecretStays() throws {
        let editor = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!))
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

    @Test func aStaleEditIsRefusedInTheAppsWordsAndTheVaultIsReadAgain() async throws {
        let client = VaultClient()
        client.staleEdits = true
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        let host = try #require(app.host)
        let editor = VaultEditor(editing: try Fixtures.decode(Fixtures.vaultShows["v1"]!))
        editor.values["username"] = "ada2"
        #expect(await editor.save(on: host, app: app) == false)
        #expect(editor.error == AppModel.itemChangedElsewhere)
        #expect(client.lists == 1)
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

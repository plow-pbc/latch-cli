@testable import PlowLatchUI
import Foundation

/// What a test's latch double doesn't script: never called by its test.
extension LatchClient {
    func hostStatus(_ host: String) async throws -> LocalStatus { throw LatchError.unreachable("not scripted") }
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws { throw LatchError.unreachable("not scripted") }
    func agents(host: String?) async throws -> AgentsList { throw LatchError.unreachable("not scripted") }
    func rulesTry(_ request: TryRequest, host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        throw LatchError.unreachable("not scripted")
    }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { throw LatchError.unreachable("not scripted") }
    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item { throw LatchError.unreachable("not scripted") }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        throw LatchError.unreachable("not scripted")
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws { throw LatchError.unreachable("not scripted") }
    func disable(_ plugin: String) async throws { throw LatchError.unreachable("not scripted") }
    nonisolated func signInGoogle(addAccount: Bool) -> AsyncThrowingStream<GoogleSignInEvent, Error> {
        AsyncThrowingStream { $0.finish(throwing: LatchError.unreachable("not scripted")) }
    }
    func devices() async throws -> DeviceList { throw LatchError.unreachable("not scripted") }
    func removeDevice(_ name: String) async throws { throw LatchError.unreachable("not scripted") }
    func updateCheck() async throws -> UpdateCheck { throw LatchError.unreachable("not scripted") }
    func logout() async throws -> LogoutResult { throw LatchError.unreachable("not scripted") }
    func agentCatalog() async throws -> AgentCatalog { throw LatchError.unreachable("not scripted") }
    func agentStartByText() async throws -> AgentStartByText { throw LatchError.unreachable("not scripted") }
    func removeAgent(_ uid: String) async throws { throw LatchError.unreachable("not scripted") }
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview { throw LatchError.unreachable("not scripted") }
    func vaultImport(_ file: URL, picks: [Int], expect: String, host: String?) async throws -> ImportResult { throw LatchError.unreachable("not scripted") }
    func auditShot(_ record: String, shot: String, host: String?) async throws -> Data { throw LatchError.unreachable("not scripted") }
    func auditJudge(_ id: String, host: String?) async throws -> String { throw LatchError.unreachable("not scripted") }
    // What a double that only follows or pages needs no script for.
    nonisolated var stateDir: String { "/s" }
    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    nonisolated func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    nonisolated func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    nonisolated func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

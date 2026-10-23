@testable import PlowLatchUI
import Foundation

/// What a test's latch double doesn't script: never called by its test.
extension LatchClient {
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws { throw LatchError.unreachable("not scripted") }
    func agents(host: String?) async throws -> AgentsList { throw LatchError.unreachable("not scripted") }
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        throw LatchError.unreachable("not scripted")
    }
    func vaultShow(_ item: String, host: String?) async throws -> VaultShow { throw LatchError.unreachable("not scripted") }
    func vaultAdd(_ draft: VaultDraft, host: String?) async throws -> VaultList.Item { throw LatchError.unreachable("not scripted") }
    func vaultEdit(_ item: String, revision: Int, _ draft: VaultDraft, host: String?) async throws -> VaultList.Item {
        throw LatchError.unreachable("not scripted")
    }
    func vaultRemove(_ item: String, revision: Int, host: String?) async throws { throw LatchError.unreachable("not scripted") }
    func vaultImportPreview(_ file: URL, host: String?) async throws -> ImportPreview { throw LatchError.unreachable("not scripted") }
    func vaultImport(_ file: URL, picks: [Int], host: String?) async throws -> ImportResult { throw LatchError.unreachable("not scripted") }
}

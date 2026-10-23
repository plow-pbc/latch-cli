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
    func disable(_ plugin: String) async throws { throw LatchError.unreachable("not scripted") }
    func devices() async throws -> DeviceList { throw LatchError.unreachable("not scripted") }
    func removeDevice(_ name: String) async throws { throw LatchError.unreachable("not scripted") }
    func updateCheck() async throws -> UpdateCheck { throw LatchError.unreachable("not scripted") }
    func logout() async throws { throw LatchError.unreachable("not scripted") }
    func agentCatalog() async throws -> AgentCatalog { throw LatchError.unreachable("not scripted") }
    func agentLines() async throws -> AgentLines { throw LatchError.unreachable("not scripted") }
    func newAgent(name: String, cloud: String?, tokenFile: URL?, line: String) async throws -> AgentCreated {
        throw LatchError.unreachable("not scripted")
    }
    func agentShow(_ uid: String) async throws -> AgentsList.Agent { throw LatchError.unreachable("not scripted") }
}

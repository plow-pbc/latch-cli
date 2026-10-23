@testable import PlowLatchUI

/// What a test's latch double doesn't script: never called by its test.
extension LatchClient {
    func rulesSet(_ text: String, host: String?, agent: String?, revision: String?) async throws { throw LatchError.unreachable("not scripted") }
    func agents(host: String?) async throws -> AgentsList { throw LatchError.unreachable("not scripted") }
    func rulesTry(_ request: [String], host: String?, agent: String?, newAgent: Bool, draft: String?) async throws -> RulesTry {
        throw LatchError.unreachable("not scripted")
    }
}

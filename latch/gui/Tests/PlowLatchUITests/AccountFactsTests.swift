import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// Settings' account, as `latch status` says it.
@MainActor
struct AccountFactsTests {
    private func facts(_ account: String) throws -> AccountFacts {
        AccountFacts(try Fixtures.decode(#"{"account":\#(account),"hosts":[],"api_url":"https://api.plow.co"}"#), locale: Locale(identifier: "en_US"))
    }

    /// The owner's login: their name, numbers, Google accounts and credit,
    /// in words, and which Plow.
    @Test func theOwnersAccountIsSaidInWords() throws {
        let owner = try facts(#"{"owner_uid":"u","display_name":"Morgan","phones":["+14155550123"],"emails":["morgan@example.com","m.work@example.com"],"available_balance_usd":"48.630000"}"#)
        #expect(owner == AccountFacts(name: "Morgan", phones: "(415) 555-0123", google: "morgan@example.com, m.work@example.com",
                                      credit: "$48.63", overdrawn: false, plow: "api.plow.co"))
        #expect(try facts(#"{"owner_uid":"u","display_name":null,"phones":[],"emails":[],"available_balance_usd":"-2.5"}"#)
            == AccountFacts(name: "Not set", credit: "-$2.50", overdrawn: true, plow: "api.plow.co"))
    }

    /// An agent's login reads none of what only the owner's may.
    @Test func anAgentsLoginShowsNoOwnersFacts() throws {
        let agent = try facts(#"{"owner_uid":"u","display_name":"Morgan","phones":null,"emails":null,"available_balance_usd":null}"#)
        #expect(agent.phones == nil && agent.google == nil && agent.credit == nil)
    }

    @Test func countsAreWords() {
        #expect(AccountFacts.counted(0, "host", online: 0) == "None")
        #expect(AccountFacts.counted(1, "agent") == "1 agent")
        #expect(AccountFacts.counted(3, "host", online: 2) == "3 hosts · 2 online")
    }
}

/// A latch that counts `latch status` and `latch agents`.
final class AccountReads: LatchClient, @unchecked Sendable {
    private let counted = Mutex<[String]>([])
    var calls: [String] { counted.withLock { $0 } }

    func accountStatus() async throws -> AccountStatus {
        counted.withLock { $0.append("status") }
        return try Fixtures.decode(Fixtures.studioHosts)
    }
    func agents(host: String?) async throws -> AgentsList {
        counted.withLock { $0.append("agents") }
        return try Fixtures.decode(Fixtures.agents)
    }
}

@MainActor
struct SettingsReadsTests {
    /// Opening Settings reads the account and agents again only once the
    /// last read is a minute old.
    @Test func settingsReadsAgainOnlyAfterAMinute() async {
        let client = AccountReads()
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), preferences: nil)
        await app.refreshAccountIfStale()
        #expect(client.calls == ["status", "agents"])
        await app.refreshAccountIfStale(now: .now.addingTimeInterval(30))
        #expect(client.calls.count == 2)
        await app.refreshAccountIfStale(now: .now.addingTimeInterval(61))
        #expect(client.calls == ["status", "agents", "status", "agents"])
    }
}

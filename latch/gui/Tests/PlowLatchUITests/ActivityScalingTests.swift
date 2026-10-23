import Foundation
import Testing
@testable import PlowLatchUI

/// How long Activity takes to take in a host's records as they stream, by
/// count: the duplicate check must keep it linear. A measurement, so it
/// runs only when asked for (`LATCH_SCALING=1 swift test --filter
/// ActivityScalingTests`), never in the default run.
@MainActor
@Suite(.enabled(if: ProcessInfo.processInfo.environment["LATCH_SCALING"] == "1"))
struct ActivityScalingTests {
    private static func lines(_ count: Int) throws -> [AuditLine] {
        try (0..<count).map { try LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line($0, op: "exec").utf8)) }
    }

    @Test func takingInRecordsGrowsLinearlyWithTheirCount() throws {
        var times: [Int: Duration] = [:]
        for count in [1_000, 2_000, 5_000, 10_000] {
            let lines = try Self.lines(count)
            let feed = ActivityFeed(client: PagedClient(), host: nil)
            let took = ContinuousClock().measure {
                for line in lines { feed.apply(line) }
            }
            #expect(feed.records.count == count)
            times[count] = took
            print("activity scaling: \(count) records in \(took)")
        }
        // Linear: ten times the records takes well under a hundred times
        // as long (a quadratic check takes about a hundred).
        let ratio = times[10_000]! / times[1_000]!
        print("activity scaling: 10,000 / 1,000 = \(ratio)")
        #expect(ratio < 30)
    }
}

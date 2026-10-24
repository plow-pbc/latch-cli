import Foundation
import Testing
@testable import PlowLatchUI

/// Home's counts and newest five, read from the newest record back: the
/// same as counting every held record, whatever the day and mix.
struct HomeTests {
    /// Records 50 minutes apart over three days, oldest first: connections,
    /// agents' requests allowed and refused, and the owner's own.
    private static let records: [AuditRecord] = (0..<90).map { n in
        let time = Date(timeIntervalSince1970: 1_792_800_000 + Double(n) * 3000)
        let op = n % 4 == 0 ? "connect" : "exec"
        let agent = n % 3 == 0 ? "null" : #""\#(String(format: "%032x", n % 2 + 1))""#
        let name = n % 2 == 0 ? "null" : #""scout""#
        let json = #"{"agent":\#(agent),"caller":"k","caller_name":\#(name),"decision":"\#(n % 5 == 0 ? "deny" : "allow")","details":{"args":{"argv":["ls"]}},"id":"\#(String(format: "%064d", n))","op":"\#(op)","outcome":"ok","time":"\#(ISO8601DateFormatter().string(from: time))"}"#
        return try! LatchJSON.decoder.decode(AuditRecord.self, from: Data(json.utf8))
    }

    @Test(arguments: [0, 20, 45, 70, 88, 89])
    func countsAndTheNewestFiveAreThoseOfEveryRecord(_ dayOf: Int) {
        // Everything up to the record the day is taken from; that day.
        let held = Array(Self.records.prefix(dayOf + 1))
        let day = Calendar.current.dateInterval(of: .day, for: held.last!.time)!
        let home = HomeActivity(held, today: day)

        let calls = held.filter { !$0.isBackground }
        let today = calls.filter { day.start <= $0.time && $0.time < day.end }
        #expect(home.today == today.count)
        #expect(home.refusedToday == today.filter(\.isRefused).count)
        #expect(home.agentsToday == Set(today.compactMap { $0.agent == nil ? nil : $0.callerName ?? "Agent \($0.agent!.prefix(8))" }).sorted())
        #expect(home.recent.map(\.id) == calls.suffix(5).reversed().map(\.id))
    }

    // A day with nothing yet: no counts, and still the newest five.
    @Test func aDayWithNothingYetCountsNoneAndShowsTheNewestFive() {
        let next = Calendar.current.date(byAdding: .day, value: 1, to: Self.records.last!.time)!
        let home = HomeActivity(Self.records, today: Calendar.current.dateInterval(of: .day, for: next)!)
        #expect(home.today == 0 && home.refusedToday == 0 && home.agentsToday.isEmpty)
        #expect(home.recent.map(\.id) == Self.records.filter { !$0.isBackground }.suffix(5).reversed().map(\.id))
    }

    @Test func noCallsIsNoneRecent() {
        let connections = Self.records.filter(\.isBackground)
        #expect(!connections.isEmpty)
        let home = HomeActivity(connections, today: Calendar.current.dateInterval(of: .day, for: connections.last!.time)!)
        #expect(home.recent.isEmpty && home.today == 0)
    }
}

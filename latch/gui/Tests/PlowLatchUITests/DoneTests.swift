import Foundation
import Testing
@testable import PlowLatchUI

/// Done offers to text an agent with a phone line: Messages opens through
/// the app's opener, which a test holds, so it never launches.
@MainActor
struct DoneTests {
    private func agent(_ name: String, number: String?, status: String? = nil, created: String) -> String {
        let line = number.map { #"{"name":null,"number":"\#($0)"}"# } ?? "null"
        let status = status.map { "\"\($0)\"" } ?? "null"
        return #"{"name":"\#(name)","uid":"\#(name)-uid","kind":"cloud","line":\#(line),"status":\#(status),"created_at":"\#(created)"}"#
    }

    private func done(_ agents: [String]) throws -> (Onboarding, () -> [URL]) {
        let memory = ProgressStore.Memory()
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: memory, stateDir: "/Users/me/.latch"), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        app.stage(agents: try Fixtures.decode(#"{"agents":[\#(agents.joined(separator: ","))]}"#), standings: [:])
        return (app.stage(.done, progress: Progress()), { opened })
    }

    @Test func anAgentWithALineIsTextedWithTheDemoRequest() throws {
        let (flow, opened) = try done([agent("scout", number: "+15550000002", status: "active", created: "2026-10-23T05:37:45.397426")])
        #expect(flow.primaryTitle == "Text scout")
        flow.primary()
        #expect(opened() == [URL(string: "sms:+15550000002?&body=Use%20Latch%20to%20say%20%22hello%20world%22%20out%20loud%20on%20my%20Mac.")!])
        // Texting leaves Done up; Open Latch finishes.
        #expect(!flow.progress.finished)
        flow.finish()
        #expect(flow.progress.finished)
    }

    @Test(arguments: [[String](), ["provisioning"], ["no line"]])
    func noAgentToTextLeavesOpenLatch(cases: [String]) throws {
        let agents = cases.map { $0 == "provisioning"
            ? agent("new", number: "+15550000004", status: "provisioning", created: "2026-10-23T06:00:00")
            : agent("own", number: nil, created: "2026-10-23T06:00:00") }
        let (flow, opened) = try done(agents)
        #expect(flow.textAgent == nil)
        #expect(flow.primaryTitle == "Open Latch")
        flow.primary()
        #expect(opened().isEmpty)
        #expect(flow.progress.finished)
    }

    /// The newest agent that can be texted, past a newer one still
    /// provisioning and one with no line.
    @Test func theNewestQualifyingAgentWins() throws {
        let (flow, opened) = try done([
            agent("old", number: "+15550000002", status: "active", created: "2026-10-20T09:00:00"),
            agent("newer", number: "+15550000003", status: "active", created: "2026-10-22T09:00:00.5"),
            agent("newest", number: "+15550000004", status: "provisioning", created: "2026-10-23T09:00:00"),
            agent("lineless", number: nil, status: "active", created: "2026-10-23T10:00:00"),
        ])
        #expect(flow.primaryTitle == "Text newer")
        flow.primary()
        #expect(opened().map(\.absoluteString).allSatisfy { $0.hasPrefix("sms:+15550000003?&body=") })
        #expect(opened().count == 1)
    }
}

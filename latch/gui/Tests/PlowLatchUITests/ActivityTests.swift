import Foundation
import Testing
@testable import PlowLatchUI

@MainActor
struct ActivityTests {
    private func records(_ fixture: String) throws -> [AuditRecord] {
        try AuditPage.parse(Data(fixture.utf8)).records
    }

    @Test func readsEveryRecordOfARealAudit() throws {
        let page = try AuditPage.parse(Data(Fixtures.studioAudit.utf8))
        #expect(page.records.count == 27)
        #expect(!page.hasOlder)
        let gated = try #require(page.records.first { $0.gate?.p != nil })
        #expect(gated.gate?.p?["NOT_COVERED"] != nil)
    }

    @Test func eachOutcomeHasItsWords() throws {
        let all = try records(Fixtures.studioAudit) + records(Fixtures.kitchenAudit)
        let titles = Set(all.map(\.status.title))
        #expect(titles.isSuperset(of: [
            "Connected", "Allowed", "Refused · not covered", "Refused · you said no",
            "Refused · exec is off for this agent", "Refused · Google is off", "Refused · no instructions from you",
        ]))
    }

    @Test func requestsReadAsTyped() throws {
        let all = try records(Fixtures.studioAudit)
        let commands = all.map(\.command)
        #expect(commands.contains("cat Projects/latch-demo/README.md"))
        #expect(commands.contains("rm -rf Finance"))
        #expect(commands.contains("rules set · rules for every agent"))
    }

    @Test func callersAreNamed() throws {
        let all = try records(Fixtures.studioAudit)
        let rules = try #require(all.first { $0.op == "rules" })
        #expect(rules.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("You", "on this Mac"))
        let laptop = try #require(all.first { $0.op == "exec" && $0.agent == nil })
        #expect(laptop.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("You", "laptop"))
        let scout = try #require(all.first { $0.callerName == "scout" })
        #expect(scout.callerLabel(hostKey: "6b55a78f", hostName: "studio", isThisMac: true) == ("scout", "agent"))
        // An agent's first call after a host starts can be unnamed.
        let unnamed = try #require(all.first { $0.agent != nil && $0.callerName == nil })
        #expect(unnamed.callerLabel(hostKey: nil, hostName: "studio", isThisMac: true).who.hasPrefix("Agent "))
    }

    @Test func aFeedHoldsEachRecordOnceAndAtMostItsLimit() throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: nil)
        feed.stage(Fixtures.studioAudit)
        feed.stage(Fixtures.studioAudit)
        #expect(feed.records.count == 27)
        #expect(feed.caughtUp)
        let record = try #require(feed.records.last)
        for n in 0..<ActivityFeed.limit {
            var copy = record
            copy.id = "r\(n)"
            feed.apply(.record(cursor: copy.id, copy))
        }
        #expect(feed.records.count == ActivityFeed.limit)
        #expect(feed.hasOlder)
        #expect(feed.records.last?.id == "r\(ActivityFeed.limit - 1)")
    }

    @Test func aFeedStreamsOnlyWhileWatched() async throws {
        let feed = ActivityFeed(client: FakeLatchClient(), host: "pi-kitchen")
        feed.start()
        for _ in 0..<200 where !feed.caughtUp { try await Task.sleep(for: .milliseconds(5)) }
        #expect(feed.caughtUp)
        #expect(feed.records.contains { $0.outcome == "no_intent" })
        feed.stop()
    }
}

/// Every stream the app reads opens with `hello` (and a newer latch may add
/// line types): each is skipped, never an error.
struct StreamLineTests {
    @Test func unknownLinesAreSkipped() throws {
        for line in [#"{"type":"hello","schema":1}"#, #"{"type":"something_new","x":1}"#] {
            let data = Data(line.utf8)
            #expect(try LatchJSON.decoder.decode(LoginEvent.self, from: data) == .other)
            #expect(try LatchJSON.decoder.decode(AuditLine.self, from: data) == (line.contains("hello") ? .hello : .other))
            guard case .other = try LatchJSON.decoder.decode(StatusLine.self, from: data) else {
                Issue.record("status line \(line) wasn't skipped")
                continue
            }
        }
    }
}

/// Against a real latch and a host it serves, named by the environment
/// (`LATCH_LIVE_LATCH`, `LATCH_LIVE_STATE`, and `LATCH_LIVE_CALLER_STATE`
/// for another of the owner's machines that calls it; with
/// `LATCH_LIVE_REMOTE`, also a host that machine reads with `--host`);
/// skipped without.
@MainActor
struct LiveActivityTests {
    nonisolated static let env = ProcessInfo.processInfo.environment

    @Test(.enabled(if: env["LATCH_LIVE_LATCH"] != nil), arguments: [false, true])
    func aNewRecordArrivesWhileWatchedAndLatchStopsAfter(remote: Bool) async throws {
        guard !remote || Self.env["LATCH_LIVE_REMOTE"] != nil else { return }
        let latch = URL(filePath: try #require(Self.env["LATCH_LIVE_LATCH"]))
        let caller = try #require(Self.env["LATCH_LIVE_CALLER_STATE"])
        // Remote: the caller's machine reads the remote host's audit.
        let state = remote ? caller : try #require(Self.env["LATCH_LIVE_STATE"])
        let host = remote ? Self.env["LATCH_LIVE_REMOTE"]
            : try await LiveLatchClient(executable: latch, stateDir: state).localStatus().peer?.name
        let feed = ActivityFeed(client: LiveLatchClient(executable: latch, stateDir: state), host: remote ? host : nil)
        feed.start()
        for _ in 0..<500 where !feed.caughtUp { try await Task.sleep(for: .milliseconds(10)) }
        #expect(feed.caughtUp)
        let before = feed.records.count
        let marker = "live-\(UUID().uuidString.prefix(8))"
        _ = try await LatchProcess(executable: latch, arguments: ["--state-dir", caller, "exec", "--host", try #require(host),
                                                                   "--", "echo", marker]).output()
        for _ in 0..<1000 where !feed.records.contains(where: { $0.args?.argv?.last == marker }) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(feed.records.count > before)
        #expect(feed.records.contains { $0.command == "echo \(marker)" && $0.status == .allowed(nil) })
        feed.stop()
        try await Task.sleep(for: .milliseconds(500))
        #expect(Self.followers(state) == 0)
    }

    /// `latch audit --follow` runs still started on `state`.
    static func followers(_ state: String) -> Int {
        let ps = Process()
        ps.executableURL = URL(filePath: "/bin/ps")
        ps.arguments = ["-axo", "command"]
        let out = Pipe()
        ps.standardOutput = out
        try? ps.run()
        let listing = out.fileHandleForReading.readDataToEndOfFile()
        ps.waitUntilExit()
        return String(decoding: listing, as: UTF8.self)
            .split(separator: "\n").filter { $0.contains(state) && $0.contains("audit --follow") }.count
    }
}

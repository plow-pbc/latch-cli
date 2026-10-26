import AppKit
import Synchronization
import Testing
@testable import PlowLatchUI

/// A host's audit as latch keeps it: records numbered from 0, each a second
/// after the last, followed as `latch audit --follow` does: the newest
/// `--last`, or every record after `--after`, then `caught_up`, then what's
/// added while it runs. A test can end a stream, add records, or forget
/// where a stream left off.
final class HostLog: LatchClient, @unchecked Sendable {
    let stateDir = "/s"
    private struct State {
        var count: Int
        var follows: [(after: String?, last: Int?)] = []
        var live: AsyncThrowingStream<AuditLine, Error>.Continuation?
        var expired = false
    }
    private let state: Mutex<State>

    init(records: Int) {
        state = Mutex(State(count: records))
    }

    var follows: [(after: String?, last: Int?)] { state.withLock { $0.follows } }
    var count: Int { state.withLock { $0.count } }

    static func id(_ n: Int) -> String { String(format: "%064d", n) }
    static let start = Date(timeIntervalSince1970: 1_792_700_000)
    static func time(_ n: Int) -> Date { start.addingTimeInterval(Double(n)) }

    /// Record n: scout's, helper's or the owner's in turn; every fourth
    /// refused; every tenth a connection.
    static func line(_ n: Int) -> AuditLine {
        let who = switch n % 3 {
        case 0: #""agent":"\#(scout)","caller":"c1","caller_name":"scout""#
        case 1: #""agent":"\#(helper)","caller":"c2","caller_name":"helper""#
        default: #""agent":null,"caller":"k","caller_name":"laptop""#
        }
        let (op, command) = n % 10 == 9 ? ("connect", "") : ("exec", #","command":"latch exec ls item-\#(n)""#)
        let decision = n % 4 == 3 ? #""decision":"deny","outcome":"gatekeeper_denied""# : #""decision":"allow","outcome":"ok""#
        let time = time(n).formatted(.iso8601)
        let text = #"{"type":"record","cursor":"\#(id(n))","record":{"id":"\#(id(n))","time":"\#(time)",\#(who),\#(decision),"details":null,"op":"\#(op)"\#(command)}}"#
        return try! LatchJSON.decoder.decode(AuditLine.self, from: Data(text.utf8))
    }
    static let scout = "ba3c0b3ad1eba7a9a1eb04b67c9db0dd"
    static let helper = "3971582e59baad83250141c8dcc95855"

    func auditFollow(host: String?, after: String?, last: Int?) -> AsyncThrowingStream<AuditLine, Error> {
        let (stream, continuation) = AsyncThrowingStream<AuditLine, Error>.makeStream()
        let (count, expired) = state.withLock { state in
            state.follows.append((after, last))
            state.live = continuation
            return (state.count, state.expired)
        }
        if let after, expired {
            continuation.finish(throwing: LatchError.failed(LatchFailure(code: "cursor_expired", message: "that record isn't in this audit \(after.suffix(4))")))
            state.withLock { $0.expired = false }
            return stream
        }
        let from = after.flatMap { Int($0) }.map { $0 + 1 } ?? max(0, count - (last ?? 50))
        continuation.yield(.snapshot(cursor: Self.id(from - 1), hasOlder: after == nil && from > 0))
        // Sent again after the cursor, as a latch may: held once.
        if from > 0, after != nil { continuation.yield(Self.line(from - 1)) }
        for n in from..<count { continuation.yield(Self.line(n)) }
        continuation.yield(.caughtUp(cursor: Self.id(count - 1), hasOlder: after == nil && from > 0))
        return stream
    }

    /// New records, sent to the stream following, if one is.
    func add(_ n: Int) {
        let (from, live) = state.withLock { state in
            defer { state.count += n }
            return (state.count, state.live)
        }
        for k in from..<(from + n) { live?.yield(Self.line(k)) }
    }

    /// Records written while no stream follows: none sent.
    func write(_ n: Int) {
        state.withLock { $0.count += n }
    }

    /// The stream following drops, as when the host goes out of reach.
    func drop() {
        state.withLock { $0.live }?.finish(throwing: LatchError.failed(LatchFailure(code: "host_unreachable", message: "out of reach")))
    }

    /// The next stream picking up after a record finds it gone.
    func forget() {
        state.withLock { $0.expired = true }
    }

    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [])) }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? { nil }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

/// Activity's table on a host's audit: the opening page and how far back it
/// goes, its filters, records arriving on top with the selection and the
/// inspector kept, picking up after a drop with nothing missed, starting
/// again when latch forgot where it was, the pace of changes, the sort kept,
/// and Quick Look's files.
@MainActor
struct ActivityTableTests {
    private func app(_ client: HostLog) throws -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"))
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        app.place = .host(.activity)
        return app
    }

    /// Activity in a window of its own, off every display, its feed running.
    private func open(_ app: AppModel, autosaveName: String? = nil, previews: QuickLookFiles = .shared)
        -> (ActivityController, NSWindow, ActivityFeed) {
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 900, height: 600), styleMask: [.titled, .resizable],
                               backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let controller = ActivityController(app: app, autosaveName: autosaveName, previews: previews)
        window.contentViewController = controller
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        let feed = app.activityFeed!
        feed.firstRetry = .milliseconds(10)
        feed.start()
        return (controller, window, feed)
    }

    private func close(_ window: NSWindow, _ feed: ActivityFeed) {
        feed.end()
        window.orderOut(nil)
    }

    /// The rows Activity should list, worked out from the records apart
    /// from the app's own filtering.
    private func expected(_ records: [AuditRecord], _ keep: (AuditRecord) -> Bool) -> [String] {
        records.filter { !$0.isBackground && keep($0) }.reversed().map(\.id)
    }

    @Test func itOpensOnTheNewest500AndSaysFromWhen() async throws {
        let client = HostLog(records: 2000)
        let app = try app(client)
        let (controller, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && controller.shown.records == 500 }
        #expect(client.follows.map(\.last) == [ActivityFeed.openingPage])
        #expect(feed.records.map(\.id) == (1500..<2000).map(HostLog.id))
        #expect(controller.shown.since == HostLog.time(1500))
        #expect(controller.table.numberOfRows == expected(feed.records, { _ in true }).count)
        #expect(controller.rows.first?.id == HostLog.id(1999))
    }

    @Test func outcomeAgentAndSearchFilterTheRows() async throws {
        let client = HostLog(records: 600)
        let app = try app(client)
        let (controller, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && controller.shown.records == 500 }
        let studio = try #require(app.host)
        let cases: [(String, () -> Void, (AuditRecord) -> Bool)] = [
            ("all", {}, { _ in true }),
            ("allowed", { app.activityFilter = .allowed }, { $0.decision == "allow" }),
            ("refused", { app.activityFilter = .refused }, { $0.decision != "allow" }),
            ("scout's", { app.activityFilter = .all; app.openActivity(studio, agent: HostLog.scout) }, { $0.agent == HostLog.scout }),
            ("scout's refused", { app.activityFilter = .refused }, { $0.agent == HostLog.scout && $0.decision != "allow" }),
            ("search", { app.activityAgent = nil; app.activityFilter = .all; app.activitySearch = "item-55" },
             { $0.command?.contains("item-55") == true }),
        ]
        for (name, set, keep) in cases {
            set()
            let want = expected(feed.records, keep)
            await waitUntil { controller.rows.map(\.id) == want }
            #expect(controller.rows.map(\.id) == want, "\(name)")
            #expect(controller.table.numberOfRows == want.count, "\(name)")
            #expect(!want.isEmpty, "\(name)")
        }
        // Connections and reads only when asked for.
        app.activitySearch = ""
        app.showConnections = true
        await waitUntil { controller.rows.count == feed.records.count }
    }

    @Test func arrivalsGoOnTopAndTheSelectionAndInspectorStay() async throws {
        let client = HostLog(records: 600)
        let app = try app(client)
        let (controller, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && controller.shown.records == 500 }
        let studio = try #require(app.host)
        let picked = controller.rows[10]
        controller.table.selectRowIndexes([10], byExtendingSelection: false)
        #expect(app.selectedRecord == OnHost(host: studio.name, id: picked.id))
        client.add(600)
        await waitUntil { feed.records.count == 1100 && controller.shown.records == 1100 }
        // Nothing went: the opening page and every arrival.
        #expect(feed.records.map(\.id) == (100..<1200).map(HostLog.id))
        #expect(controller.rows.first?.id == expected(feed.records, { _ in true }).first)
        let row = try #require(controller.rows.firstIndex { $0.id == picked.id })
        #expect(controller.table.selectedRow == row)
        #expect(app.selectedRecord == OnHost(host: studio.name, id: picked.id))
        #expect(app.inspectedRecord(on: studio)?.id == picked.id)
    }

    @Test func afterADropItPicksUpEveryRecordMissedOnce() async throws {
        let client = HostLog(records: 1000)
        let app = try app(client)
        let (controller, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && feed.records.count == 500 }
        client.drop()
        // More than an opening page written while out of reach.
        client.write(700)
        await waitUntil { feed.records.count == 1200 && controller.shown.records == 1200 }
        #expect(client.follows.count == 2)
        #expect(client.follows[1].after == HostLog.id(999))
        // No hole, and the record sent again is held once.
        #expect(feed.records.map(\.id) == (500..<1700).map(HostLog.id))
        #expect(controller.shown.since == HostLog.time(500))
    }

    @Test func whenLatchForgotWhereItLeftOffItStartsAgainFromTheNewest() async throws {
        let client = HostLog(records: 1000)
        let app = try app(client)
        let (controller, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && feed.records.count == 500 }
        client.forget()
        client.write(2000)
        client.drop()
        await waitUntil { feed.caughtUp && feed.records.first?.id == HostLog.id(2500) }
        #expect(client.follows.map(\.last) == [500, nil, 500])
        #expect(feed.records.map(\.id) == (2500..<3000).map(HostLog.id))
        await waitUntil { controller.shown.since == HostLog.time(2500) }
    }

    // 10,000 records arriving at once change the records at most about ten
    // times a second, and every one arrives.
    @Test func aBurstChangesTheRecordsAtMostTenTimesASecond() async throws {
        let client = HostLog(records: 500)
        let app = try app(client)
        let (_, window, feed) = open(app)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && feed.records.count == 500 }
        let changes = Changes(feed)
        let began = ContinuousClock.now
        client.add(10_000)
        await waitUntil(within: .seconds(30)) { feed.records.count == 10_500 }
        let took = ContinuousClock.now - began
        let seconds = Double(took.components.seconds) + Double(took.components.attoseconds) / 1e18
        let count = changes.count
        #expect(count >= 1)
        #expect(Double(count) <= max(2, seconds * 11), "\(count) changes in \(seconds) s")
    }

    @Test func theSortIsKeptForTheNextWindow() async throws {
        let name = "ActivityTableTests-\(UUID().uuidString)"
        defer {
            for key in UserDefaults.standard.dictionaryRepresentation().keys where key.contains(name) {
                UserDefaults.standard.removeObject(forKey: key)
            }
        }
        let client = HostLog(records: 600)
        let app = try app(client)
        do {
            let (controller, window, feed) = open(app, autosaveName: name)
            await waitUntil { feed.caughtUp && controller.shown.records == 500 }
            controller.table.sortDescriptors = [NSSortDescriptor(key: "caller", ascending: true)]
            #expect(controller.rows.first?.callerName == "helper")
            close(window, feed)
        }
        let (again, window, feed) = open(app, autosaveName: name)
        defer { close(window, feed) }
        #expect(again.table.sortDescriptors.first?.key == "caller")
        #expect(again.table.sortDescriptors.first?.ascending == true)
        await waitUntil { again.rows.first?.callerName == "helper" }
    }

    @Test func quickLooksFileIsTheOwnersAndGoesWithIt() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "ActivityTableTests-\(UUID().uuidString)", directoryHint: .isDirectory)
        let previews = QuickLookFiles(dir: dir)
        defer { previews.removeAll() }
        let client = HostLog(records: 100)
        let app = try app(client)
        let (controller, window, feed) = open(app, previews: previews)
        defer { close(window, feed) }
        await waitUntil { feed.caughtUp && !controller.rows.isEmpty }
        func mode(_ url: URL) -> Int? { (try? FileManager.default.attributesOfItem(atPath: url.path))?[.posixPermissions] as? Int }

        controller.table.selectRowIndexes([0], byExtendingSelection: false)
        controller.previewSelected()
        let first = try #require(controller.preview)
        #expect(mode(first) == 0o600)
        #expect(mode(dir) == 0o700)
        #expect(try String(contentsOf: first, encoding: .utf8).hasPrefix(controller.rows[0].command ?? ""))
        // Moving to another request: its file, the last one's gone.
        controller.table.selectRowIndexes([1], byExtendingSelection: false)
        await waitUntil { controller.preview != nil && controller.preview != first }
        #expect(!FileManager.default.fileExists(atPath: first.path))
        let second = try #require(controller.preview)
        #expect(FileManager.default.fileExists(atPath: second.path))
        // Closed: gone.
        controller.endQuickLook()
        #expect(!FileManager.default.fileExists(atPath: second.path))
        // At quit, and at launch for what a crash left: the folder.
        _ = previews.write("left over", name: "Request x.txt")
        previews.removeAll()
        #expect(!FileManager.default.fileExists(atPath: dir.path))
    }
}

/// Counts each change to a feed's records, from now.
@MainActor
private final class Changes {
    private(set) var count = 0
    private let feed: ActivityFeed

    init(_ feed: ActivityFeed) {
        self.feed = feed
        watch()
    }

    private func watch() {
        withObservationTracking { _ = feed.records } onChange: { [weak self] in
            Task { @MainActor in
                guard let self else { return }
                self.count += 1
                self.watch()
            }
        }
    }
}

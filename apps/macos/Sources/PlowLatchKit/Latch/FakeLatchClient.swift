import Foundation
import Synchronization

/// A latch that answers from sample data, for previews, snapshots and design
/// work without a CLI, a login or real data. It remembers what the app does
/// to it (a login, saved folders, sharing started), so onboarding can run
/// end to end.
public final class FakeLatchClient: LatchClient, SharingService {
    public enum Scenario: String, CaseIterable, Sendable {
        case serving, stopped, loggedOut, noActivity, hostError, unreachable
        /// Serving from a `latch serve` the app didn't start.
        case outside
        /// The app's agent is registered but turned off in Login Items.
        case needsApproval
    }

    /// How a fake login ends.
    public enum LoginOutcome: Sendable {
        case verified(after: Duration)
        case expired(after: Duration)
        case failed(LatchFailure)
        /// Shows the code and keeps waiting.
        case waiting
    }

    private struct State {
        var loggedIn: Bool
        var serving: Bool
        var shares: [String]
        var lastError: LatchFailure?
        var watchers: [UUID: AsyncThrowingStream<LatchEvent, Error>.Continuation] = [:]
        var cursor = 100
    }

    public let scenario: Scenario
    /// When set, a new activity entry arrives this often while following.
    public let liveEvery: Duration?
    public let loginOutcome: LoginOutcome
    private let state: Mutex<State>

    public init(_ scenario: Scenario, liveEvery: Duration? = nil, login: LoginOutcome = .verified(after: .seconds(4))) {
        self.scenario = scenario
        self.liveEvery = liveEvery
        loginOutcome = login
        let sample = Fixtures.sampleStatus
        state = Mutex(State(
            loggedIn: scenario != .loggedOut,
            serving: [.serving, .noActivity, .outside].contains(scenario),
            shares: scenario == .loggedOut ? [] : sample.shares,
            lastError: scenario == .hostError
                ? LatchFailure(code: "folder_missing", message: "/Users/owner/work: no such folder") : nil))
    }

    public func localStatus() async throws -> LocalStatus {
        if scenario == .unreachable { throw Self.unreachable }
        var status = Fixtures.sampleStatus
        state.withLock { now in
            if !now.loggedIn { status.peer = nil }
            status.serve = .init(running: now.serving, ready: now.serving, pid: now.serving ? 4242 : nil)
            status.shares = now.shares
            status.lastError = now.lastError
        }
        return status
    }

    public func rules() async throws -> RulesShow {
        if scenario == .unreachable { throw Self.unreachable }
        if !state.withLock({ $0.loggedIn }) {
            throw LatchError.failed(LatchFailure(code: "not_logged_in", message: "not logged in: run `latch login`"))
        }
        if scenario == .noActivity { return RulesShow(host: "mac-host", everyAgent: nil, agents: []) }
        return try Fixtures.decode(RulesShow.self, Fixtures.rules)
    }

    public func events(after cursor: String?, lifecycleOnly: Bool) -> AsyncThrowingStream<LatchEvent, Error> {
        AsyncThrowingStream { continuation in
            if scenario == .unreachable {
                continuation.finish(throwing: Self.unreachable)
                return
            }
            let id = UUID()
            let (host, n) = state.withLock { now in
                now.watchers[id] = continuation
                return (lifecycle(now), now.cursor)
            }
            let activity = scenario == .serving && !lifecycleOnly ? Fixtures.activity : []
            continuation.yield(.snapshot(cursor: "c\(n)", host: host, activity: activity))
            let live = Task { [liveEvery] in
                guard let liveEvery, !lifecycleOnly else { return }
                var n = activity.count
                while !Task.isCancelled {
                    try? await Task.sleep(for: liveEvery)
                    guard state.withLock({ $0.serving }) else { continue }
                    n += 1
                    continuation.yield(.activity(cursor: "c\(n)", Fixtures.entry(n, time: .now)))
                }
            }
            continuation.onTermination = { [self] _ in
                live.cancel()
                _ = state.withLock { $0.watchers.removeValue(forKey: id) }
            }
        }
    }

    public func login() -> AsyncThrowingStream<LoginEvent, Error> {
        let outcome = loginOutcome
        return AsyncThrowingStream { continuation in
            let task = Task {
                continuation.yield(.code(Fixtures.loginCode))
                continuation.yield(.waiting)
                switch outcome {
                case let .verified(after):
                    try? await Task.sleep(for: after)
                    guard !Task.isCancelled else { return }
                    state.withLock { $0.loggedIn = true }
                    continuation.yield(.verified(Fixtures.sampleStatus.peer!))
                case let .expired(after):
                    try? await Task.sleep(for: after)
                    continuation.yield(.expired)
                case let .failed(failure):
                    continuation.yield(.error(failure))
                case .waiting:
                    return
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func setShares(_ folders: [String]) async throws -> Shares {
        state.withLock { $0.shares = folders }
        return Shares(shares: folders)
    }

    public func agent() -> SharingAgent {
        switch scenario {
        case .outside: .off
        case .needsApproval: .needsApproval
        default: state.withLock { $0.serving } ? .on : .off
        }
    }

    public func startSharing() async throws {
        change { $0.serving = true }
    }

    public func stopSharing() async throws {
        change { $0.serving = false }
    }

    /// Changes the host and tells everyone following it.
    private func change(_ update: (inout State) -> Void) {
        state.withLock { now in
            update(&now)
            now.cursor += 1
            for watcher in now.watchers.values {
                watcher.yield(.host(cursor: "c\(now.cursor)", lifecycle(now)))
            }
        }
    }

    private func lifecycle(_ state: State) -> HostLifecycle {
        if let error = state.lastError { return .error(error.message) }
        return state.serving ? .serving : .stopped
    }

    private static let unreachable = LatchError.unreachable(
        "Couldn't run latch at /Applications/Plow Latch.app/Contents/Helpers/latch: The file doesn’t exist.")
}

/// Sample CLI output, in the exact shapes latch prints.
public enum Fixtures {
    public static let statusServing = """
    {"state_dir":"/Users/owner/.latch","version":"0.9.0","protocol":1,
     "peer":{"name":"mac-host","key":"k7f3a9c2e1d04b58a6f2c9e3d1b7a4f0","kind":"device"},
     "serve":{"running":true,"ready":true,"pid":4242},
     "shares":["/Users/owner/work","/Users/owner/Documents/shared"],
     "last_error":null}
    """

    public static let rules = """
    {"host":"mac-host",
     "every_agent":{"text":"Ask before deleting anything outside ~/work/scratch.\\nNever email anyone who isn't already in the thread.","revision":"a1b2c3"},
     "agents":[{"agent_uid":"33333333333333333333333333333333","agent_name":"owner-laptop agent","text":"May run the test suite in ~/work.","revision":"d4e5f6"}],
     "warning":"Freeform rules are judged, not guaranteed. --deny-exec is enforced in code."}
    """

    public static let activityLines = """
    {"type":"snapshot","cursor":"c3","host":{"state":"serving"},"activity":[{"id":"h1","time":"2026-10-20T09:12:03.120394857Z","caller":"k1","caller_name":"owner-laptop","agent":null,"op":"file.read","decision":"allow","outcome":"ok"},{"id":"h2","time":"2026-10-20T09:14:41.5Z","caller":"k2","caller_name":"build-agent","agent":"a9","op":"exec","decision":"refuse","outcome":"gatekeeper refused: outside the standing rules"},{"id":"h3","time":"2026-10-20T09:20:00Z","caller":"k1","caller_name":"owner-laptop","agent":null,"op":"browser.open","decision":"allow","outcome":"ok"}]}
    {"type":"host","cursor":"c4","state":"stopped"}
    {"type":"activity","cursor":"c5","entry":{"id":"h4","time":"2026-10-20T09:21:07.000001Z","caller":"k2","op":"google","decision":"allow","outcome":"failed: token expired"}}
    {"type":"reset"}
    """

    static let sampleStatus = try! decode(LocalStatus.self, statusServing)

    public static let loginLines = """
    {"type":"code","code":"K7P2Q","send_to":"+15555551234","send_to_display":"+1 555-555-1234","smsto":"SMSTO:+15555551234:K7P2Q","sms_link":"sms:+15555551234?&body=K7P2Q","expires_at":"2026-10-20T21:40:00Z"}
    {"type":"waiting"}
    {"type":"verified","peer":{"name":"mac-host","key":"k7f3a9c2e1d04b58a6f2c9e3d1b7a4f0","kind":"device"}}
    {"type":"expired"}
    {"type":"error","error":{"code":"login_not_found","message":"this sign-in code is no longer valid: get a new one"}}
    """

    public static var loginCode: LoginCode {
        let first = loginLines.split(separator: "\n").first.map(String.init) ?? ""
        guard case let .code(code) = try? decode(LoginEvent.self, first) else { fatalError("bad login fixture") }
        return code
    }

    /// The snapshot's activity from `activityLines`, oldest first.
    public static var activity: [ActivityEntry] {
        let first = activityLines.split(separator: "\n").first.map(String.init) ?? ""
        guard case let .snapshot(_, _, activity) = try? decode(LatchEvent.self, first) else { return [] }
        return activity
    }

    static func entry(_ n: Int, time: Date) -> ActivityEntry {
        ActivityEntry(
            id: "live\(n)", time: time, caller: "k1", callerName: "owner-laptop",
            op: ["file.read", "exec", "browser.act"][n % 3], decision: "allow", outcome: "ok")
    }

    public static func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try LatchJSON.decoder.decode(type, from: Data(json.utf8))
    }
}

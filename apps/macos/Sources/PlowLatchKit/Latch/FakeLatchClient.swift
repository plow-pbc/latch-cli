import Foundation

/// A latch that answers from sample data, for previews, snapshots and design
/// work without a CLI, a login or real data.
public struct FakeLatchClient: LatchClient {
    public enum Scenario: String, CaseIterable, Sendable {
        case serving, stopped, loggedOut, noActivity, hostError, unreachable
    }

    public let scenario: Scenario
    /// When set, a new activity entry arrives this often while following.
    public let liveEvery: Duration?

    public init(_ scenario: Scenario, liveEvery: Duration? = nil) {
        self.scenario = scenario
        self.liveEvery = liveEvery
    }

    public func localStatus() async throws -> LocalStatus {
        if scenario == .unreachable { throw Self.unreachable }
        var status = try Fixtures.decode(LocalStatus.self, Fixtures.statusServing)
        switch scenario {
        case .stopped:
            status.serve = .init(running: false, ready: false)
        case .loggedOut:
            status.peer = nil
            status.serve = .init(running: false, ready: false)
            status.shares = []
        case .hostError:
            status.serve = .init(running: false, ready: false)
            status.lastError = LatchFailure(
                code: "folder_missing", message: "/Users/owner/work: no such folder")
        case .serving, .noActivity, .unreachable:
            break
        }
        return status
    }

    public func rules() async throws -> RulesShow {
        switch scenario {
        case .unreachable: throw Self.unreachable
        case .loggedOut:
            throw LatchError.failed(LatchFailure(
                code: "not_logged_in", message: "not logged in: run `latch login`"))
        case .noActivity: return RulesShow(host: "mac-host", everyAgent: nil, agents: [])
        default: return try Fixtures.decode(RulesShow.self, Fixtures.rules)
        }
    }

    public func events(after cursor: String?, lifecycleOnly: Bool) -> AsyncThrowingStream<LatchEvent, Error> {
        let scenario = scenario
        let liveEvery = liveEvery
        return AsyncThrowingStream { continuation in
            let task = Task {
                if scenario == .unreachable {
                    continuation.finish(throwing: Self.unreachable)
                    return
                }
                let host: HostLifecycle = switch scenario {
                case .serving, .noActivity: .serving
                case .hostError: .error("/Users/owner/work: no such folder")
                default: .stopped
                }
                let activity = scenario == .serving && !lifecycleOnly ? Fixtures.activity : []
                continuation.yield(.snapshot(cursor: "c\(activity.count)", host: host, activity: activity))
                guard let liveEvery, !lifecycleOnly, host == .serving else { return }
                var n = activity.count
                while !Task.isCancelled {
                    try? await Task.sleep(for: liveEvery)
                    n += 1
                    continuation.yield(.activity(cursor: "c\(n)", Fixtures.entry(n, time: .now)))
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
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

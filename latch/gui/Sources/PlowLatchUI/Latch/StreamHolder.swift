#if DEBUG
import Foundation

/// For the tests: an app's streams on latch, held by `LiveLatchClient` as
/// the app holds them, until this process is killed. `status` is
/// `status --local --follow`, `audit` this Mac's `audit --follow`.
package enum StreamHolder {
    package static func run(latch: URL, stateDir: String, streams: [String]) async {
        let client = LiveLatchClient(executable: latch, stateDir: stateDir)
        await withTaskGroup(of: Void.self) { group in
            for stream in streams {
                group.addTask {
                    do {
                        switch stream {
                        case "status": for try await _ in client.statusFollow() {}
                        case "audit": for try await _ in client.auditFollow(host: nil, after: nil, last: nil) {}
                        default: break
                        }
                    } catch {}
                }
            }
        }
        // A stream that ended on its own: stay, so only a kill ends this.
        try? await Task.sleep(for: .seconds(3600))
    }
}
#endif

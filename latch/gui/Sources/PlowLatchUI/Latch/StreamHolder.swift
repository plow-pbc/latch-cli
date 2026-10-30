#if DEBUG
import Foundation

/// For the tests: an app's streams on latch, held by `LiveLatchClient` as
/// the app holds them, until this process is killed. `status` is
/// `status --local --follow`, `audit` this Mac's `audit --follow`, and
/// `update` runs `latch update` as Install Update does. Quit with SIGTERM,
/// as the installer quits the app, it stops what it ran as the app does
/// when it quits.
package enum StreamHolder {
    package static func run(latch: URL, stateDir: String, streams: [String]) async {
        let client = LiveLatchClient(executable: latch, stateDir: stateDir)
        signal(SIGTERM, SIG_IGN)
        let quit = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .global())
        quit.setEventHandler {
            LatchProcess.terminateAll()
            exit(0)
        }
        quit.resume()
        await withTaskGroup(of: Void.self) { group in
            for stream in streams {
                group.addTask {
                    do {
                        switch stream {
                        case "status": for try await _ in client.statusFollow() {}
                        case "audit": for try await _ in client.auditFollow(host: nil, after: nil, last: nil) {}
                        case "update": _ = try await client.update()
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

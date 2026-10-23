import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// latch's streams as processes: shell scripts standing in for latch.
@MainActor
struct StreamTests {
    /// An executable script in a fresh folder, removed by the caller.
    private func script(_ body: String) throws -> (folder: URL, script: URL) {
        let folder = FileManager.default.temporaryDirectory.appending(path: "stream-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let script = folder.appending(path: "latch")
        try "#!/bin/sh\n\(body)\n".write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        return (folder, script)
    }

    /// One stream that has gone quiet doesn't hold up another's lines: each
    /// is read as it arrives, not through a shared queue a waiting read
    /// would block.
    @Test func aQuietStreamDoesNotHoldUpAnother() async throws {
        let quiet = try script("echo first\nexec sleep 30")
        let busy = try script("echo second\nexec sleep 30")
        defer {
            try? FileManager.default.removeItem(at: quiet.folder)
            try? FileManager.default.removeItem(at: busy.folder)
        }
        let quietLatch = LatchProcess(executable: quiet.script, arguments: [])
        let busyLatch = LatchProcess(executable: busy.script, arguments: [])
        defer {
            quietLatch.terminate()
            busyLatch.terminate()
        }
        // Read on: after its first line, its next is awaited and never comes.
        let quietLines = try quietLatch.lines()
        let first = Mutex<String?>(nil)
        let waiting = Task.detached {
            for await line in quietLines { first.withLock { $0 = $0 ?? line } }
        }
        defer { waiting.cancel() }
        await waitUntil { first.withLock { $0 } != nil }
        #expect(first.withLock { $0 } == "first")
        let second = Mutex<String?>(nil)
        let reader = Task.detached {
            for await line in try busyLatch.lines() {
                second.withLock { $0 = line }
                break
            }
        }
        defer { reader.cancel() }
        await waitUntil(within: .seconds(5)) { second.withLock { $0 } != nil }
        #expect(second.withLock { $0 } == "second")
    }
}

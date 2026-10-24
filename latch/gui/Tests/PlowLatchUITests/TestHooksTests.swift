import AppKit
import SwiftUI
import Testing
@testable import PlowLatchUI

// One at a time: they share the hooks' timings file.
@MainActor @Suite(.serialized)
struct TestHooksTests {
    // A hook's argument can be a vault value (`vault-type password=…`):
    // timing keeps the hook's name, never what it was given.
    @Test func aTimedHookKeepsItsNameAndNeverItsArgument() async throws {
        let file = FileManager.default.temporaryDirectory.appending(path: "timings-\(UUID().uuidString).jsonl")
        defer { try? FileManager.default.removeItem(at: file) }
        TestHooks.timings = file
        defer { TestHooks.timings = nil }
        var given: String?
        TestHooks.run("vault-type", "password=hunter2") { given = $0 }
        #expect(given == "password=hunter2")
        await waitUntil { FileManager.default.fileExists(atPath: file.path) }
        let line = try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        #expect(line?["command"] as? String == "vault-type")
        #expect(try !String(contentsOf: file, encoding: .utf8).contains("hunter2"))
    }

    // A window kept off every display, as another test's is, never
    // refreshes: a hook isn't timed on it, so its line doesn't wait.
    @Test func aWindowOnNoDisplayIsNotWaitedFor() async throws {
        let file = FileManager.default.temporaryDirectory.appending(path: "timings-\(UUID().uuidString).jsonl")
        defer { try? FileManager.default.removeItem(at: file) }
        TestHooks.timings = file
        defer { TestHooks.timings = nil }
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 300, height: 200), styleMask: [.titled],
                               backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: Text("x"))
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        #expect(window.screen == nil)
        // Drawn by the window server, as a window open a while is.
        await waitUntil { Render.image(window) != nil }
        TestHooks.run("section", "vault") { _ in }
        await waitUntil(within: .seconds(4)) { FileManager.default.fileExists(atPath: file.path) }
        let line = try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        #expect(line?["command"] as? String == "section" && line?["visible_ms"] is NSNull)
    }
}

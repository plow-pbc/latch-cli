import Foundation
import Testing
@testable import PlowLatchUI

@MainActor
struct TestHooksTests {
    // A hook's argument can be a vault value (`vault-type password=…`):
    // timing keeps the hook's name, never what it was given.
    @Test func aTimedHookKeepsItsNameAndNeverItsArgument() throws {
        let file = FileManager.default.temporaryDirectory.appending(path: "timings-\(UUID().uuidString).jsonl")
        defer { try? FileManager.default.removeItem(at: file) }
        TestHooks.timings = file
        defer { TestHooks.timings = nil }
        var given: String?
        TestHooks.run("vault-type", "password=hunter2") { given = $0 }
        #expect(given == "password=hunter2")
        let line = try JSONSerialization.jsonObject(with: Data(contentsOf: file)) as? [String: Any]
        #expect(line?["command"] as? String == "vault-type")
        #expect(try !String(contentsOf: file, encoding: .utf8).contains("hunter2"))
    }
}

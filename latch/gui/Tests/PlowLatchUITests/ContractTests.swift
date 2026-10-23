import Foundation
import Testing
@testable import PlowLatchUI

/// What the real latch prints, read by the app's own client and models, on
/// a fresh state folder that was never logged in: no Plow, no host. The
/// latch is `LATCH_CONTRACT_LATCH`, else the debug build
/// (`$CARGO_TARGET_DIR` or `latch/target`, `/debug/latch`); without one the
/// suite fails: build it (`cargo build` in `latch/`), or leave the suite out
/// with `swift test --skip ContractTests`.
@MainActor
final class ContractTests {
    nonisolated static let env = ProcessInfo.processInfo.environment
    /// latch/, from this file's place in latch/gui/Tests/PlowLatchUITests.
    static let latchDir = URL(filePath: #filePath).deletingLastPathComponent().appending(path: "../../..").standardized

    let latch: URL
    let state: URL

    init() throws {
        let built = Self.env["CARGO_TARGET_DIR"].map { URL(filePath: $0) } ?? Self.latchDir.appending(path: "target")
        latch = Self.env["LATCH_CONTRACT_LATCH"].map { URL(filePath: $0) } ?? built.appending(path: "debug/latch")
        try #require(FileManager.default.isExecutableFile(atPath: latch.path),
                     "no latch at \(latch.path): run `cargo build` in latch/, or set LATCH_CONTRACT_LATCH")
        state = Self.latchDir.appending(path: "../.dev/tests/contract-\(UUID().uuidString.prefix(8))").standardized
        try FileManager.default.createDirectory(at: state, withIntermediateDirectories: true)
    }

    private var client: LiveLatchClient { LiveLatchClient(executable: latch, stateDir: state.path) }

    deinit {
        try? FileManager.default.removeItem(at: state)
    }

    @Test func statusOfAFreshStateFolder() async throws {
        let status = try await client.localStatus()
        #expect(status.peer == nil)
        #expect(status.serve.state == "stopped" && !status.serve.running)
    }

    /// The first line of `status --local --follow` is the status; stopping
    /// the iteration ends that latch.
    @Test func statusFollowStartsWithTheStatusAndStopsWithTheApp() async throws {
        var first: LocalStatus?
        for try await status in client.statusFollow() {
            first = status
            break
        }
        #expect(first?.serve.state == "stopped")
        await waitUntil { latchRuns(on: state.path, doing: "status --local --follow") == 0 }
    }

    @Test func setupStatusOfAFreshStateFolder() async throws {
        let setup = try await client.setupStatus()
        #expect(setup.step("login")?.state == "needed")
        #expect(setup.step("host") != nil)
        for connector in ["google", "messages", "contacts"] {
            #expect(setup.step(connector)?.enabled == false)
        }
    }

    @Test func pluginsListOfAFreshStateFolder() async throws {
        let plugins = try await client.pluginsList(host: nil)
        #expect(Set(plugins.plugins.map(\.name)) == ["google", "messages", "contacts"])
        #expect(plugins.plugins.allSatisfy { $0.state == "disabled" })
    }

    /// A command that needs a login refuses with latch's typed error, which
    /// the app decides on.
    @Test func plowReadsRefuseWithoutALogin() async throws {
        let refusal = await AppModel.result { try await client.accountStatus() }
        guard case let .failure(error) = refusal else {
            Issue.record("`status` answered without a login: \(refusal)")
            return
        }
        #expect(error.code == "not_logged_in")
    }
}

/// latch processes on `state` still running `command`.
func latchRuns(on state: String, doing command: String) -> Int {
    let ps = Process()
    ps.executableURL = URL(filePath: "/bin/ps")
    ps.arguments = ["-axo", "command"]
    let out = Pipe()
    ps.standardOutput = out
    try? ps.run()
    let listing = out.fileHandleForReading.readDataToEndOfFile()
    ps.waitUntilExit()
    return String(decoding: listing, as: UTF8.self)
        .split(separator: "\n").filter { $0.contains(state) && $0.contains(command) }.count
}

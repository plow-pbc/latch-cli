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
        // Cargo, run in latch/, reads a relative CARGO_TARGET_DIR from there.
        let built = Self.env["CARGO_TARGET_DIR"].map { $0.hasPrefix("/") ? URL(filePath: $0) : Self.latchDir.appending(path: $0) }
            ?? Self.latchDir.appending(path: "target")
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
        #expect(status.login == nil)
        #expect(status.thisMachine.hosting.state == "stopped" && !status.thisMachine.hosting.running)
    }

    /// The first line of `status --local --follow` is the status; stopping
    /// the iteration ends that latch.
    @Test func statusFollowStartsWithTheStatusAndStopsWithTheApp() async throws {
        var first: LocalStatus?
        for try await status in client.statusFollow() {
            first = status
            break
        }
        #expect(first?.thisMachine.hosting.state == "stopped")
        await waitUntil { latchRuns(on: state.path, doing: "status --local --follow") == 0 }
    }

    /// The app killed outright: its stream's latch sees its stdin close and
    /// ends, with no one left to stop it. Starting the holder and latch takes
    /// about half a second.
    @Test func aStreamEndsWhenItsAppIsKilled() async throws {
        let app = try HeldStreams(latch: latch, state: state.path, ["status"])
        defer { app.stop() }
        await waitUntil { latchRuns(on: state.path, doing: "status --local --follow") == 1 }
        app.killApp()
        await waitUntil { latchRuns(on: state.path, doing: "status --local --follow") == 0 }
        #expect(latchRuns(on: state.path, doing: "status --local --follow") == 0)
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

    /// `status` before login answers here, asking no one: no hosts.
    @Test func accountStatusOfAFreshStateFolder() async throws {
        let account = try await client.accountStatus()
        #expect(account.hosts == nil)
    }

    /// A command that needs a login refuses with latch's typed error, which
    /// the app decides on.
    @Test func plowReadsRefuseWithoutALogin() async throws {
        let refusal = await AppModel.result { try await client.agents(host: nil) }
        guard case let .failure(error) = refusal else {
            Issue.record("`agents` answered without a login: \(refusal)")
            return
        }
        #expect(error.code == "not_logged_in")
    }
}

/// The app's streams on latch, held by `LiveLatchClient` in a process of
/// their own (`StreamHolder`), so a test can kill that process outright
/// with SIGKILL, as the app dying with no chance to stop them.
struct HeldStreams {
    let holder = Process()

    /// `streams`: `status` (`status --local --follow`), `audit` (`audit --follow`).
    init(latch: URL, state: String, _ streams: [String]) throws {
        let products = Bundle(for: ContractTests.self).bundleURL.deletingLastPathComponent()
        holder.executableURL = products.appending(path: "StreamHolder")
        holder.arguments = [latch.path, state] + streams
        holder.standardOutput = FileHandle.nullDevice
        holder.standardError = FileHandle.nullDevice
        try holder.run()
    }

    func killApp() {
        kill(holder.processIdentifier, SIGKILL)
    }

    func stop() {
        if holder.isRunning { killApp() }
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

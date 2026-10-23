import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// A latch whose `plugins enable google --events` sends what a test says,
/// and which counts the waits the app stopped.
final class SignInClient: LatchClient, @unchecked Sendable {
    let stateDir = "/Users/me/.latch"
    /// Google's step in `setup --status`.
    var google = Fixtures.googleNotConnected
    /// Each sign-in asked for: whether it was `--add-account`.
    private(set) var signIns: [Bool] = []
    private(set) var events: AsyncThrowingStream<GoogleSignInEvent, Error>.Continuation?
    private let stops = Mutex(0)
    var stopped: Int { stops.withLock { $0 } }
    private(set) var enabled: [String] = []
    /// latch's refusal of a plain enable, when a test gives one.
    var enableRefusal: LatchFailure?

    func signInGoogle(addAccount: Bool) -> AsyncThrowingStream<GoogleSignInEvent, Error> {
        signIns.append(addAccount)
        let (stream, continuation) = AsyncThrowingStream<GoogleSignInEvent, Error>.makeStream()
        continuation.onTermination = { [weak self] end in
            if case .cancelled = end { self?.stops.withLock { $0 += 1 } }
        }
        events = continuation
        return stream
    }

    func setupStatus() async throws -> SetupStatus { try Fixtures.decode(Fixtures.setup(on: [], google: google)) }
    func enable(_ plugin: String) async throws -> GoogleEnabled? {
        enabled.append(plugin)
        if let enableRefusal { throw LatchError.failed(enableRefusal) }
        // As the host says it: the accounts that work, and the rest apart.
        let step = try await setupStatus().step("google")
        let again = step?.needsSignIn
        return GoogleEnabled(accounts: (step?.accounts ?? []).filter { !(again ?? []).contains($0) }, needsSignIn: again)
    }
    func pluginsList(host: String?) async throws -> PluginsList { try Fixtures.decode(Fixtures.studioPluginsSetUp) }
    func localStatus() async throws -> LocalStatus { try Fixtures.decode(Fixtures.serving) }
    func accountStatus() async throws -> AccountStatus { try Fixtures.decode(Fixtures.studioHosts) }
    func login(name: String?, replace: Bool) -> AsyncThrowingStream<LoginEvent, Error> { AsyncThrowingStream { $0.finish() } }
    func setupStatusCheckingAccess() async throws -> SetupStatus { try await setupStatus() }
    func rulesShow(host: String?) async throws -> RulesShow { try Fixtures.decode(#"{"agents":[],"every_agent":null}"#) }
    func rulesSet(_ text: String) async throws {}
    func vaultList(host: String?) async throws -> VaultList { try Fixtures.decode(#"{"items":[]}"#) }
    func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { AsyncThrowingStream { _ in } }
    func audit(host: String?, before: String?) async throws -> AuditPage { AuditPage(records: [], hasOlder: false) }
    func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { AsyncThrowingStream { _ in } }
    func perform(_ args: [String]) async throws {}
    func stop() async throws {}
}

@MainActor
struct GoogleSignInTests {
    /// Links as Plow mints them: the code is in the path.
    let first = URL(string: "http://127.0.0.1:18934/v1/connectors/gmail/connect/FIRSTsecretCODE")!
    let renewed = URL(string: "http://127.0.0.1:18934/v1/connectors/gmail/connect/RENEWEDsecretCODE")!
    let timeout = LatchError.failed(LatchFailure(code: "sign_in_timeout", message: "No new Google account was observed before the wait ended."))
    /// A link no other test uses, and a refusal whose message carries it:
    /// neither may reach what the app shows or saves.
    let canary = URL(string: "https://canary.invalid/v1/connectors/gmail/connect/CANARY-5b1e9d")!
    let canaryCode = "CANARY-5b1e9d"
    var canaryRefusal: LatchError {
        .failed(LatchFailure(code: "something_new", message: "sign in at \(canary.absoluteString), then try again"))
    }

    private func app(_ client: SignInClient) -> (AppModel, ProgressStore.Memory, () -> [URL]) {
        let memory = ProgressStore.Memory()
        let app = AppModel(client: client, store: ProgressStore(memory: memory, stateDir: client.stateDir), still: true)
        var opened: [URL] = []
        app.open = { opened.append($0) }
        return (app, memory, { opened })
    }

    /// Connect opens latch's first link and waits; a renewed link replaces
    /// it for "open again" without opening a second page; latch's result
    /// is Google on, with no enable of the app's own.
    /// Every connected account needs signing in again: the enable is
    /// refused with google_needs_sign_in, and the screen offers Sign In
    /// Again…, whose sign-in latch runs, never a Try again that repeats the
    /// refused enable.
    @Test func signingInAgainOffersSignInAgain() async {
        let client = SignInClient()
        client.google = #"{"step":"google","state":"needed","reason":"Google needs you to sign in again with testuser@plow.test: `latch plugins enable google` signs you in.","args":["plugins","enable","google","--events"],"enabled":false,"ready":false,"accounts":["testuser@plow.test"],"needs_sign_in":["testuser@plow.test"]}"#
        client.enableRefusal = LatchFailure(code: "google_needs_sign_in", message: "Google needs the owner to sign in again with testuser@plow.test.")
        let (app, _, _) = app(client)
        let flow = app.stage(.google)
        await flow.checkGoogle()
        #expect(client.enabled == ["google"])
        #expect(flow.google == .notConnected)
        #expect(flow.googleAccounts.signsInAgain("testuser@plow.test"))
        flow.connectGoogle()
        #expect(flow.google == .waiting)
        await waitUntil { client.events != nil }
        #expect(client.signIns == [false])
        client.events?.yield(.enabled(GoogleEnabled(accounts: ["testuser@plow.test"])))
        client.events?.finish()
        await waitUntil { flow.google == .connected }
        #expect(client.enabled == ["google"])
    }

    @Test func connectOpensLatchsLinkAndLatchTurnsGoogleOn() async {
        let client = SignInClient()
        let (app, _, opened) = app(client)
        let flow = app.stage(.google)
        await flow.checkGoogle()
        #expect(flow.google == .notConnected)
        flow.connectGoogle()
        #expect(flow.google == .waiting)
        await waitUntil { client.events != nil }
        #expect(client.signIns == [false])
        client.events?.yield(.link(first))
        await waitUntil { !opened().isEmpty }
        #expect(opened() == [first])
        client.events?.yield(.link(renewed))
        // Open again opens the first link until the renewal is in.
        await waitUntil {
            flow.openGoogleSignInAgain()
            return opened().last == renewed
        }
        // Every open before the renewal was in was the first link, and the
        // renewal itself opened nothing: only open again did.
        #expect(opened().last == renewed)
        #expect(opened().dropLast().allSatisfy { $0 == first })
        // Back in the app while latch waits: its answer stands for a check.
        flow.recheckGoogle()
        client.events?.yield(.enabled(GoogleEnabled(accounts: ["testuser@plow.test"])))
        client.events?.finish()
        await waitUntil { flow.google == .connected }
        #expect(flow.googleAccounts.accounts == ["testuser@plow.test"])
        #expect(client.enabled.isEmpty)
        #expect(!flow.canOpenGoogleSignInAgain)
    }

    @Test func noAccountInTimeIsSaidAndConnectCanStartAgain() async {
        let client = SignInClient()
        let (app, _, _) = app(client)
        let flow = app.stage(.google)
        flow.connectGoogle()
        await waitUntil { client.events != nil }
        client.events?.yield(.link(first))
        await waitUntil { flow.canOpenGoogleSignInAgain }
        client.events?.finish(throwing: timeout)
        await waitUntil { flow.google != .waiting }
        #expect(flow.google == .notConnected)
        #expect(flow.googleWaitEnded == GoogleSignIn.problem(timeout, adding: false))
        #expect(!flow.canOpenGoogleSignInAgain)
        #expect(!flow.primaryEnabled)
        flow.connectGoogle()
        #expect(flow.google == .waiting && flow.googleWaitEnded == nil)
        await waitUntil { client.signIns.count == 2 }
        #expect(client.signIns == [false, false])
    }

    /// Back, Skip for now, or the window closing stop latch's wait; a late
    /// answer changes nothing.
    @Test func leavingGoogleStopsTheWait() async {
        for leave in [{ (flow: Onboarding) in flow.skip() }, { $0.back() }, { $0.stopWork() }] {
            let client = SignInClient()
            let (app, _, _) = app(client)
            let flow = app.stage(.google)
            flow.connectGoogle()
            await waitUntil { client.events != nil }
            client.events?.yield(.link(first))
            await waitUntil { flow.canOpenGoogleSignInAgain }
            let wait = flow.googleTask
            leave(flow)
            await wait?.value
            await waitUntil { client.stopped == 1 }
            #expect(client.stopped == 1)
            #expect(flow.google == .notConnected)
            #expect(!flow.canOpenGoogleSignInAgain)
            // A late answer has nowhere to go: the stream is over.
            let late = client.events?.yield(.enabled(GoogleEnabled(accounts: ["testuser@plow.test"])))
            #expect({ if case .terminated? = late { true } else { false } }())
            #expect(flow.google == .notConnected)
        }
    }

    /// Onboarding opens a canary link, keeps it only for open again, and
    /// after a refusal that quotes it, neither the screen's words nor the
    /// saved progress hold it.
    @Test func onboardingOpensACanaryLinkAndKeepsItNowhere() async throws {
        let client = SignInClient()
        let (app, memory, opened) = app(client)
        let flow = app.stage(.google)
        flow.connectGoogle()
        await waitUntil { client.events != nil }
        client.events?.yield(.link(canary))
        await waitUntil { flow.canOpenGoogleSignInAgain }
        #expect(opened() == [canary])
        client.events?.finish(throwing: canaryRefusal)
        await waitUntil { flow.google != .waiting }
        #expect(flow.google == .failed(Onboarding.couldNotCheck))
        #expect(!flow.canOpenGoogleSignInAgain)
        for shown in [Onboarding.couldNotCheck, flow.primaryTitle] + flow.googleAccounts.accounts {
            #expect(!shown.contains(canaryCode))
        }
        flow.skip()
        let saved = String(decoding: try PropertyListSerialization.data(fromPropertyList: memory.all, format: .xml, options: 0), as: UTF8.self)
        #expect(saved.contains("skipped"))
        #expect(!saved.contains(canaryCode))
    }

    /// Connect Google Account… (no account yet) is a plain sign-in, Add
    /// Another Account… is `--add-account`; the sheet shows while latch
    /// waits and goes once Google is on, and Connections and this Mac's
    /// plugins are read again.
    @Test(arguments: [([String](), false), (["ada@example.com"], true)])
    func connectingWaitsInItsSheet(accounts: [String], adding: Bool) async throws {
        let client = SignInClient()
        client.google = Fixtures.googleConnected(accounts)
        let (app, _, opened) = app(client)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        await app.refreshAccount()
        await app.refreshConnections()
        app.connectGoogle(adding: adding)
        #expect(app.googleSignIn == GoogleSignIn(adding: adding))
        await waitUntil { client.events != nil }
        #expect(client.signIns == [adding])
        client.events?.yield(.link(first))
        await waitUntil { app.canOpenGoogleSignInAgain }
        #expect(opened() == [first])
        let after = accounts + ["ada.work@example.com"]
        client.google = Fixtures.googleConnected(after)
        client.events?.yield(.enabled(GoogleEnabled(accounts: after)))
        client.events?.finish()
        func googleOn() -> Bool {
            (try? app.summaries["studio"]?.plugins?.get())?.plugins.first { $0.name == "google" }?.state == "enabled"
        }
        await waitUntil { app.googleSignIn == nil && app.connections == .google(GoogleAccounts(accounts: after)) && googleOn() }
        #expect(app.googleSignIn == nil)
        #expect(app.connections == .google(GoogleAccounts(accounts: after)))
        #expect(googleOn())
        #expect(!app.canOpenGoogleSignInAgain)
    }

    /// A Connections sign-in that ends with no account, refused (in words
    /// quoting the canary link) or stopped, keeps no link, and the sheet's
    /// words don't carry it.
    @Test(arguments: [true, false])
    func aSignInThatEndsWithoutAnAccountKeepsNoLink(refused: Bool) async {
        let client = SignInClient()
        client.google = Fixtures.googleConnected(["ada@example.com"])
        let (app, _, opened) = app(client)
        await app.refreshConnections()
        app.connectGoogle(adding: true)
        await waitUntil { client.events != nil }
        client.events?.yield(.link(canary))
        await waitUntil { app.canOpenGoogleSignInAgain }
        #expect(opened() == [canary])
        if refused { client.events?.finish(throwing: canaryRefusal) } else { client.events?.finish() }
        await waitUntil { app.googleSignIn?.failure != nil }
        let failure = app.googleSignIn?.failure ?? ""
        #expect(failure == (refused ? GoogleSignIn.problem(canaryRefusal, adding: true) : GoogleSignIn.stopped))
        #expect(!failure.contains(canaryCode))
        #expect(!app.canOpenGoogleSignInAgain)
        app.openGoogleSignInAgain()
        #expect(opened() == [canary])
    }

    /// Cancel stops latch's wait; a refusal stays in the sheet, in the
    /// app's words.
    @Test func theSheetsEndStopsTheWaitAndRefusalsAreSaidPlainly() async {
        let client = SignInClient()
        client.google = Fixtures.googleConnected(["ada@example.com"])
        let (app, _, _) = app(client)
        await app.refreshConnections()
        app.connectGoogle(adding: true)
        await waitUntil { client.events != nil }
        client.events?.yield(.link(first))
        await waitUntil { app.canOpenGoogleSignInAgain }
        app.endGoogleSignIn()
        await waitUntil { client.stopped == 1 }
        #expect(client.stopped == 1)
        #expect(app.googleSignIn == nil && !app.canOpenGoogleSignInAgain)

        app.connectGoogle(adding: true)
        await waitUntil { client.signIns.count == 2 }
        client.events?.yield(.link(first))
        await waitUntil { app.canOpenGoogleSignInAgain }
        client.events?.finish(throwing: timeout)
        await waitUntil { app.googleSignIn?.failure != nil }
        #expect(app.googleSignIn?.failure == GoogleSignIn.problem(timeout, adding: true))
        #expect(!app.canOpenGoogleSignInAgain)
        let unreachable = LatchError.failed(LatchFailure(code: "plow_unreachable", message: "could not reach Plow at http://127.0.0.1:9"))
        for problem in [GoogleSignIn.problem(timeout, adding: true), GoogleSignIn.problem(timeout, adding: false),
                        GoogleSignIn.problem(unreachable, adding: false)] {
            #expect(!problem.contains("latch ") && !problem.contains("http") && !problem.contains("`"))
        }
    }

    /// Back from the browser with Plow out of reach, Connections can't list
    /// the accounts; the sheet's Try Again still starts another sign-in.
    @Test func tryAgainWorksWhenConnectionsCouldntBeRead() async {
        let client = SignInClient()
        client.google = Fixtures.googleConnected(["ada@example.com"])
        let (app, _, _) = app(client)
        await app.refreshConnections()
        app.connectGoogle(adding: true)
        await waitUntil { client.events != nil }
        client.events?.finish(throwing: timeout)
        await waitUntil { app.googleSignIn?.failure != nil }
        client.google = Fixtures.googleUnreachable
        await app.refreshConnections()
        #expect(app.connections == .unknown)
        // Try Again, as the sheet presses it.
        app.connectGoogle(adding: app.googleSignIn?.adding ?? false)
        await waitUntil { client.signIns.count == 2 }
        #expect(client.signIns == [true, true])
        #expect(app.googleSignIn == GoogleSignIn(adding: true))
    }

    /// The lines latch prints with `--events`, as `plugins/connect.rs` and
    /// `main.rs` write them.
    @Test func latchsSignInLinesDecode() throws {
        func event(_ json: String) throws -> GoogleSignInEvent? { try LatchJSON.decoder.decode(GoogleSignInLine.self, from: Data(json.utf8)).event }
        #expect(try event(#"{"type":"hello","schema":1}"#) == nil)
        #expect(try event(#"{"type":"link","url":"\#(first.absoluteString)","expires_at":"2026-10-23T18:02:00Z"}"#) == .link(first))
        #expect(try event(#"{"type":"result","result":{"plugin":"google","state":"enabled","accounts":["owner@example.test"]}}"#)
            == .enabled(GoogleEnabled(accounts: ["owner@example.test"])))
        #expect(try event(#"{"type":"error","error":{"code":"sign_in_timeout","message":"No new Google account was observed before the wait ended."}}"#) == nil)
    }

    /// The live client runs latch with the wait's arguments, and stopping
    /// the iteration ends that latch, not only the app's reading of it.
    @Test func stoppingTheStreamEndsLatch() async throws {
        let folder = FileManager.default.temporaryDirectory.appending(path: "sign-in-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        let script = folder.appending(path: "latch")
        try """
        #!/bin/sh
        echo "$$" > '\(folder.path)/pid'
        printf '%s\\n' "$@" > '\(folder.path)/args'
        echo '{"type":"hello","schema":1}'
        echo '{"type":"link","url":"\(first.absoluteString)","expires_at":"2026-10-23T18:02:00Z"}'
        exec sleep 30
        """.write(to: script, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
        let client = LiveLatchClient(executable: script, stateDir: "/s")
        var seen: [GoogleSignInEvent] = []
        for try await event in client.signInGoogle(addAccount: true) {
            seen.append(event)
            break
        }
        #expect(seen == [.link(first)])
        let args = try String(contentsOf: folder.appending(path: "args"), encoding: .utf8)
        #expect(args == "--state-dir\n/s\nplugins\nenable\ngoogle\n--events\n--add-account\n")
        let pid = try #require(pid_t(try String(contentsOf: folder.appending(path: "pid"), encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)))
        await waitUntil { kill(pid, 0) != 0 }
        #expect(kill(pid, 0) != 0)
    }
}

import Foundation
import Testing
@testable import PlowLatchUI

/// Onboarding's Google section on a real latch logged in to a local Plow,
/// whose Google is its OAuth twin (one account, `testuser@plow.test`),
/// named by the environment: `LATCH_LIVE_LATCH`, a latch built with
/// `--features fake-gog` (it stands in for gog too); `LATCH_LIVE_PLOW`,
/// the API; `LATCH_LIVE_LINQ` and `LATCH_LIVE_GOOGLE`, its Linq and Google
/// OAuth twins. Each run logs a fresh state folder in from a made-up phone
/// number, so a new Plow account with no Google. Built only with
/// `LATCH_LIVE_TESTS=1`; a selected test without its inputs fails.
@MainActor
struct GooglePlowTests {
    nonisolated static let env = ProcessInfo.processInfo.environment

    private static func input(_ name: String, sourceLocation: SourceLocation = #_sourceLocation) throws -> String {
        try #require(env[name], "\(name) is required", sourceLocation: sourceLocation)
    }

    /// Connect Google… signs in the twin's account and lists it; Add
    /// Another Account… is `--add-account`, so the same account signed in
    /// again ends the wait with no new one, said as not added again.
    @Test func connectThenAddTheSameAccount() async throws {
        let latch = URL(filePath: try Self.input("LATCH_LIVE_LATCH"))
        let plow = try Self.input("LATCH_LIVE_PLOW")
        let linq = URL(string: try Self.input("LATCH_LIVE_LINQ"))!
        let google = URL(string: try Self.input("LATCH_LIVE_GOOGLE"))!
        let folder = ContractTests.latchDir.appending(path: "../.dev/tests/gp-\(UUID().uuidString.prefix(4))").standardized
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: folder) }
        // Short: the host's socket goes in it.
        let state = folder.appending(path: "s")
        try await logIn(latch: latch, state: state, plow: plow, linq: linq)
        let pin = try JSONSerialization.jsonObject(with: Data(contentsOf: ContractTests.latchDir.appending(path: "src/google/pin.json"))) as? [String: Any]
        let gog = state.appending(path: "plugins/gog/\(try #require(pin?["version"] as? String))")
        try FileManager.default.createDirectory(at: gog, withIntermediateDirectories: true)
        try FileManager.default.copyItem(at: latch, to: gog.appending(path: "gog"))
        // The app's own client, with latch's wait for an account cut to 20 s.
        let wrapper = folder.appending(path: "latch")
        try "#!/bin/sh\nLATCH_TEST_CONNECT_WITHIN=20 exec '\(latch.path)' \"$@\"\n".write(to: wrapper, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: wrapper.path)
        let client = LiveLatchClient(executable: wrapper, stateDir: state.path)
        let app = AppModel(client: client, store: ProgressStore(memory: ProgressStore.Memory(), stateDir: state.path), still: true)
        var approvals: [Task<Void, Error>] = []
        app.open = { link in approvals.append(Task { try await Self.approve(link, google: google) }) }

        let flow = app.stage(.google)
        await flow.checkGoogle()
        #expect(flow.google == .notConnected)
        #expect(flow.googleAccounts.accounts.isEmpty && flow.googleAccounts.buttonTitle == "Connect Google…")
        flow.connectGoogle()
        await waitUntil(within: .seconds(30)) { flow.google != .waiting }
        #expect(flow.google == .connected)
        #expect(flow.googleAccounts.accounts == ["testuser@plow.test"])
        #expect(flow.googleAccounts.buttonTitle == "Add Another Account…")

        flow.connectGoogle()
        await waitUntil(within: .seconds(40)) { flow.google != .waiting }
        #expect(approvals.count == 2)
        for approval in approvals { try await approval.value }
        #expect(flow.google == .connected)
        #expect(flow.googleWaitEnded == "No new Google account showed up in time. An account that's already connected isn't added again.")
        // Connections reads the same one account off `setup --status`.
        await app.refreshConnections()
        #expect(app.connections == .google(flow.googleAccounts))
    }

    /// `latch login --events`, its code texted in through the Linq twin
    /// from a made-up number.
    private func logIn(latch: URL, state: URL, plow: String, linq: URL) async throws {
        let login = Process()
        login.executableURL = latch
        login.arguments = ["--state-dir", state.path, "login", "--events", "--api-url", plow, "--name", "google-plow-test"]
        let out = Pipe()
        login.standardOutput = out
        login.standardError = FileHandle.nullDevice
        try login.run()
        let phone = "+1555" + String(format: "%07d", Int.random(in: 0..<10_000_000))
        var verified = false
        for try await line in out.fileHandleForReading.bytes.lines {
            let event = try JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any]
            switch event?["type"] as? String {
            case "code":
                var text = URLRequest(url: linq.appending(path: "ui/inbound"))
                text.httpMethod = "POST"
                text.setValue("application/json", forHTTPHeaderField: "Content-Type")
                text.httpBody = try JSONSerialization.data(withJSONObject: ["to_phone": event?["send_to"], "remote_phone": phone, "text": event?["code"]])
                _ = try await URLSession.shared.data(for: text)
            case "verified": verified = true
            default: break
            }
        }
        login.waitUntilExit()
        try #require(verified && login.terminationStatus == 0, "latch login didn't verify")
    }

    /// Signs in as a person would in the browser: the link sends Plow's
    /// sign-in to the Google twin, which approves its account and sends
    /// Plow's callback the code.
    nonisolated private static func approve(_ link: URL, google: URL) async throws {
        let session = URLSession(configuration: .ephemeral, delegate: NoRedirects(), delegateQueue: nil)
        let (_, toGoogle) = try await session.data(from: link)
        let authorize = try #require((toGoogle as? HTTPURLResponse)?.value(forHTTPHeaderField: "Location").flatMap(URLComponents.init(string:)))
        // A form's query: `+` is a space, as between the scopes.
        let query = Dictionary((authorize.queryItems ?? []).map { ($0.name, ($0.value ?? "").replacingOccurrences(of: "+", with: " ")) }) { $1 }
        var approve = URLRequest(url: google.appending(path: "auto-approve"))
        approve.httpMethod = "POST"
        approve.setValue("application/json", forHTTPHeaderField: "Content-Type")
        approve.httpBody = try JSONSerialization.data(withJSONObject: ["client_id", "redirect_uri", "scope", "state"].reduce(into: [String: String]()) { $0[$1] = query[$1] ?? "" })
        let (body, _) = try await session.data(for: approve)
        let redirect = try #require((try JSONSerialization.jsonObject(with: body) as? [String: String])?["redirect_url"].flatMap(URL.init(string:)))
        let (_, callback) = try await session.data(from: redirect)
        let landed = (callback as? HTTPURLResponse)?.value(forHTTPHeaderField: "Location") ?? ""
        #expect(landed.hasSuffix("/oauth/success?provider=gmail"), "Plow's callback sent the browser to \(landed)")
    }
}

/// Hands each redirect back as the answer, so the test reads where it goes.
private final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest) async -> URLRequest? { nil }
}

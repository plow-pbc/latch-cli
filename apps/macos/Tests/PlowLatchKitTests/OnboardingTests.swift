import AppKit
import CoreImage
import Foundation
import Testing
@testable import PlowLatchKit

@Test func loginLinesDecode() throws {
    let events = try Fixtures.loginLines.split(separator: "\n").map {
        try Fixtures.decode(LoginEvent.self, String($0))
    }
    guard case let .code(code) = events[0] else { Issue.record("not a code"); return }
    #expect(code.code == "K7P2Q" && code.sendToDisplay == "+1 555-555-1234")
    #expect(code.smsto == "SMSTO:+15555551234:K7P2Q")
    #expect(code.smsLink.absoluteString == "sms:+15555551234?&body=K7P2Q")
    #expect(events[1] == .waiting)
    #expect(events[2] == .verified(.init(name: "mac-host", key: "k7f3a9c2e1d04b58a6f2c9e3d1b7a4f0", kind: "device")))
    #expect(events[3] == .expired)
    #expect(events[4] == .error(.init(code: "login_not_found", message: "this sign-in code is no longer valid: get a new one")))
}

@Test func qrCodeDecodesToTheCLIsPayload() throws {
    let payload = Fixtures.loginCode.smsto
    let image = try #require(QRCode.image(payload))
    // Scale it up as the screen does, then read it back with Core Image.
    let scaled = CIImage(cgImage: image).transformed(by: CGAffineTransform(scaleX: 8, y: 8))
    let detector = try #require(CIDetector(ofType: CIDetectorTypeQRCode, context: nil, options: nil))
    let found = detector.features(in: scaled).compactMap { ($0 as? CIQRCodeFeature)?.messageString }
    #expect(found == [payload])
}

@Test func plowMarkParsesItsSixOutlines() {
    #expect(PlowMark.subpaths.count == 6)
    let box = PlowMark.mark.boundingRect
    // The mark spans 4…396 on both axes of its 400-unit box.
    #expect([box.minX, box.minY, box.maxX, box.maxY].map { $0.rounded() } == [4, 4, 396, 396])
    #expect(PlowMark.drawn(0).boundingRect.isEmpty || PlowMark.drawn(0).isEmpty)
    #expect(PlowMark.drawn(1).boundingRect == box)
}

@Test func entranceStartsEmptyAndSettlesAtTheOldRest() {
    let start = EntranceFrame.at(0)
    #expect(start.draw == 0 && start.fill == 0 && start.tagline == 0 && start.button == 0 && start.bloomOpacity == 0)
    let rest = EntranceFrame.at(EntranceFrame.settlesAt)
    #expect(rest.draw == 1 && rest.fill == 1 && rest.strokeWidth == 0 && rest.sheenOpacity == 0)
    #expect(abs(rest.tagline - 1) < 0.001 && abs(rest.button - 1) < 0.01)
    #expect(rest.glow.map { ($0 * 100).rounded() } == [22, 16, 12])
    #expect(abs(rest.bloomOpacity - 0.7) < 0.001 && rest.scale == 1)
}

/// Runs a fake onboarding to its end, recording every link the app opens.
@MainActor
private func runFlow(startSharing: Bool) async throws -> (Onboarding, AppModel, [URL]) {
    let fake = FakeLatchClient(.loggedOut, login: .verified(after: .milliseconds(50)))
    let app = AppModel(client: fake, sharing: fake)
    await app.refreshStatus()
    var opened: [URL] = []
    let flow = Onboarding(app: app) { opened.append($0) }
    flow.next()
    #expect(flow.step == .signIn)
    for _ in 0..<100 {
        if case .verified = flow.signIn { break }
        try await Task.sleep(for: .milliseconds(10))
    }
    guard case let .verified(peer) = flow.signIn else { Issue.record("not verified: \(flow.signIn)"); return (flow, app, opened) }
    #expect(peer.name == "mac-host")
    flow.showLinked()
    flow.next()
    #expect(flow.step == .keepRunning)
    flow.addFolders([URL(filePath: "/tmp/work")])
    await flow.keepRunning(startSharing: startSharing)
    #expect(flow.step == .done)
    return (flow, app, opened)
}

@Test @MainActor func onboardingStartSharingEndsServing() async throws {
    let (_, app, opened) = try await runFlow(startSharing: true)
    let status = try #require(app.status.value)
    #expect(status.peer?.name == "mac-host" && status.serve.ready && status.shares == ["/tmp/work"])
    #expect(opened.isEmpty)
}

@Test @MainActor func onboardingNotNowSavesFoldersWithoutServing() async throws {
    let (_, app, opened) = try await runFlow(startSharing: false)
    let status = try #require(app.status.value)
    #expect(status.peer != nil && !status.serve.running && status.shares == ["/tmp/work"])
    #expect(opened.isEmpty)
}

@Test @MainActor func messagesOpensOnlyOnTheClick() async throws {
    let fake = FakeLatchClient(.loggedOut, login: .waiting)
    let app = AppModel(client: fake, sharing: fake)
    var opened: [URL] = []
    let flow = Onboarding(app: app) { opened.append($0) }
    flow.next()
    for _ in 0..<100 where flow.signIn == .starting { try await Task.sleep(for: .milliseconds(10)) }
    #expect(opened.isEmpty)
    flow.openMessages()
    #expect(opened.map(\.absoluteString) == ["sms:+15555551234?&body=K7P2Q"])
    flow.cancel()
}

@Test @MainActor func startSharingNeedsAFolder() async {
    let fake = FakeLatchClient(.loggedOut)
    let flow = Onboarding(app: AppModel(client: fake, sharing: fake)) { _ in }
    flow.stage(.keepRunning)
    await flow.keepRunning(startSharing: true)
    #expect(flow.step == .keepRunning)
}

@Test @MainActor func aLoggedInMacResumesAtLinkedAndStillChoosesFolders() async {
    let fake = FakeLatchClient(.stopped)
    let app = AppModel(client: fake, sharing: fake)
    let flow = Onboarding(app: app) { _ in }
    flow.resumeIfLoggedIn()
    #expect(flow.step == .welcome)  // status not known yet: nothing to resume from
    await app.refreshStatus()
    flow.resumeIfLoggedIn()
    #expect(flow.step == .linked && flow.signIn == .verified(.init(name: "mac-host", key: "k7f3a9c2e1d04b58a6f2c9e3d1b7a4f0", kind: "device")))
    flow.next()
    #expect(flow.step == .keepRunning)
}

@Test @MainActor func aLoggedOutMacStaysAtWelcome() async {
    let fake = FakeLatchClient(.loggedOut)
    let app = AppModel(client: fake, sharing: fake)
    await app.refreshStatus()
    let flow = Onboarding(app: app) { _ in }
    flow.resumeIfLoggedIn()
    #expect(flow.step == .welcome)
}

@Test @MainActor func notNowLeavesNothingRegistered() async {
    let fake = FakeLatchClient(.stopped)
    let app = AppModel(client: fake, sharing: fake)
    await app.refreshStatus()
    await app.startSharing()
    #expect(app.agent == .on && app.ownsHost && app.menuState == .serving)
    let flow = Onboarding(app: app) { _ in }
    flow.stage(.keepRunning, folders: [URL(filePath: "/tmp/work")])
    await flow.keepRunning(startSharing: false)
    #expect(flow.step == .done && app.agent == .off && app.menuState == .stopped)
}

@Test @MainActor func aHostTheAppDidNotStartIsNeverTakenOver() async {
    let fake = FakeLatchClient(.outside)
    let app = AppModel(client: fake, sharing: fake)
    await app.refreshStatus()
    await app.refreshAgent()
    #expect(app.menuState == .outside && !app.ownsHost)
    await app.startSharing()
    #expect(app.agent == .off && app.sharingError != nil && app.menuState == .outside)
    #expect(!app.canStopSharing)
}

@Test func stateFoldersCompareAsFolders() {
    #expect(AppModel.sameFolder(nil, nil))
    #expect(AppModel.sameFolder("/tmp/a/", "/tmp/a"))
    #expect(AppModel.sameFolder("/tmp/a/../a", "/tmp/a"))
    #expect(!AppModel.sameFolder("/tmp/a", nil))
    #expect(!AppModel.sameFolder("/tmp/a", "/tmp/b"))
}

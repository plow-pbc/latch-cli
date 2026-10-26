import AppKit
import Synchronization
import Testing
@testable import PlowLatchUI

/// The main window runs the model's reads and streams while it's open:
/// this Mac's status as it changes, the selected host's activity, each
/// place's reads (cancelled when the place changes); closing it ends them,
/// and a logout hands it to onboarding.
@MainActor
struct MainWindowTests {
    /// A latch whose status and audit streams the test writes to.
    final class LiveClient: LatchClient, @unchecked Sendable {
        let statuses = AsyncThrowingStream<LocalStatus, Error>.makeStream()
        let lines = AsyncThrowingStream<AuditLine, Error>.makeStream()
        nonisolated func statusFollow() -> AsyncThrowingStream<LocalStatus, Error> { statuses.stream }
        nonisolated func auditFollow(host: String?, after: String?) -> AsyncThrowingStream<AuditLine, Error> { lines.stream }
    }

    private func app(_ client: some LatchClient, progress: PlowLatchUI.Progress = PlowLatchUI.Progress(passed: [.welcome: .continued], finished: true)) throws -> AppModel {
        let store = ProgressStore(memory: .init(), stateDir: "/s")
        store.save(progress)
        let app = AppModel(client: client, store: store)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return app
    }

    /// The window, off every display; closed when the test ends.
    private func open(_ app: AppModel) -> MainWindowController {
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700),
                               styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let controller = MainWindowController(app: app, window: window, defaults: nil)
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        return controller
    }

    // The toolbar holds the same items on every place: nothing is added,
    // removed or moved by navigating.
    @Test func theToolbarIsTheSameOnEveryPlace() async throws {
        let app = try app(LiveClient())
        let controller = open(app)
        defer { controller.close() }
        let fixed: [NSToolbarItem.Identifier] = [.toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, .inspectorTrackingSeparator,
                                                 .flexibleSpace, .toggleInspector]
        for place in [Place.host(.home), .host(.activity), .account(.agents), .host(.vault), .account(.connections), .host(.settings)] {
            app.place = place
            try await Task.sleep(for: .milliseconds(20))
            #expect(controller.window?.toolbar?.items.map(\.itemIdentifier) == fixed, "\(place)")
        }
    }

    // The inspector's toggle works only where there's an inspector, Activity
    // and Vault; elsewhere it stays in place, greyed, as Finder's Show
    // Preview does where there's no preview.
    @Test func theInspectorToggleIsGreyedWhereThereIsNoInspector() async throws {
        let app = try app(LiveClient())
        let controller = open(app)
        defer { controller.close() }
        let toolbar = try #require(controller.window?.toolbar)
        for (place, enabled) in [(Place.host(.activity), true), (.host(.home), false), (.host(.vault), true), (.host(.rules), false),
                                 (.host(.settings), false), (.account(.agents), false), (.account(.connections), false)] {
            app.place = place
            try await Task.sleep(for: .milliseconds(20))
            toolbar.validateVisibleItems()
            #expect(toolbar.items.first { $0.itemIdentifier == .toggleInspector }?.isEnabled == enabled, "\(place)")
        }
    }

    @Test func aChangeOfPlaceCancelsTheLastPlacesReads() async throws {
        let client = CountingClient()
        let gate = Gate()
        client.held["rules show"] = gate
        let app = try app(client)
        app.showHost("pi-kitchen")
        let controller = open(app)
        defer { controller.close() }
        // Home on pi-kitchen reads its rules (held), then its vault.
        await waitUntil { client.hostsAsked.contains("pi-kitchen") }
        app.place = .account(.connections)
        await waitUntil { client.calls.contains("devices") }
        gate.open()
        try await Task.sleep(for: .milliseconds(100))
        #expect(app.summaries["pi-kitchen"]?.rules == nil)
        #expect(!client.calls.contains("vault list"))
    }

    @Test func aPlaceLeftAloneFinishesItsReads() async throws {
        let client = CountingClient()
        let app = try app(client)
        app.showHost("pi-kitchen")
        let controller = open(app)
        defer { controller.close() }
        await waitUntil { app.summaries["pi-kitchen"]?.vault != nil }
        #expect(AppModel.held(app.summaries["pi-kitchen"]?.rules))
    }

    @Test func theWindowFollowsStatusAndActivityLive() async throws {
        let client = LiveClient()
        let app = try app(client)
        app.place = .host(.activity)
        let controller = open(app)
        defer { controller.close() }
        let feed = try #require(app.activityFeed)
        await waitUntil { feed.running }
        client.lines.continuation.yield(.caughtUp(cursor: String(format: "%064d", 0), hasOlder: false))
        for n in 1...3 {
            client.lines.continuation.yield(try LatchJSON.decoder.decode(AuditLine.self, from: Data(PagedClient.line(n, op: "exec").utf8)))
        }
        await waitUntil { feed.records.count == 3 }
        // This Mac stops: the window's title says so as latch does.
        let serving = controller.window?.subtitle
        client.statuses.continuation.yield(try Fixtures.decode(Fixtures.stopped))
        await waitUntil { app.status?.thisMachine.hosting.state == "stopped" }
        await waitUntil { controller.window?.subtitle != serving }
    }

    @Test func closingTheWindowEndsItsFeedsAtOnce() async throws {
        let client = LiveClient()
        let app = try app(client)
        app.place = .host(.home)
        let controller = open(app)
        let feed = try #require(app.activityFeed)
        await waitUntil { feed.running }
        controller.close()
        #expect(!feed.running)
        // Nothing the window ran comes back: no feed starts on a change.
        app.place = .host(.activity)
        try await Task.sleep(for: .milliseconds(50))
        #expect(!feed.running)
    }

    @Test func aLogoutHandsTheWindowToOnboarding() async throws {
        let client = LiveClient()
        let app = try app(client)
        let controller = open(app)
        defer { controller.close() }
        // The window's own actions, once its views have set them, recorded
        // instead: a test opens no window.
        await waitUntil { app.openWindow != nil }
        let opened = Mutex<[String]>([]), dismissed = Mutex<[String]>([])
        app.openWindow = { id in opened.withLock { $0.append(id) } }
        app.dismissWindow = { id in dismissed.withLock { $0.append(id) } }
        client.statuses.continuation.yield(try Fixtures.decode(Fixtures.loggedOut))
        await waitUntil { app.onboarding != nil }
        #expect(app.onboarding?.step == .login)
        #expect(opened.withLock { $0 } == [AppModel.onboardingWindow])
        #expect(dismissed.withLock { $0 } == [AppModel.mainWindow])
    }

    // A closed inspector reads nothing: selecting a Vault item with it
    // closed asks latch for nothing until it opens.
    @Test func aClosedVaultInspectorReadsNoItem() async throws {
        let client = VaultClient()
        let app = try app(client)
        app.place = .host(.vault)
        app.inspecting.remove(.vault)
        let controller = open(app)
        defer { controller.close() }
        await waitUntil { AppModel.held(app.summaries["studio"]?.vault) }
        let split = try #require(controller.window?.contentViewController as? NSSplitViewController)
        #expect(split.splitViewItems[2].isCollapsed)
        app.selectedVaultItem = OnHost(host: "studio", id: "v1")
        try await Task.sleep(for: .milliseconds(200))
        #expect(app.vaultShows["studio"]?["v1"] == nil)
        app.inspecting.insert(.vault)
        await waitUntil { !split.splitViewItems[2].isCollapsed }
        await waitUntil { app.vaultShows["studio"]?["v1"] != nil }
    }

    // The place, host and inspectors are the window's to keep: a new window
    // on the same defaults comes back to them.
    @Test func aNewWindowComesBackWhereTheLastWasLeft() async throws {
        let defaults = try #require(UserDefaults(suiteName: "MainWindowTests-\(UUID().uuidString)"))
        let client = LiveClient()
        let first = try app(client)
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1120, height: 700), styleMask: [.titled, .closable],
                               backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let controller = MainWindowController(app: first, window: window, defaults: defaults)
        first.showHost("pi-kitchen")
        first.place = .host(.vault)
        first.inspecting.remove(.vault)
        await waitUntil { defaults.string(forKey: "MainWindowPlace") == "vault" && defaults.stringArray(forKey: "MainWindowInspecting") == ["activity"] }
        controller.close()

        let second = try app(client)
        let again = MainWindowController(app: second, window: Offscreen(contentRect: .zero, styleMask: [.titled], backing: .buffered, defer: false),
                                         defaults: defaults)
        defer { again.close() }
        #expect(second.route == .host(.vault, second.hosts.first { $0.name == "pi-kitchen" }))
        #expect(second.inspecting == [.activity])
    }
}

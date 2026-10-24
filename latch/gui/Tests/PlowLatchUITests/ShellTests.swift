import AppKit
import SwiftUI
import Testing
@testable import PlowLatchUI

/// The window's shell: a sidebar of account pages and hosts, the host
/// workspace's screens, and one inspector whose selection belongs to a host.
@MainActor
struct ShellTests {
    private func app() throws -> AppModel {
        let app = AppModel(client: FakeLatchClient(), store: ProgressStore(memory: .init(), stateDir: "/s"), still: true)
        app.stage(status: try Fixtures.decode(Fixtures.serving))
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts))
        return app
    }

    private func host(_ app: AppModel, _ name: String) throws -> HostEntry { try #require(app.hosts.first { $0.name == name }) }

    @Test func pickingAHostKeepsTheScreenEvenAfterAnAccountPage() throws {
        let app = try app()
        #expect(app.inspecting == HostScreen.inspected)
        app.place = .host(.rules)
        app.showHost("pi-kitchen")
        #expect(app.route == .host(.rules, try host(app, "pi-kitchen")))
        app.place = .host(.vault)
        app.place = .account(.agents)
        #expect(app.route == .account(.agents))
        app.showHost("sidecar")
        #expect(app.route == .host(.vault, try host(app, "sidecar")))
    }

    // Plow stops listing the picked host: the workspace shows a host that
    // is listed, not one that's gone.
    @Test func aHostNoLongerListedFallsBackToOneThatIs() throws {
        let app = try app()
        app.showHost("pi-kitchen")
        app.stage(account: try Fixtures.decode(Fixtures.studioHosts.replacingOccurrences(of: "pi-kitchen", with: "pi-garage")))
        #expect(app.host?.name == "studio")
        #expect(app.route == .host(.home, try host(app, "studio")))
    }

    @Test(arguments: [Place.account(.agents), .account(.connections), .host(.home), .host(.activity), .host(.rules), .host(.vault),
                      .host(.settings)])
    func aPlaceIsKeptAsTextAndComesBack(_ place: Place) {
        #expect(Place(rawValue: place.rawValue) == place)
    }

    @Test func aRestoredWindowComesBackToAPlaceAndHostThatExist() throws {
        let app = try app()
        app.restore(place: "connections", host: "pi-kitchen")
        #expect(app.route == .account(.connections))
        app.showHost("pi-kitchen")
        #expect(app.route == .host(.home, try host(app, "pi-kitchen")))
        app.restore(place: "vault", host: "a-host-since-removed")
        #expect(app.route == .host(.vault, try host(app, "studio")))
        app.restore(place: "a-screen-since-removed", host: nil)
        #expect(app.route == .host(.home, try host(app, "studio")))
    }

    // A record selected on one host is never shown as another's, even
    // where the other has a record with that id.
    @Test func theInspectorShowsARecordOnlyOnItsHost() throws {
        let app = try app()
        let studio = try host(app, "studio"), kitchen = try host(app, "pi-kitchen")
        app.feed(for: studio).stage(Fixtures.studioAudit)
        app.feed(for: kitchen).stage(Fixtures.studioAudit)
        let record = try #require(app.feed(for: studio).records.last { !$0.isBackground })
        app.selectedRecord = OnHost(host: "studio", id: record.id)
        #expect(app.inspectedRecord(on: studio) == record)
        #expect(app.inspectedRecord(on: kitchen) == nil)
        app.activityFilter = record.isRefused ? .allowed : .refused
        #expect(app.inspectedRecord(on: studio) == nil)
    }

    @Test func theInspectorShowsAVaultItemOnlyOnItsHostAndWhileListed() throws {
        let app = try app()
        let studio = try host(app, "studio"), kitchen = try host(app, "pi-kitchen")
        app.stage(summary: HostSummary(vault: .success(try Fixtures.decode(Fixtures.studioVault))), for: "studio")
        app.stage(summary: HostSummary(vault: .success(try Fixtures.decode(Fixtures.remoteVaultItems))), for: "pi-kitchen")
        app.selectedVaultItem = OnHost(host: "studio", id: "v1")
        #expect(app.inspectedVaultItem(on: studio)?.title == "Shop (test)")
        // pi-kitchen has a v1 of its own; it isn't what was selected.
        #expect(app.inspectedVaultItem(on: kitchen) == nil)
        app.vaultSearch = "grocer"
        #expect(app.inspectedVaultItem(on: studio) == nil)
        app.vaultSearch = "shop"
        #expect(app.inspectedVaultItem(on: studio)?.item == "v1")
    }

    // Remove… asked on one host is confirmed only on that host's Vault:
    // another host's item with the same id is never offered.
    @Test func aRemovalAskedOnOneHostIsNeverConfirmedOnAnother() throws {
        let app = try app()
        let studio = try host(app, "studio"), kitchen = try host(app, "pi-kitchen")
        let item = try #require((try Fixtures.decode(Fixtures.remoteVaultItems) as VaultList).items.first)
        app.askToRemove(item, on: kitchen)
        #expect(app.removal(on: studio) == nil)
        #expect(app.removal(on: kitchen) == item)
        app.endRemoval()
        #expect(app.removal(on: kitchen) == nil)
    }

    // The sidebar dragged to its widest at the window's narrowest, with
    // Activity's inspector open: the window lays out, as it once didn't
    // (AppKit's "more Update Constraints in Window passes" exception).
    @Test func aWideSidebarBesideActivityAndItsInspectorLaysOut() async throws {
        let app = try app()
        let studio = try host(app, "studio")
        app.feed(for: studio).stage(Fixtures.studioAudit)
        let record = try #require(app.feed(for: studio).records.last { $0.agent != nil })
        app.openRecord(record, on: studio)
        let window = Offscreen(contentRect: CGRect(x: -4000, y: -4000, width: 1040, height: 600), styleMask: [.titled, .resizable],
                               backing: .buffered, defer: false)
        window.contentView = NSHostingView(rootView: MainView().environment(app))
        window.setFrameOrigin(CGPoint(x: -4000, y: -4000))
        window.orderFrontRegardless()
        defer { window.orderOut(nil) }
        #expect(!NSScreen.screens.contains { $0.frame.intersects(window.frame) })
        func splits(_ view: NSView) -> [NSSplitView] { ((view as? NSSplitView).map { [$0] } ?? []) + view.subviews.flatMap(splits) }
        await waitUntil { splits(window.contentView!).count == 2 }
        // The outermost: the sidebar, then the rest.
        let split = try #require(splits(window.contentView!).last)
        for _ in 0..<30 {
            split.setPosition(split.arrangedSubviews[0].frame.width + 4, ofDividerAt: 0)
            window.layoutIfNeeded()
            window.displayIfNeeded()
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(split.arrangedSubviews[0].frame.width == 320)
    }
}

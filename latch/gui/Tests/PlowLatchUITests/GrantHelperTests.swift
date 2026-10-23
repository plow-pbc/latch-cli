import AppKit
import Testing
@testable import PlowLatchUI

struct GrantHelperTests {
    let workArea = CGRect(x: 0, y: 25, width: 1920, height: 1055)

    @Test func theHelperSitsUnderSettingsBesideItsSidebar() {
        let settings = CGRect(x: 400, y: 200, width: 800, height: 600)
        #expect(SettingsTracker.panelFrame(settings: settings, workArea: workArea, height: 94)
            == CGRect(x: 630, y: 800, width: 570, height: 94))
    }

    @Test func theHelperStaysOnScreen() {
        let atTheEdge = SettingsTracker.panelFrame(settings: CGRect(x: 1500, y: 900, width: 800, height: 500), workArea: workArea, height: 94)
        #expect(atTheEdge.maxX <= workArea.maxX - 12)
        #expect(atTheEdge.maxY <= workArea.maxY - 12)
        let narrow = SettingsTracker.panelFrame(settings: CGRect(x: 100, y: 100, width: 300, height: 500), workArea: workArea, height: 94)
        #expect(narrow.width == 240)
        let small = CGRect(x: 0, y: 0, width: 500, height: 400)
        #expect(SettingsTracker.panelFrame(settings: CGRect(x: 0, y: 0, width: 800, height: 350), workArea: small, height: 94).width == 476)
    }

    @Test func settingsIsItsLargestNormalWindow() {
        let windows = [
            window(pid: 7, CGRect(x: 0, y: 0, width: 300, height: 40), layer: 25),  // a floating panel
            window(pid: 9, CGRect(x: 10, y: 10, width: 900, height: 700)),          // another app
            window(pid: 7, CGRect(x: 100, y: 100, width: 400, height: 200)),        // a sheet, too small
            window(pid: 7, CGRect(x: 200, y: 150, width: 715, height: 600)),
        ]
        #expect(SettingsTracker.frame(of: 7, in: windows) == CGRect(x: 200, y: 150, width: 715, height: 600))
        #expect(SettingsTracker.frame(of: 8, in: windows) == nil)
    }

    @Test func settingsIsInFrontWhenItsWindowLeadsTheList() {
        let settings = window(pid: 7, CGRect(x: 200, y: 150, width: 715, height: 600))
        let other = window(pid: 9, CGRect(x: 10, y: 10, width: 900, height: 700))
        let floating = window(pid: 3, CGRect(x: 0, y: 0, width: 400, height: 94), layer: 3)
        let cursor = window(pid: 4, CGRect(x: 0, y: 0, width: 32, height: 32))
        #expect(SettingsTracker.isFrontmost(7, in: [floating, cursor, settings, other]))
        #expect(!SettingsTracker.isFrontmost(7, in: [other, settings]))
    }

    @Test func theTrackerReportsChangesAndAClosedWindow() {
        var tracker = SettingsTracker()
        let at = CGRect(x: 200, y: 150, width: 715, height: 600)
        let other = window(pid: 9, CGRect(x: 10, y: 10, width: 900, height: 700))
        #expect(tracker.observe([window(pid: 7, at)], pid: 7) == .window(at, front: true))
        #expect(tracker.observe([window(pid: 7, at)], pid: 7) == nil)
        #expect(tracker.observe([other, window(pid: 7, at)], pid: 7) == .window(at, front: false))
        let moved = at.offsetBy(dx: 40, dy: 0)
        #expect(tracker.observe([window(pid: 7, moved)], pid: 7) == .window(moved, front: true))
        // Missing for a moment (a pane swap) is not closed; twelve looks are.
        for _ in 1..<SettingsTracker.missesWhileTracking { #expect(tracker.observe([other], pid: 7) == nil) }
        #expect(tracker.observe([other], pid: 7) == .gone)
    }

    @Test func settingsGetsLongerToShowItsFirstWindow() {
        var tracker = SettingsTracker()
        for _ in 1..<SettingsTracker.missesBeforeFirstWindow { #expect(tracker.observe([], pid: nil) == nil) }
        #expect(tracker.observe([], pid: nil) == .gone)
    }

    @Test func theDragCarriesTheAppItself() {
        let bundle = URL(filePath: "/Applications/Plow Latch.app")
        let item = GrantHelper.dragItem(for: bundle)
        #expect((item.item as? NSURL) == bundle as NSURL)
    }

    @Test func theHelperNamesTheAppAsTheFinderDoes() {
        #expect(GrantHelper(bundle: URL(filePath: "/Users/me/Applications/Plow Latch.app")).name == "Plow Latch")
    }

    private func window(pid: pid_t, _ bounds: CGRect, layer: Int = 0) -> [String: Any] {
        [kCGWindowOwnerPID as String: pid, kCGWindowLayer as String: layer, kCGWindowAlpha as String: 1.0,
         kCGWindowBounds as String: bounds.dictionaryRepresentation]
    }
}

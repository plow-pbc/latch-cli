import Testing
@testable import PlowLatchUI

/// "Keep this Mac awake while plugged in", on Start and in Host Settings.
@MainActor
struct KeepAwakeTests {
    let off = Fixtures.keepAwake(#"{"enabled":false,"held":false,"reason":"disabled"}"#)
    let held = Fixtures.keepAwake(#"{"enabled":true,"held":true,"reason":null}"#)

    /// The switch runs `settings set --keep-awake`, then shows what latch
    /// reads back, not what was clicked.
    @Test func theSwitchShowsWhatLatchReadsBack() async throws {
        let client = ScriptedClient { _ in nil }
        client.status = off
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        await app.refreshStatus()
        #expect(!app.keepAwakeBinding.wrappedValue && app.canSetKeepAwake)
        // Saved, but latch still reads it off: the switch stays off.
        app.keepAwakeBinding.wrappedValue = true
        await waitUntil { client.performed.count == 1 && !app.settingKeepAwake }
        #expect(client.performed == [["settings", "set", "--keep-awake", "on"]])
        #expect(!app.keepAwakeBinding.wrappedValue)
        // Latch reads it on and held: on, and said.
        client.status = held
        app.keepAwakeBinding.wrappedValue = true
        await waitUntil { client.performed.count == 2 && !app.settingKeepAwake }
        #expect(app.keepAwakeBinding.wrappedValue)
        #expect(app.status?.thisMachine.hosting.keepAwake?.said == "Latch is keeping this Mac awake while it's plugged in.")
        client.status = off
        app.keepAwakeBinding.wrappedValue = false
        await waitUntil { client.performed.count == 3 && !app.settingKeepAwake }
        #expect(client.performed.last == ["settings", "set", "--keep-awake", "off"])
        #expect(!app.keepAwakeBinding.wrappedValue)
    }

    /// A refused save is said; the switch keeps latch's word.
    @Test func aFailedSaveIsSaid() async {
        let client = ScriptedClient { _ in nil }
        client.status = off
        client.serveFailure = LatchFailure(code: "state_unwritable", message: "settings.json could not be written")
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        await app.refreshStatus()
        app.keepAwakeBinding.wrappedValue = true
        await waitUntil { app.keepAwakeError != nil }
        #expect(!app.keepAwakeBinding.wrappedValue)
    }

    /// Start follows this Mac's status: unplugged after showing "keeping
    /// awake", its line says the Mac sleeps on battery.
    @Test func startFollowsThePowerSource() async {
        let client = ScriptedClient { _ in nil }
        client.followed = [held, Fixtures.keepAwake(#"{"enabled":true,"held":false,"reason":"on_battery"}"#)]
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        let flow = app.stage(.start)
        let showing = Task { await flow.startAppeared() }
        await waitUntil { app.status?.thisMachine.hosting.keepAwake?.reason == "on_battery" }
        #expect(app.status?.thisMachine.hosting.keepAwake?.said == "On battery, this Mac sleeps as usual.")
        showing.cancel()
    }

    /// Unreadable settings: no switch to set.
    @Test func unreadableSettingsCantBeSet() async {
        let client = ScriptedClient { _ in nil }
        client.status = Fixtures.keepAwake("null")
        let app = AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/Users/me/.latch"), still: true)
        await app.refreshStatus()
        #expect(app.status?.thisMachine.hosting.keepAwake == nil)
        #expect(!app.canSetKeepAwake)
    }

    /// What the host makes of an on switch, in words; off says nothing.
    @Test func eachReasonIsSaid() {
        func said(_ held: Bool?, _ reason: String?, enabled: Bool = true) -> String? {
            LocalStatus.KeepAwake(enabled: enabled, held: held, reason: reason).said
        }
        #expect(said(false, "disabled", enabled: false) == nil)
        #expect(said(nil, nil) == nil)
        #expect(said(false, "not_serving") == "Latch isn't running, so this Mac sleeps as usual.")
        #expect(said(false, "on_battery") == "On battery, this Mac sleeps as usual.")
        #expect(said(false, "refused") == "macOS didn't let Latch keep this Mac awake.")
        #expect(said(false, "unknown") == "Latch can't tell whether this Mac is plugged in, so it sleeps as usual.")
    }
}

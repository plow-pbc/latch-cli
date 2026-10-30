import Foundation
import Synchronization
import Testing
@testable import PlowLatchUI

/// A latch whose `update --check` says a newer build is published, and
/// whose `update` answers as scripted; it counts both.
final class UpdatingClient: LatchClient, @unchecked Sendable {
    private let counted = Mutex<[String]>([])
    var calls: [String] { counted.withLock { $0 } }
    var check = Fixtures.updateAvailable
    var installs: Result<UpdateResult, LatchError> = .success(UpdateResult(updated: true))

    func updateCheck() async throws -> UpdateCheck {
        counted.withLock { $0.append("update --check") }
        return try Fixtures.decode(check)
    }
    func update() async throws -> UpdateResult {
        counted.withLock { $0.append("update") }
        return try installs.get()
    }
}

@MainActor
struct UpdateTests {
    private func app(_ client: UpdatingClient, preferences: UserDefaults? = nil) -> AppModel {
        AppModel(client: client, store: ProgressStore(memory: .init(), stateDir: "/s"), preferences: preferences)
    }

    /// The automatic check runs at once, then not again until the last one
    /// is `updateCheckEvery` old; never while it's turned off.
    @Test func anAutomaticCheckRunsOnlyWhenDue() async {
        let client = UpdatingClient()
        let app = app(client)
        await app.checkForUpdateIfDue()
        #expect(client.calls == ["update --check"])
        #expect(app.updateAvailable && app.availableBuild == 155)
        await app.checkForUpdateIfDue(now: .now.addingTimeInterval(AppModel.updateCheckEvery - 60))
        #expect(client.calls.count == 1)
        await app.checkForUpdateIfDue(now: .now.addingTimeInterval(AppModel.updateCheckEvery + 60))
        #expect(client.calls.count == 2)
        app.checksForUpdates = false
        await app.checkForUpdateIfDue(now: .now.addingTimeInterval(2 * AppModel.updateCheckEvery))
        #expect(client.calls.count == 2)
    }

    /// The choice is kept in the app's preferences, on until turned off.
    @Test func checkingAutomaticallyIsOnUntilTurnedOff() throws {
        try withScratchDefaults { defaults in
            #expect(app(UpdatingClient(), preferences: defaults).checksForUpdates)
            app(UpdatingClient(), preferences: defaults).checksForUpdates = false
            #expect(!app(UpdatingClient(), preferences: defaults).checksForUpdates)
        }
    }

    /// A failed install keeps latch's error for the person and the update
    /// on offer, so a click tries again; the next one that works clears it.
    @Test func aFailedInstallSaysWhyAndCanBeTriedAgain() async {
        let client = UpdatingClient()
        let refused = LatchFailure(code: "bad_signature", message: "SHA256SUMS is not signed with latch's release key; nothing was installed")
        client.installs = .failure(.failed(refused))
        let app = app(client)
        await app.checkForUpdate()
        await app.installUpdate()
        #expect(app.installUpdateError == .failed(refused))
        #expect(app.updateAvailable && !app.installingUpdate && !app.updateInstalled)

        client.installs = .success(UpdateResult(updated: true))
        client.check = Fixtures.updateCheck
        await app.installUpdate()
        #expect(app.installUpdateError == nil)
        // Installed, with this window not quit for it: no longer offered,
        // and it says to open Latch again.
        #expect(app.updateInstalled && !app.updateAvailable)
        #expect(client.calls == ["update --check", "update", "update --check", "update", "update --check"])
    }
}

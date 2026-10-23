import Observation
import SwiftUI

/// The app's state: this Mac's latch as `status --local` last said, and the
/// onboarding flow while it runs. The only owner of the client.
@Observable
final class AppModel {
    let client: any LatchClient
    private(set) var status: LocalStatus?
    private(set) var statusError: String?
    /// The onboarding flow, while the onboarding window is open.
    private(set) var onboarding: Onboarding?
    /// How the app hands a link to the system (Messages, System Settings) or
    /// shows a file in the Finder; only ever on a person's click. A test
    /// build records them instead, so a run never opens anything.
    var open: (URL) -> Void = { NSWorkspace.shared.open($0) }
    var reveal: (URL) -> Void = { NSWorkspace.shared.activateFileViewerSelecting([$0]) }
    /// Turns "Start when you log in" on or off; a test build records it.
    var setLoginItem: (Bool) throws -> Void = { try LoginItem.set($0) }
    /// The windows SwiftUI opens and closes, set by the app's scenes.
    var openWindow: ((String) -> Void)?
    var dismissWindow: ((String) -> Void)?

    static let onboardingWindow = "onboarding"
    static let mainWindow = "main"

    /// A still, for snapshots: screens start nothing of their own.
    let still: Bool

    init(client: any LatchClient, still: Bool = false) {
        self.client = client
        self.still = still
    }

    var progress: Progress { onboarding?.progress ?? Progress.load() }

    /// What's set up, as latch says: whether shared rules are saved, and
    /// each connector's state; nil where latch couldn't say (a connector's
    /// state needs this Mac's host running).
    private(set) var rulesSaved: Bool?
    private(set) var plugins: [String: String]?

    func refreshSetup() async {
        rulesSaved = (try? await client.rulesShow()).map { $0.everyAgent != nil }
        plugins = (try? await client.pluginsList()).map { Dictionary(uniqueKeysWithValues: $0.plugins.map { ($0.name, $0.state) }) }
    }

    /// For snapshots: what latch said is set up.
    func stage(rulesSaved: Bool?, plugins: [String: String]?) {
        self.rulesSaved = rulesSaved
        self.plugins = plugins
    }

    func refreshStatus() async {
        do {
            status = try await client.localStatus()
            statusError = nil
        } catch {
            statusError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    /// The onboarding window opened with no flow (at launch): asks latch
    /// whether this Mac is logged in and resumes where the person left off.
    func resumeOnboarding() async {
        guard onboarding == nil else { return }
        await refreshStatus()
        let progress = Progress.load()
        switch progress.destination(loggedIn: status?.peer != nil) {
        case .main: finishOnboarding()
        case let .onboarding(step): onboarding = Onboarding(app: self, step: step, progress: progress)
        }
    }

    func beginOnboarding(at step: Step, progress: Progress) {
        onboarding = Onboarding(app: self, step: step, progress: progress)
        openWindow?(Self.onboardingWindow)
        dismissWindow?(Self.mainWindow)
    }

    /// "Set up again…" from the main window: every screen, from the first
    /// after Log in (Log in itself when this Mac is logged out).
    func setUpAgain() {
        let progress = Progress.load().settingUpAgain()
        progress.save()
        guard case let .onboarding(step) = progress.destination(loggedIn: status?.peer != nil) else { return }
        beginOnboarding(at: step, progress: progress)
    }

    /// The main window learned this Mac is logged out (a `latch logout`):
    /// onboarding takes over, at Log in.
    func leaveMainIfLoggedOut() {
        guard let status, status.peer == nil, onboarding == nil else { return }
        let progress = Progress.load()
        guard case let .onboarding(step) = progress.destination(loggedIn: false) else { return }
        beginOnboarding(at: step, progress: progress)
    }

    func finishOnboarding() {
        openWindow?(Self.mainWindow)
        dismissWindow?(Self.onboardingWindow)
        onboarding = nil
    }

    /// For snapshots: a flow staged at `step`.
    func stage(_ step: Step, progress: Progress = Progress()) -> Onboarding {
        let flow = Onboarding(app: self, step: step, progress: progress)
        onboarding = flow
        return flow
    }

    /// For snapshots: as `status --local` said.
    func stage(status: LocalStatus?, error: String? = nil) {
        self.status = status
        statusError = error
    }
}

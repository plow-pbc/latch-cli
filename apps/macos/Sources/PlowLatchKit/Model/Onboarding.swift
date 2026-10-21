import Foundation
import Observation

/// The first-run flow: Welcome → Sign in → Linked → Keep Latch running →
/// Done. Owns the steps and the login; the views only draw it.
@MainActor @Observable
public final class Onboarding {
    public enum Step: Int, CaseIterable, Sendable {
        case welcome, signIn, linked, keepRunning, done
    }

    public enum SignIn: Equatable, Sendable {
        case starting
        case waiting(LoginCode)
        case expired(LoginCode)
        case failed(String)
        case verified(LocalStatus.Peer)
    }

    public private(set) var step: Step = .welcome
    public private(set) var signIn: SignIn = .starting
    /// The folders chosen on Keep Latch running; at least one before going on.
    public var folders: [URL] = []
    public private(set) var saving = false
    public private(set) var keepRunningError: String?
    /// Whether the person chose Start Sharing, for Done's wording.
    public private(set) var sharing = false

    private unowned let app: AppModel
    private let open: @MainActor (URL) -> Void
    private var login: Task<Void, Never>?

    /// `open` is how a link reaches the system; it's only ever called from a click.
    public init(app: AppModel, open: @escaping @MainActor (URL) -> Void) {
        self.app = app
        self.open = open
    }

    public func next() {
        switch step {
        case .welcome:
            step = .signIn
            startLogin()
        case .linked:
            step = .keepRunning
        case .signIn, .keepRunning, .done:
            break
        }
    }

    public func back() {
        switch step {
        case .signIn:
            cancel()
            signIn = .starting
            step = .welcome
        case .keepRunning:
            step = .linked
        case .welcome, .linked, .done:
            break
        }
    }

    /// Runs `latch login --events` until it ends or is cancelled.
    public func startLogin() {
        cancel()
        signIn = .starting
        let events = app.client.login()
        login = Task {
            // latch reports how a login ended, then exits non-zero for an
            // expired code or an error; the line already said why.
            var ended = false
            do {
                for try await event in events {
                    if case .code = event {} else if case .waiting = event {} else { ended = true }
                    switch event {
                    case let .code(code):
                        signIn = .waiting(code)
                    case .waiting:
                        break
                    case let .verified(peer):
                        signIn = .verified(peer)
                        await app.refreshStatus()
                    case .expired:
                        if case let .waiting(code) = signIn { signIn = .expired(code) } else { signIn = .failed("The code expired.") }
                    case let .error(failure):
                        signIn = .failed(failure.message)
                    }
                }
            } catch is CancellationError {
            } catch where !ended {
                signIn = .failed((error as? LatchError)?.message ?? error.localizedDescription)
            } catch {}
        }
    }

    /// A Mac that is already logged in (a sign-in that finished before the
    /// app quit, or a login from a terminal) picks up at Linked instead of
    /// signing in again.
    public func resumeIfLoggedIn() {
        guard step == .welcome, let peer = app.status.value?.peer else { return }
        signIn = .verified(peer)
        step = .linked
    }

    /// The window came back while signing in: a login stopped by closing it
    /// starts again.
    public func resumeLogin() {
        guard step == .signIn, login == nil else { return }
        if case .verified = signIn { return }
        startLogin()
    }

    /// Called once the morph into the check has played.
    public func showLinked() {
        if case .verified = signIn, step == .signIn { step = .linked }
    }

    /// The person clicked the QR code: open Messages with the text filled in.
    public func openMessages() {
        switch signIn {
        case let .waiting(code), let .expired(code): open(code.smsLink)
        default: break
        }
    }

    public func addFolders(_ urls: [URL]) {
        for url in urls where !folders.contains(url) { folders.append(url) }
    }

    public func removeFolder(_ url: URL) {
        folders.removeAll { $0 == url }
    }

    /// Saves the folders, starts sharing if asked, and goes on to Done.
    public func keepRunning(startSharing: Bool) async {
        guard !folders.isEmpty, !saving else { return }
        saving = true
        defer { saving = false }
        do {
            _ = try await app.client.setShares(folders.map(\.path))
            // Not now leaves nothing registered, even from an earlier try.
            if startSharing { await app.startSharing() } else { await app.stopSharing() }
            if let error = app.sharingError { throw LatchError.unreachable(error) }
            sharing = startSharing
            keepRunningError = nil
            await app.refreshStatus()
            step = .done
        } catch {
            keepRunningError = (error as? LatchError)?.message ?? error.localizedDescription
        }
    }

    /// Puts the flow at a given point without running it, for previews and
    /// snapshots.
    public func stage(_ step: Step, signIn: SignIn = .starting, folders: [URL] = [],
                      sharing: Bool = false, error: String? = nil) {
        self.step = step
        self.signIn = signIn
        self.folders = folders
        self.sharing = sharing
        keepRunningError = error
    }

    /// Done: the main window takes over and this flow ends.
    public func finish() {
        app.endOnboarding()
        WindowRouter.shared.finishOnboarding()
    }

    /// Stops a login in progress; leaving the window does this.
    public func cancel() {
        login?.cancel()
        login = nil
    }
}

import Foundation
import Observation
import os

/// A value fetched from latch, with when it was last good.
public enum Loadable<Value: Equatable & Sendable>: Equatable, Sendable {
    case loading
    case loaded(Value, at: Date)
    case failed(LatchError, last: Value?)

    public var value: Value? {
        switch self {
        case .loading: nil
        case let .loaded(value, _): value
        case let .failed(_, last): last
        }
    }
}

/// Everything the app shows, and the only thing that talks to the client.
/// Views read it and call its intents; they never call latch.
@MainActor @Observable
public final class AppModel {
    public private(set) var status: Loadable<LocalStatus> = .loading
    /// The host's lifecycle from the status subscription; nil until it reports.
    public private(set) var host: HostLifecycle?
    public private(set) var activity = ActivityFeed()
    /// Why following activity stopped, while it is retrying.
    public private(set) var activityError: LatchError?
    public private(set) var rules: Loadable<RulesShow> = .loading
    /// Whether the app's background-sharing agent is registered.
    public private(set) var agent: SharingAgent = .off
    /// The process launchd runs the agent as, while it runs.
    public private(set) var agentProcess: Int?
    /// Start or Stop Sharing is in progress.
    public private(set) var changingSharing = false
    /// Why the last Start or Stop Sharing failed.
    public private(set) var sharingError: String?
    /// The main window's screen.
    public var screen: Screen = .overview
    /// The first-run flow, while it is under way. It lives here, not in its
    /// window, so closing and reopening the window picks it up again.
    public private(set) var onboarding: Onboarding?

    let client: any LatchClient
    let sharing: any SharingService
    private nonisolated static let log = Logger(subsystem: "co.plow.latch", category: "host")

    public init(client: any LatchClient, sharing: any SharingService = UnavailableSharing()) {
        self.client = client
        self.sharing = sharing
        agent = sharing.agent()
    }

    /// The onboarding under way, or a new one. `open` is how it hands a link
    /// to the system, only ever on a click.
    public func beginOnboarding(open: @escaping @MainActor (URL) -> Void) -> Onboarding {
        if let onboarding { return onboarding }
        let flow = Onboarding(app: self, open: open)
        flow.resumeIfLoggedIn()
        onboarding = flow
        return flow
    }

    /// Onboarding's Done.
    public func endOnboarding() {
        onboarding?.cancel()
        onboarding = nil
    }

    /// What the menu bar icon and status line show.
    public var menuState: MenuState {
        if case .failed = status, status.value == nil { return .unreachable }
        if let status = status.value, status.peer == nil { return .loggedOut }
        if agent == .needsApproval { return .needsApproval }
        if changingSharing { return .starting }
        let lifecycle: MenuState = switch host {
        case .serving: .serving
        case .starting: .starting
        case let .error(message): .error(message)
        case .stopped: .stopped
        case nil:
            if status.value?.serve.ready == true { .serving }
            else if let error = status.value?.lastError { .error(error.message) }
            else { .stopped }
        }
        // A host the app didn't start (a `latch serve` in a terminal) is
        // shown, never taken over.
        if lifecycle == .serving || lifecycle == .starting, !ownsHost { return .outside }
        return lifecycle
    }

    /// Whether the serving host is the app's: latch's serve is the very
    /// process launchd runs the agent as. Registration alone doesn't say.
    public var ownsHost: Bool {
        guard let pid = status.value?.serve.pid else { return false }
        return pid == agentProcess
    }

    /// Whether Start Sharing makes sense now. A registered agent whose host
    /// stopped or failed is started afresh.
    public var canStartSharing: Bool {
        guard !changingSharing, agent == .off || agent == .on else { return false }
        return switch menuState {
        case .stopped, .error: true
        default: false
        }
    }

    /// Whether Stop Sharing makes sense now: there is a registration to remove.
    public var canStopSharing: Bool {
        !changingSharing && (agent == .on || agent == .needsApproval)
    }

    /// Reads whether the agent is registered and which process it runs as;
    /// the person can change Login Items in System Settings at any time.
    public func refreshAgent() async {
        agent = sharing.agent()
        agentProcess = await sharing.agentProcess()
    }

    /// Registers the background host and waits, on latch's lifecycle stream,
    /// until it serves. Anything short of serving unregisters it again, so a
    /// failed start leaves nothing behind.
    public func startSharing() async {
        // The error shown is this attempt's, never an earlier one's.
        sharingError = nil
        await refreshAgent()
        guard canStartSharing else {
            switch menuState {
            case .outside: sharingError = "A latch serve started outside Plow Latch is already sharing this Mac."
            case .needsApproval: sharingError = Self.allowInLoginItems
            default: break
            }
            return
        }
        guard Self.sameFolder(client.stateDir, sharing.stateDir) else {
            sharingError = "Background sharing would serve \(sharing.stateDir ?? "latch's default state folder"), not this app's \(client.stateDir ?? "default state folder")."
            return
        }
        changingSharing = true
        defer { changingSharing = false }
        do {
            if agent == .on { try await sharing.stopSharing() }
            try await sharing.startSharing()
            agent = sharing.agent()
            if agent == .needsApproval {
                // Left registered: Login Items lists it only while it is, and
                // that is where the person allows it.
                sharingError = Self.allowInLoginItems
            } else {
                host = try await waitForHost(starting: true)
            }
        } catch {
            try? await sharing.stopSharing()
            sharingError = Self.latchError(error).message
        }
        await refreshStatus()
        await refreshAgent()
    }

    /// Unregisters the background host and waits, on latch's lifecycle
    /// stream, until it stopped.
    public func stopSharing() async {
        sharingError = nil
        await refreshAgent()
        guard canStopSharing else { return }
        changingSharing = true
        defer { changingSharing = false }
        do {
            try await sharing.stopSharing()
            host = try await waitForHost(starting: false)
        } catch {
            sharingError = Self.latchError(error).message
        }
        await refreshStatus()
        await refreshAgent()
    }

    private static let allowInLoginItems = "Allow Plow Latch in Login Items to share in the background."

    /// How long Start or Stop waits for latch to report the host's new state.
    nonisolated static let sharingTimeout = Duration.seconds(30)

    /// Follows latch's lifecycle stream until the host serves (or, stopping,
    /// stops), for at most `sharingTimeout`.
    private func waitForHost(starting: Bool) async throws -> HostLifecycle {
        let events = client.events(after: nil, lifecycleOnly: true)
        return try await withThrowingTaskGroup(of: HostLifecycle.self) { group in
            group.addTask {
                var began = false
                for try await event in events {
                    let host: HostLifecycle
                    switch event {
                    case let .snapshot(_, now, _), let .host(_, now): host = now
                    case .activity, .reset: continue
                    }
                    Self.log.info("sharing: host \(String(describing: host), privacy: .public)")
                    switch (starting, host) {
                    case (true, .serving), (false, .stopped): return host
                    case (true, .starting): began = true
                    case let (_, .error(message)): throw LatchError.unreachable(message)
                    case (true, .stopped) where began: throw LatchError.unreachable("latch stopped while starting.")
                    default: break
                    }
                }
                throw LatchError.unreachable("latch's lifecycle stream ended.")
            }
            group.addTask {
                try await Task.sleep(for: Self.sharingTimeout)
                throw LatchError.unreachable(starting ? "latch didn't start serving in time." : "latch didn't stop serving in time.")
            }
            defer { group.cancelAll() }
            return try await group.next()!
        }
    }

    private nonisolated static func resolved(_ path: String) -> String {
        URL(filePath: path).standardizedFileURL.resolvingSymlinksInPath().path
    }

    /// Whether two state folders, as given (nil being latch's default), are one.
    nonisolated static func sameFolder(_ a: String?, _ b: String?) -> Bool {
        switch (a, b) {
        case (nil, nil): true
        case let (a?, b?): resolved(a) == resolved(b)
        default: false
        }
    }

    public func refreshStatus() async {
        do {
            status = .loaded(try await client.localStatus(), at: .now)
        } catch {
            status = .failed(Self.latchError(error), last: status.value)
        }
    }

    public func loadRules() async {
        do {
            rules = .loaded(try await client.rules(), at: .now)
        } catch {
            rules = .failed(Self.latchError(error), last: rules.value)
        }
    }

    /// The menu bar's subscription: host lifecycle only, for as long as the
    /// app runs. Each change also refreshes the status, which carries the
    /// shares and last error that go with it.
    public func watchHost() async {
        await reconnecting { [client] in
            for try await event in client.events(after: nil, lifecycleOnly: true) {
                switch event {
                case let .snapshot(_, host, _), let .host(_, host):
                    if self.host != host { Self.log.info("host \(String(describing: host), privacy: .public)") }
                    self.host = host
                    await self.refreshStatus()
                    await self.refreshAgent()
                case .activity, .reset:
                    break
                }
            }
        }
    }

    /// Follows activity while the caller (the Activity screen) is running;
    /// cancelling it ends the CLI process.
    public func followActivity() async {
        await reconnecting { [client] in
            let cursor = self.activity.cursor
            self.activity.connecting(resumed: cursor != nil)
            for try await event in client.events(after: cursor, lifecycleOnly: false) {
                self.activityError = nil
                self.activity.apply(event)
            }
        } failed: { error in
            self.activityError = error
        }
    }

    /// Runs `connect` until cancelled, waiting longer after each failure (up
    /// to a minute) so a missing or broken latch costs almost nothing.
    private func reconnecting(
        _ connect: () async throws -> Void,
        failed: (LatchError) -> Void = { _ in }
    ) async {
        var delay = Duration.seconds(1)
        while !Task.isCancelled {
            do {
                try await connect()
                delay = .seconds(1)
            } catch {
                if Task.isCancelled { return }
                Self.log.error("latch events: \(error, privacy: .public)")
                failed(Self.latchError(error))
                delay = min(delay * 2, .seconds(60))
            }
            try? await Task.sleep(for: delay)
        }
    }

    private static func latchError(_ error: Error) -> LatchError {
        error as? LatchError ?? .unreachable(error.localizedDescription)
    }
}

public enum MenuState: Equatable, Sendable {
    case serving, starting, stopped, loggedOut, unreachable
    /// Serving, but from a `latch serve` the app didn't start.
    case outside
    /// The agent is registered but not allowed in Login Items.
    case needsApproval
    case error(String)
}

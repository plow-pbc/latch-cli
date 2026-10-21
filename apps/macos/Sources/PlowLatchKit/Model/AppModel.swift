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
    /// Start or Stop Sharing is in progress.
    public private(set) var changingSharing = false
    /// Why the last Start or Stop Sharing failed.
    public private(set) var sharingError: String?

    let client: any LatchClient
    let sharing: any SharingService
    private static let log = Logger(subsystem: "co.plow.latch", category: "host")

    public init(client: any LatchClient, sharing: any SharingService = UnavailableSharing()) {
        self.client = client
        self.sharing = sharing
        agent = sharing.agent()
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
        if agent != .on, lifecycle == .serving || lifecycle == .starting { return .outside }
        return lifecycle
    }

    /// Whether Start Sharing makes sense now.
    public var canStartSharing: Bool {
        guard !changingSharing, agent == .off else { return false }
        return switch menuState {
        case .stopped, .error: true
        default: false
        }
    }

    /// Whether Stop Sharing makes sense now: only for the host the app runs.
    public var canStopSharing: Bool {
        !changingSharing && (agent == .on || agent == .needsApproval)
    }

    /// Reads whether the agent is registered; cheap, and the person can
    /// change it in System Settings at any time.
    public func refreshAgent() {
        agent = sharing.agent()
    }

    /// Starts the background host and waits until latch says it serves.
    public func startSharing() async {
        refreshAgent()
        guard canStartSharing else {
            if menuState == .outside {
                sharingError = "A latch serve started outside Plow Latch is already sharing this Mac."
            }
            return
        }
        changingSharing = true
        defer { changingSharing = false }
        do {
            try await sharing.startSharing()
            refreshAgent()
            if agent == .needsApproval { throw LatchError.unreachable("Allow Plow Latch in Login Items to share in the background.") }
            try await waitForServe(running: true)
            sharingError = nil
        } catch {
            refreshAgent()
            sharingError = Self.latchError(error).message
        }
    }

    /// Stops the background host and waits until latch says it stopped.
    public func stopSharing() async {
        refreshAgent()
        guard canStopSharing else { return }
        changingSharing = true
        defer { changingSharing = false }
        do {
            try await sharing.stopSharing()
            refreshAgent()
            try await waitForServe(running: false)
            sharingError = nil
        } catch {
            refreshAgent()
            sharingError = Self.latchError(error).message
        }
    }

    /// Asks latch for its status every half second, for at most 30 s, until
    /// its host is serving (or stopped). Only while Start or Stop runs.
    private func waitForServe(running: Bool) async throws {
        for _ in 0..<60 {
            await refreshStatus()
            if let serve = status.value?.serve {
                if running ? serve.ready : !serve.running { return }
            }
            try await Task.sleep(for: .milliseconds(500))
        }
        let detail = status.value?.lastError?.message
        throw LatchError.unreachable(running
            ? "latch didn't start serving\(detail.map { ": \($0)" } ?? ".")"
            : "latch didn't stop serving.")
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

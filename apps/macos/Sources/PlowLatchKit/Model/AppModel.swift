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

    private let client: any LatchClient
    private static let log = Logger(subsystem: "co.plow.latch", category: "host")

    public init(client: any LatchClient) {
        self.client = client
    }

    /// What the menu bar icon and status line show.
    public var menuState: MenuState {
        if case .failed = status, status.value == nil { return .unreachable }
        if let status = status.value, status.peer == nil { return .loggedOut }
        return switch host {
        case .serving: .serving
        case .starting: .starting
        case let .error(message): .error(message)
        case .stopped: .stopped
        case nil:
            if status.value?.serve.ready == true { .serving }
            else if let error = status.value?.lastError { .error(error.message) }
            else { .stopped }
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
    case error(String)
}

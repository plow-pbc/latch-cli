import Foundation

/// The activity list the app shows, newest first, built from `latch events`.
public struct ActivityFeed: Equatable, Sendable {
    /// The most entries kept in memory; older ones fall off the end.
    public static let limit = 500

    public private(set) var entries: [ActivityEntry] = []
    /// Where a reconnect resumes with `--after`.
    public private(set) var cursor: String?
    private var replaceOnNextSnapshot = true

    public init() {}

    /// A new connection is starting: a snapshot on a fresh connection replaces
    /// the list, one on a resumed connection only adds what is missing.
    public mutating func connecting(resumed: Bool) {
        replaceOnNextSnapshot = !resumed
    }

    public mutating func apply(_ event: LatchEvent) {
        switch event {
        case .reset:
            replaceOnNextSnapshot = true
        case let .snapshot(cursor, _, activity):
            if replaceOnNextSnapshot { entries = [] }
            replaceOnNextSnapshot = false
            insert(activity)
            self.cursor = cursor
        case let .activity(cursor, entry):
            insert([entry])
            self.cursor = cursor
        case let .host(cursor, _):
            self.cursor = cursor
        }
    }

    /// Operations the audit log records that aren't activity people review:
    /// a peer connecting is the start of a request, not a request.
    public static let hiddenOps: Set<String> = ["connect"]

    /// Adds entries given oldest first, dropping any already held.
    private mutating func insert(_ new: [ActivityEntry]) {
        let held = Set(entries.map(\.id))
        let fresh = new.filter { !held.contains($0.id) && !Self.hiddenOps.contains($0.op) }
        entries.insert(contentsOf: fresh.reversed(), at: 0)
        if entries.count > Self.limit { entries.removeLast(entries.count - Self.limit) }
    }
}

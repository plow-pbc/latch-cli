import Foundation
import Observation

/// The screenshots a browser record names, read from its host once the
/// record is opened and held in memory only: closing it lets them go, and
/// none is kept on disk.
@Observable
final class RecordShots {
    enum State: Equatable {
        case loading
        case loaded(Data)
        /// The host no longer keeps it.
        case gone
        case failed(LatchError)
    }

    struct Shot: Identifiable, Equatable {
        /// Its name: an action's number, or `final`.
        let id: String
        var state: State = .loading

        var title: String { id == "final" ? "Last page" : "After action \(id)" }
    }

    let client: any LatchClient
    /// nil for this Mac's own host, else the host `--host` names.
    let host: String?
    let record: AuditRecord.ID
    private(set) var shots: [Shot]

    init(client: any LatchClient, host: String?, record: AuditRecord) {
        self.client = client
        self.host = host
        self.record = record.id
        shots = record.shots.map { Shot(id: $0) }
    }

    /// Reads each one, in the order they were taken.
    func load() async {
        for index in shots.indices where shots[index].state == .loading {
            do {
                shots[index].state = .loaded(try await client.auditShot(record, shot: shots[index].id, host: host))
            } catch let failure as LatchError where failure.code == "no_such_shot" {
                shots[index].state = .gone
            } catch {
                // Leaving the record that asked is no failure to show.
                guard !(error is CancellationError), !Task.isCancelled else { return }
                shots[index].state = .failed(.from(error))
            }
        }
    }

    static let gone = "Kept on the host for 7 days, then removed."
    static let standalone = "Saved on the caller's machine; the host doesn't keep it."
}

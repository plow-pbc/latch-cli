import AppKit
import ImageIO
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
        /// Drawn once from the PNG, at the size a tile shows.
        var thumbnail: NSImage?

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
            guard !Task.isCancelled else { return }
            do {
                let png = try await client.auditShot(record, shot: shots[index].id, host: host)
                shots[index].thumbnail = Self.thumbnail(png)
                shots[index].state = .loaded(png)
            } catch let failure as LatchError where failure.code == "no_such_shot" {
                shots[index].state = .gone
            } catch {
                // Leaving the record that asked is no failure to show.
                guard !(error is CancellationError), !Task.isCancelled else { return }
                shots[index].state = .failed(.from(error))
            }
        }
    }

    private static func thumbnail(_ png: Data) -> NSImage? {
        let options = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceThumbnailMaxPixelSize: 256] as CFDictionary
        guard let source = CGImageSourceCreateWithData(png as CFData, nil),
              let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options) else { return nil }
        return NSImage(cgImage: image, size: .zero)
    }

    static let gone = "Kept on the host for 7 days, then removed."
    static let standalone = "Saved on the caller's machine; the host doesn't keep it."
}

/// The folders a screenshot passes through on its way to the screen: one
/// each, readable only by this user, named for this app's process, and
/// removed once read, or when the app quits.
nonisolated enum ShotFolder {
    private static var prefix: String { "latch-shot-\(ProcessInfo.processInfo.processIdentifier)-" }

    static func make() throws -> URL {
        let folder = FileManager.default.temporaryDirectory.appending(path: prefix + UUID().uuidString)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        return folder
    }

    /// Every one this process left, as it quits.
    static func removeAll() {
        let temporary = FileManager.default.temporaryDirectory
        for name in (try? FileManager.default.contentsOfDirectory(atPath: temporary.path)) ?? [] where name.hasPrefix(prefix) {
            try? FileManager.default.removeItem(at: temporary.appending(path: name))
        }
    }
}

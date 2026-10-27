import Foundation

/// Quick Look shows files, so a request previewed is written to one: only
/// the owner's (0600), in a folder only the owner's (0700) in the per-user
/// temporary directory. Each goes when its preview closes or moves to
/// another request; the folder goes at quit, and at launch for whatever a
/// crash left.
struct QuickLookFiles {
    let dir: URL

    static let shared = QuickLookFiles(dir: FileManager.default.temporaryDirectory.appending(path: "Activity Previews",
                                                                                              directoryHint: .isDirectory))

    /// `text` as a file named `name`, or nil if it couldn't be written.
    func write(_ text: String, name: String) -> URL? {
        let files = FileManager.default
        do {
            try files.createDirectory(at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            // A folder already there keeps its mode: still the owner's only.
            try files.setAttributes([.posixPermissions: 0o700], ofItemAtPath: dir.path)
        } catch {
            return nil
        }
        let url = dir.appending(path: name)
        try? files.removeItem(at: url)
        // Made with its mode, never readable by another even for a moment.
        guard files.createFile(atPath: url.path, contents: Data(text.utf8), attributes: [.posixPermissions: 0o600]) else { return nil }
        return url
    }

    func remove(_ url: URL) {
        try? FileManager.default.removeItem(at: url)
    }

    func removeAll() {
        try? FileManager.default.removeItem(at: dir)
    }
}

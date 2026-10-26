import Foundation
import Testing

/// Runs `body` with a UserDefaults suite of its own, kept in a scratch folder
/// and removed with it afterwards. A suite named like an app's lives in
/// ~/Library/Preferences, and removing its domain still leaves an empty plist
/// there, one more for every run; so the suite is named by a path instead, and
/// the test fails if anything of it is left once the preferences are written.
func withScratchDefaults(sourceLocation: SourceLocation = #_sourceLocation, _ body: (UserDefaults) throws -> Void) throws {
    let folder = FileManager.default.temporaryDirectory.appending(path: "latch-defaults-\(UUID())")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    let suite = folder.appending(path: "defaults").path
    let defaults = try #require(UserDefaults(suiteName: suite), sourceLocation: sourceLocation)
    defer {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: folder)
        CFPreferencesAppSynchronize(suite as CFString)
        let left = [folder, URL.libraryDirectory.appending(path: "Preferences/\(suite).plist")]
            .filter { FileManager.default.fileExists(atPath: $0.path) }
        #expect(left.isEmpty, "the test's defaults are left at \(left)", sourceLocation: sourceLocation)
    }
    try body(defaults)
}

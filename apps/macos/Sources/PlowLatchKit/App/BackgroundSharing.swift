import Foundation
import ServiceManagement

/// The launch agent in this app's bundle that runs the bundled `latch serve`,
/// registered with ServiceManagement. Its plist is named in Info.plist under
/// `LatchServeAgent` and lives in Contents/Library/LaunchAgents.
public struct BackgroundSharing: SharingService {
    public let plistName: String
    public let label: String
    public let stateDir: String?

    /// This app's agent, or nil when its Info.plist names none.
    public static func bundled() -> BackgroundSharing? {
        guard let name = Bundle.main.object(forInfoDictionaryKey: "LatchServeAgent") as? String else { return nil }
        let url = Bundle.main.bundleURL.appending(path: "Contents/Library/LaunchAgents/\(name)")
        guard let plist = NSDictionary(contentsOf: url),
              let label = plist["Label"] as? String,
              let arguments = plist["ProgramArguments"] as? [String]
        else { return nil }
        let stateDir = arguments.firstIndex(of: "--state-dir").flatMap { arguments.indices.contains($0 + 1) ? arguments[$0 + 1] : nil }
        return BackgroundSharing(plistName: name, label: label, stateDir: stateDir)
    }

    private var service: SMAppService { .agent(plistName: plistName) }

    public func agent() -> SharingAgent {
        switch service.status {
        case .enabled: .on
        case .requiresApproval: .needsApproval
        // An agent that was never registered reads as not found, not as
        // not registered; registering finds out if it's really missing.
        case .notFound, .notRegistered: .off
        @unknown default: .off
        }
    }

    /// Asks launchd for the agent's process (`launchctl print`), since
    /// ServiceManagement only says whether it is registered.
    public func agentProcess() async -> Int? {
        let launchctl = Process()
        launchctl.executableURL = URL(filePath: "/bin/launchctl")
        launchctl.arguments = ["print", "gui/\(getuid())/\(label)"]
        let out = Pipe()
        launchctl.standardOutput = out
        launchctl.standardError = FileHandle.nullDevice
        guard (try? launchctl.run()) != nil else { return nil }
        let text = String(decoding: await out.fileHandleForReading.readUntilEnd(), as: UTF8.self)
        // A loaded job that isn't running has no `pid = ` line.
        let line = text.split(separator: "\n").first { $0.trimmingCharacters(in: .whitespaces).hasPrefix("pid = ") }
        return line.flatMap { Int($0.split(separator: "=").last?.trimmingCharacters(in: .whitespaces) ?? "") }
    }

    public func startSharing() async throws {
        do {
            try service.register()
        } catch {
            throw LatchError.unreachable("Couldn't start sharing in the background: \(error.localizedDescription)")
        }
    }

    public func stopSharing() async throws {
        do {
            try await service.unregister()
        } catch {
            throw LatchError.unreachable("Couldn't stop sharing: \(error.localizedDescription)")
        }
    }

    /// Opens System Settings ▸ General ▸ Login Items, where a person allows it.
    @MainActor public static func openLoginItems() {
        SMAppService.openSystemSettingsLoginItems()
    }
}

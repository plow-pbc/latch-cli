import Foundation
import ServiceManagement

/// The launch agent in this app's bundle that runs the bundled `latch serve`,
/// registered with ServiceManagement. Its plist is named in Info.plist under
/// `LatchServeAgent` and lives in Contents/Library/LaunchAgents.
public struct BackgroundSharing: SharingService {
    public let plistName: String

    public init(plistName: String) {
        self.plistName = plistName
    }

    /// This app's agent, or nil when its Info.plist names none.
    public static func bundled() -> BackgroundSharing? {
        (Bundle.main.object(forInfoDictionaryKey: "LatchServeAgent") as? String).map(BackgroundSharing.init)
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

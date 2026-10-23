// swift-tools-version:6.2
// PlowLatchUI: the Mac app's window. Plow Latch.app's executable is latch
// itself; opened by LaunchServices, latch loads this library from
// Contents/Frameworks and hands it the main thread (src/gui.rs).
import PackageDescription

let isolated: [SwiftSetting] = [.defaultIsolation(MainActor.self)]

let package = Package(
    name: "PlowLatchUI",
    platforms: [.macOS(.v26)],
    products: [.library(name: "PlowLatchUI", type: .dynamic, targets: ["PlowLatchUI"])],
    targets: [
        .target(name: "PlowLatchUI", swiftSettings: isolated),
        // Renders every onboarding screen and state, light and dark, on the
        // sample latch to PNGs: `swift run Snapshots OUTPUT_DIR`.
        .executableTarget(name: "Snapshots", dependencies: ["PlowLatchUI"], swiftSettings: isolated),
        .testTarget(name: "PlowLatchUITests", dependencies: ["PlowLatchUI"], swiftSettings: isolated),
    ]
)

// swift-tools-version:6.2
// PlowLatchUI: the Mac app's window. Plow Latch.app's executable is latch
// itself; opened by LaunchServices, latch loads this library from
// Contents/Frameworks and hands it the main thread (src/gui.rs).
import Foundation
import PackageDescription

let isolated: [SwiftSetting] = [.defaultIsolation(MainActor.self)]
// The live tests need a real host and its owner's machines; they're built
// only when asked for: `LATCH_LIVE_TESTS=1 swift test --filter LiveTests`.
let live = ProcessInfo.processInfo.environment["LATCH_LIVE_TESTS"] != nil

let package = Package(
    name: "PlowLatchUI",
    platforms: [.macOS(.v26)],
    products: [.library(name: "PlowLatchUI", type: .dynamic, targets: ["PlowLatchUI"])],
    targets: [
        .target(name: "PlowLatchUI", swiftSettings: isolated),
        // Renders every onboarding screen and state, light and dark, on the
        // sample latch to PNGs: `swift run Snapshots OUTPUT_DIR`.
        .executableTarget(name: "Snapshots", dependencies: ["PlowLatchUI"], swiftSettings: isolated),
        // `swift test` runs the model tests and `ContractTests`, which run
        // the real debug latch; `swift test --skip ContractTests` needs no
        // Rust build.
        .testTarget(name: "PlowLatchUITests", dependencies: ["PlowLatchUI"], exclude: live ? [] : ["Live"],
                    swiftSettings: isolated),
    ]
)

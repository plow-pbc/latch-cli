// swift-tools-version:6.2
// PlowLatchUI: the Mac app's window. Plow Latch.app's executable is latch
// itself; opened by LaunchServices, latch loads this library from
// Contents/Frameworks and hands it the main thread (src/gui.rs).
import PackageDescription

let package = Package(
    name: "PlowLatchUI",
    platforms: [.macOS(.v26)],
    products: [.library(name: "PlowLatchUI", type: .dynamic, targets: ["PlowLatchUI"])],
    targets: [
        .target(name: "PlowLatchUI", swiftSettings: [.defaultIsolation(MainActor.self)])
    ]
)

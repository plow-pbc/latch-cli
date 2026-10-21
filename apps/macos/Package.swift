// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "PlowLatch",
    platforms: [.macOS("26.0")],
    targets: [
        .executableTarget(name: "PlowLatch", dependencies: ["PlowLatchKit"]),
        .target(name: "PlowLatchKit"),
        // Renders every screen and state with the fake client to PNGs.
        .executableTarget(name: "Snapshots", dependencies: ["PlowLatchKit"]),
        .testTarget(name: "PlowLatchKitTests", dependencies: ["PlowLatchKit"]),
    ]
)

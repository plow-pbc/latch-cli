// swift-tools-version:6.0
// latch-messages: reads the owner's Messages database (chat.db) and address
// book, read-only. A port of plow-messages, with imsg's typedstream parser in
// place of NSUnarchiver and imsg's AddressBook reader for names (see NOTICE).
import Foundation
import PackageDescription

// PhoneNumberKit's metadata, linked into the binary itself (section
// __DATA,__phonemeta) so the reader is one file with no resource bundle.
let metadata = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    .appendingPathComponent(".build/checkouts/PhoneNumberKit/Sources/PhoneNumberKit/Resources/PhoneNumberMetadata.json")
    .path
let embedMetadata: [LinkerSetting] = [
    .linkedLibrary("sqlite3"),
    .unsafeFlags(["-Xlinker", "-sectcreate", "-Xlinker", "__DATA", "-Xlinker", "__phonemeta", "-Xlinker", metadata]),
]

let package = Package(
    name: "latch-messages",
    platforms: [.macOS(.v13)],
    products: [.executable(name: "latch-messages", targets: ["latch-messages"])],
    dependencies: [
        .package(url: "https://github.com/PhoneNumberKit/PhoneNumberKit.git", exact: "5.0.12")
    ],
    targets: [
        // The body parser imports nothing, Foundation included, so it cannot
        // instantiate a class from a message.
        .target(name: "TypedStream"),
        .target(
            name: "MessagesReader",
            dependencies: ["TypedStream", .product(name: "PhoneNumberKit", package: "PhoneNumberKit")]
        ),
        .executableTarget(name: "latch-messages", dependencies: ["MessagesReader"], linkerSettings: embedMetadata),
        .testTarget(
            name: "ReaderTests",
            dependencies: ["MessagesReader", "TypedStream"],
            resources: [.copy("Fixtures")],
            linkerSettings: embedMetadata
        ),
    ]
)

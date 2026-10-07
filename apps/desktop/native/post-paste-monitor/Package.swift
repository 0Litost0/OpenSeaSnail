// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SeaSnailPostPasteMonitor",
    platforms: [.macOS(.v13)],
    dependencies: [
        .package(url: "https://github.com/apple/swift-protobuf.git", exact: "1.33.3")
    ],
    targets: [
        .executableTarget(
            name: "seasnail-post-paste-monitor",
            dependencies: [.product(name: "SwiftProtobuf", package: "swift-protobuf")]
        )
    ]
)

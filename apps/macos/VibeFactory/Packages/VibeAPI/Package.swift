// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "VibeAPI",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "VibeAPI", targets: ["VibeAPI"]),
    ],
    targets: [
        .target(name: "VibeAPI"),
        .testTarget(
            name: "VibeAPITests",
            dependencies: ["VibeAPI"],
            resources: [.copy("Fixtures")]
        ),
    ]
)

// swift-tools-version: 6.0
import PackageDescription

// The Swift side of the Local Network probe: two C functions, no dependencies.
// The build script of the Rust crate builds this package; the name LocalNetwork is the library name it looks for.
let package = Package(
    name: "LocalNetwork",
    // The deployment floor of the code. build.rs states the same major version (MACOS_FLOOR) and a test pins that.
    // Whether the system has the privacy setting at all (macOS 15) is asked at run time.
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "LocalNetwork", type: .static, targets: ["LocalNetwork"])
    ],
    targets: [
        .target(
            name: "LocalNetwork",
            path: "Sources/LocalNetwork"
        )
    ]
)

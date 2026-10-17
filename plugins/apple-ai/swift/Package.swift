// swift-tools-version: 6.0
import PackageDescription

// The Swift side of the Apple on-device model bridge: two C functions, no dependencies.
// The build script of the Rust crate builds this package; the name AppleAI is the library name it looks for.
let package = Package(
    name: "AppleAI",
    // The deployment floor of the code. build.rs states the same major version (MACOS_FLOOR) and a test pins that.
    // The framework itself is guarded at run time with #available(macOS 26.0, *).
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "AppleAI", type: .static, targets: ["AppleAI"])
    ],
    targets: [
        .target(
            name: "AppleAI",
            path: "Sources/AppleAI",
            swiftSettings: [
                // Without this every object file asks the linker for a strong -framework FoundationModels, and an
                // app that contains the bridge would not start where the framework is missing. The final link gets
                // -weak_framework from build.rs instead. Unsafe flags are accepted because `swift build` runs in this
                // directory, so this is a root package.
                .unsafeFlags(["-Xfrontend", "-disable-autolink-framework", "-Xfrontend", "FoundationModels"])
            ]
        )
    ]
)

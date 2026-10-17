//! Builds the Swift package of the bridge and decides how Apple's FoundationModels framework is linked.
//!
//! The framework is linked weakly: an app that contains the bridge must still start on a Mac whose operating system
//! has no such framework. Two things make that so, and both are needed:
//!
//! 1. The Swift package is compiled with the autolink entry for the framework disabled (see `swift/Package.swift`),
//!    otherwise every object file asks the linker for a strong `-framework FoundationModels`.
//! 2. The final link gets `-weak_framework FoundationModels`. `rustc -l` has no weak modifier, so the argument goes
//!    through `cargo:rustc-link-arg`. Link arguments of a library crate do not reach the crates that depend on it, so
//!    this script also prints `cargo:weak_framework=...` metadata, which a dependent's build script reads and repeats.
//!
//! The Swift runtime libraries are found through the run path `/usr/lib/swift`, where the operating system keeps them;
//! that argument is repeated for dependents in the same way (`cargo:swift_rpath`).
//!
//! Off macOS nothing happens here: the crate is plain Rust and no Swift tool is needed.

use std::env;

/// The deployment floor of the Swift code. `swift/Package.swift` states the same major version (a test pins that).
/// It is not 26, the version of the framework: compiling with a deployment target of 26 would let the compiler call
/// macOS 26 symbols without a guard, and the app must start where Apple's model does not exist.
#[allow(dead_code)]
const MACOS_FLOOR: &str = "13.0";
/// The product, library and target name of the Swift package; `libAppleAI.a` is found by it.
#[allow(dead_code)]
const PACKAGE: &str = "AppleAI";
/// The directory of the Swift package, relative to the crate.
#[allow(dead_code)]
const SWIFT_DIR: &str = "swift";
/// Where the operating system keeps the Swift runtime libraries.
#[allow(dead_code)]
const SWIFT_RPATH: &str = "/usr/lib/swift";
/// The framework that is linked weakly.
#[allow(dead_code)]
const FRAMEWORK: &str = "FoundationModels";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=swift/Package.swift");
    println!("cargo:rerun-if-changed=swift/Sources");
    println!("cargo:rustc-check-cfg=cfg(apple_ai_sdk_foundation_models)");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    build_for_macos();
}

#[cfg(target_os = "macos")]
fn build_for_macos() {
    swift_rs::SwiftLinker::new(MACOS_FLOOR).with_package(PACKAGE, SWIFT_DIR).link();
    link_search_for_current_swiftpm();

    // The Swift runtime libraries (concurrency, core) are named @rpath/... by the linker. The operating system provides
    // them in /usr/lib/swift (back to macOS 12), so that directory is the run path. Like the weak framework argument,
    // it is repeated for dependents through metadata, because link arguments of a library crate do not reach them.
    println!("cargo:swift_rpath={SWIFT_RPATH}");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{SWIFT_RPATH}");

    if sdk_has_framework() {
        println!("cargo:rustc-cfg=apple_ai_sdk_foundation_models");
        // Metadata for a crate that depends on this one (see the module documentation).
        println!("cargo:weak_framework={FRAMEWORK}");
        // For this crate's own tests and examples.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,{FRAMEWORK}");
    } else {
        println!(
            "cargo:warning=the macOS SDK has no {FRAMEWORK} framework: the Apple model probe reports sdk_lacks_framework"
        );
    }
}

/// `swift-rs` looks for the archive in `<arch>-apple-macosx/<configuration>`, the layout of SwiftPM's classic build
/// system. SwiftPM of Swift 6.4 builds with the newer system, which puts the archive in `out/Products/<Configuration>`
/// (and keeps a `<configuration>` link to it). Every directory below the package's build path that really holds the
/// archive is added to the link search path, so that either layout links.
#[cfg(target_os = "macos")]
fn link_search_for_current_swiftpm() {
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let build_path = std::path::Path::new(&out).join("swift-rs").join(PACKAGE);
    let configuration = if env::var("DEBUG").as_deref() == Ok("true") { "debug" } else { "release" };
    let arch = if env::consts::ARCH == "aarch64" { "arm64" } else { env::consts::ARCH };
    let capitalised = if configuration == "debug" { "Debug" } else { "Release" };
    let archive = format!("lib{PACKAGE}.a");
    let candidates = [
        build_path.join(format!("{arch}-apple-macosx")).join(configuration),
        build_path.join(configuration),
        build_path.join("out").join("Products").join(capitalised),
    ];
    let mut found = false;
    for dir in candidates {
        if dir.join(&archive).is_file() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            found = true;
        }
    }
    assert!(found, "the Swift build produced no {archive} below {}", build_path.display());
}

#[cfg(not(target_os = "macos"))]
fn build_for_macos() {
    panic!("building the Apple bridge for a macOS target needs a Mac with Swift: build it on macOS");
}

/// Whether the macOS SDK that Swift compiles against contains the framework.
#[cfg(target_os = "macos")]
fn sdk_has_framework() -> bool {
    let output = std::process::Command::new("xcrun").args(["--sdk", "macosx", "--show-sdk-path"]).output();
    let Ok(output) = output else { return false };
    if !output.status.success() {
        return false;
    }
    let sdk = String::from_utf8_lossy(&output.stdout);
    std::path::Path::new(sdk.trim()).join("System/Library/Frameworks").join(format!("{FRAMEWORK}.framework")).is_dir()
}

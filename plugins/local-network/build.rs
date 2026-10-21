//! Builds the Swift package of the Local Network probe.
//!
//! The package uses the Network framework only, which the operating system has had since macOS 10.14, so nothing is
//! linked weakly. The Swift runtime libraries are found through the run path `/usr/lib/swift`, where the operating system
//! keeps them. The link arguments of a library crate do not reach the crates that depend on it; the app repeats the run
//! path it already takes from the apple-ai crate, so this script prints no metadata for dependents.
//!
//! Off macOS nothing happens here: the crate is plain Rust and no Swift tool is needed.

use std::env;

/// The deployment floor of the Swift code. `swift/Package.swift` states the same major version (a test pins that).
/// The check for macOS 15 is made at run time (`hw_ln_applies`).
#[allow(dead_code)]
const MACOS_FLOOR: &str = "13.0";
/// The product, library and target name of the Swift package; `libLocalNetwork.a` is found by it.
#[allow(dead_code)]
const PACKAGE: &str = "LocalNetwork";
/// The directory of the Swift package, relative to the crate.
#[allow(dead_code)]
const SWIFT_DIR: &str = "swift";
/// Where the operating system keeps the Swift runtime libraries.
#[allow(dead_code)]
const SWIFT_RPATH: &str = "/usr/lib/swift";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=swift/Package.swift");
    println!("cargo:rerun-if-changed=swift/Sources");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    build_for_macos();
}

#[cfg(target_os = "macos")]
fn build_for_macos() {
    swift_rs::SwiftLinker::new(MACOS_FLOOR).with_package(PACKAGE, SWIFT_DIR).link();
    link_search_for_current_swiftpm();
    // For this crate's own tests and examples; the app has its own copy of this argument.
    println!("cargo:rustc-link-arg=-Wl,-rpath,{SWIFT_RPATH}");
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
    panic!("building the Local Network probe for a macOS target needs a Mac with Swift: build it on macOS");
}

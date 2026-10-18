//! How `wsh` is wired into the packages: a Tauri configuration overlay that only the release scripts pass.
//!
//! The overlay names the sidecar (`bundle.externalBin`) and the two static Linux builds (`bundle.resources`). The
//! default configurations never name either, because `tauri-build` requires a named sidecar file at compile time and
//! that would make `cargo test` and `cargo run` need a built binary. This file reads the configurations as text and
//! JSON from the workspace root; it needs no Zig and builds nothing.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn json(rel: &str) -> Value {
    let path = root().join(rel);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

#[test]
fn the_default_configuration_names_no_sidecar() {
    for file in
        ["app/src-tauri/tauri.conf.json", "app/src-tauri/tauri.linux.conf.json", "app/src-tauri/tauri.windows.conf.json"]
    {
        let conf = json(file);
        let bundle = conf.get("bundle").and_then(Value::as_object);
        for key in ["externalBin", "resources"] {
            assert!(
                bundle.is_none_or(|b| !b.contains_key(key)),
                "{file} names bundle.{key}: only the overlay may, or cargo test needs a built binary"
            );
        }
    }
}

#[test]
fn the_overlay_names_the_wsh_sidecar_and_the_two_linux_builds() {
    let conf = json("app/src-tauri/tauri.wsh.conf.json");
    let bundle = conf.get("bundle").and_then(Value::as_object).expect("the overlay has a bundle object");
    let external: Vec<&str> = bundle
        .get("externalBin")
        .and_then(Value::as_array)
        .expect("bundle.externalBin is a list")
        .iter()
        .map(|v| v.as_str().expect("an externalBin entry is a string"))
        .collect();
    assert_eq!(external, ["binaries/wsh"], "the host sidecar, named without a target triple");

    let resources = bundle.get("resources").and_then(Value::as_object).expect("bundle.resources is a map");
    let mut pairs: Vec<(&str, &str)> =
        resources.iter().map(|(k, v)| (k.as_str(), v.as_str().expect("a resource target is a string"))).collect();
    pairs.sort_unstable();
    assert_eq!(
        pairs,
        [
            ("binaries/wsh-linux-aarch64", "wsh/linux-aarch64"),
            ("binaries/wsh-linux-x86_64", "wsh/linux-x86_64")
        ],
        "the two static Linux builds and no other source"
    );
}

#[test]
fn the_binaries_folder_is_not_committed() {
    let ignore = fs::read_to_string(root().join(".gitignore")).expect("the repository has a .gitignore");
    assert!(
        ignore.lines().any(|l| l.trim() == "/app/src-tauri/binaries/"),
        "the sidecar binaries are build products: /app/src-tauri/binaries/ must be ignored"
    );
}

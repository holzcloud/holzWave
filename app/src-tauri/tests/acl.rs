//! The deny-by-default guard of the app shell (T-02-55): the commands the page may call are listed three times, in the
//! invoke handler of `lib.rs`, in the app manifest of `build.rs` and in the permissions of the capability, and this
//! test fails when the three lists disagree. It reads the three files as text and scans them with plain string
//! functions, so adding a command to one list and forgetting another is caught at `cargo test`.

use std::collections::BTreeSet;

const LIB_RS: &str = include_str!("../src/lib.rs");
const BUILD_RS: &str = include_str!("../build.rs");
const CAPABILITY: &str = include_str!("../capabilities/default.json");

/// Permissions of the main window that are not commands of the app: the defaults of the core plugin set and the one
/// permission the own-drawn title bar needs (`data-tauri-drag-region`; `core:default` does not contain it).
const FIXED_PERMISSIONS: [&str; 2] = ["core:default", "core:window:allow-start-dragging"];

/// The text between `open` and the first `]` after it, with line comments removed.
fn list_after(source: &str, open: &str) -> Option<String> {
    let start = source.find(open)? + open.len();
    let end = start + source[start..].find(']')?;
    let body: Vec<&str> = source[start..end].lines().map(|line| line.split("//").next().unwrap_or("")).collect();
    Some(body.join("\n"))
}

/// The command names inside `generate_handler![ ... ]` (`commands::name, ...`).
fn handler_commands(lib_rs: &str) -> Result<BTreeSet<String>, String> {
    let body = list_after(lib_rs, "generate_handler![").ok_or("lib.rs has no generate_handler![ ... ] list")?;
    let mut names = BTreeSet::new();
    for entry in body.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let name = entry.rsplit("::").next().unwrap_or(entry).trim();
        if !names.insert(name.to_owned()) {
            return Err(format!("lib.rs lists the command {name} twice"));
        }
    }
    Ok(names)
}

/// The quoted names inside the manifest list of `build.rs` (`.commands(&["name", ...])`).
fn manifest_commands(build_rs: &str) -> Result<BTreeSet<String>, String> {
    let body = list_after(build_rs, ".commands(&[").ok_or("build.rs has no .commands(&[ ... ]) manifest list")?;
    let mut names = BTreeSet::new();
    for (i, part) in body.split('"').enumerate() {
        // Between the quotes: odd positions.
        if i % 2 == 1 && !names.insert(part.to_owned()) {
            return Err(format!("build.rs lists the command {part} twice"));
        }
    }
    Ok(names)
}

/// The permission a command needs: `allow-` plus the name with underscores turned into dashes.
fn permission_of(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// Every way the three lists disagree, each message naming the command or permission.
fn parity_problems(handler: &BTreeSet<String>, manifest: &BTreeSet<String>, permissions: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for name in handler.difference(manifest) {
        problems.push(format!("command {name} is in the invoke handler but not in the build.rs manifest"));
    }
    for name in manifest.difference(handler) {
        problems.push(format!("command {name} is in the build.rs manifest but not in the invoke handler"));
    }
    let mut wanted: BTreeSet<String> = FIXED_PERMISSIONS.iter().map(|p| (*p).to_owned()).collect();
    wanted.extend(manifest.iter().map(|name| permission_of(name)));
    for permission in wanted.difference(permissions) {
        problems.push(format!("permission {permission} is missing from capabilities/default.json"));
    }
    for permission in permissions.difference(&wanted) {
        problems.push(format!("permission {permission} in capabilities/default.json belongs to no manifest command"));
    }
    problems
}

fn capability() -> serde_json::Value {
    serde_json::from_str(CAPABILITY).expect("capabilities/default.json is JSON")
}

/// The `permissions` array of the capability; every entry must be a plain identifier string (an object would carry
/// a scope that this test does not model, so it is refused).
fn capability_permissions() -> Result<BTreeSet<String>, String> {
    let value = capability();
    let list = value.get("permissions").and_then(|p| p.as_array()).ok_or("the capability has no permissions array")?;
    let mut out = BTreeSet::new();
    for entry in list {
        let name = entry.as_str().ok_or_else(|| format!("permission entry {entry} is not a plain identifier"))?;
        if !out.insert(name.to_owned()) {
            return Err(format!("the capability lists {name} twice"));
        }
    }
    Ok(out)
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn handler_manifest_and_capability_list_the_same_commands() {
    let handler = handler_commands(LIB_RS).expect("handler list");
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    let permissions = capability_permissions().expect("capability permissions");
    assert!(!handler.is_empty(), "the invoke handler lists no command: the scan found nothing");
    let problems = parity_problems(&handler, &manifest, &permissions);
    assert!(problems.is_empty(), "the allow-list is out of step:\n  {}", problems.join("\n  "));
}

#[test]
fn the_capability_targets_only_the_main_window_and_no_remote_url() {
    let value = capability();
    let windows: Vec<&str> = value
        .get("windows")
        .and_then(|w| w.as_array())
        .expect("capability has a windows array")
        .iter()
        .map(|w| w.as_str().expect("a window label is a string"))
        .collect();
    assert_eq!(windows, ["main"], "the capability must grant the window labelled main and no other");
    for key in ["remote", "webviews", "platforms"] {
        assert!(value.get(key).is_none(), "the capability has a {key} entry; it must grant the main window only");
    }
    assert!(!CAPABILITY.contains("http:"), "the capability names a remote URL");
    assert!(!CAPABILITY.contains("https:"), "the capability names a remote URL");
}

#[test]
fn there_is_no_raw_input_command() {
    let handler = handler_commands(LIB_RS).expect("handler list");
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    for list in [&handler, &manifest] {
        assert!(!list.contains("input"), "the raw input command is gone and must not come back");
    }
    assert!(
        !capability_permissions().expect("capability permissions").contains("allow-input"),
        "the capability grants allow-input"
    );
}

#[test]
fn window_events_are_not_commands() {
    // note_window and placement are called by the Rust window handlers; granting them to the page would let a
    // script move the saved window.
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    for name in ["note_window", "placement", "shutdown"] {
        assert!(!manifest.contains(name), "{name} is an AppCore method, not a page command");
    }
}

#[test]
fn a_command_added_to_one_list_only_fails_with_its_name() {
    let handler = set(&["get_layout", "paste", "brand_new"]);
    let manifest = set(&["get_layout", "paste"]);
    let permissions = set(&["core:default", "core:window:allow-start-dragging", "allow-get-layout", "allow-paste"]);
    let problems = parity_problems(&handler, &manifest, &permissions);
    assert_eq!(problems, ["command brand_new is in the invoke handler but not in the build.rs manifest"]);

    // In the manifest and the handler but never granted.
    let handler = set(&["get_layout", "scroll_to_bottom"]);
    let manifest = set(&["get_layout", "scroll_to_bottom"]);
    let permissions = set(&["core:default", "core:window:allow-start-dragging", "allow-get-layout"]);
    assert_eq!(
        parity_problems(&handler, &manifest, &permissions),
        ["permission allow-scroll-to-bottom is missing from capabilities/default.json"]
    );

    // A permission nobody asked for.
    let permissions = set(&[
        "core:default",
        "core:window:allow-start-dragging",
        "allow-get-layout",
        "allow-scroll-to-bottom",
        "core:window:allow-close",
    ]);
    assert_eq!(
        parity_problems(&handler, &manifest, &permissions),
        ["permission core:window:allow-close in capabilities/default.json belongs to no manifest command"]
    );
}

#[test]
fn the_scanners_read_a_list_with_comments_and_paths() {
    let source = "x.invoke_handler(tauri::generate_handler![\n    commands::a_b, // first\n    commands::c,\n])";
    assert_eq!(handler_commands(source).unwrap(), set(&["a_b", "c"]));
    let build = "tauri_build::AppManifest::new().commands(&[\n  \"a_b\", // first\n  \"c\",\n])";
    assert_eq!(manifest_commands(build).unwrap(), set(&["a_b", "c"]));
    assert!(handler_commands("fn main() {}").is_err());
    assert!(manifest_commands("fn main() {}").is_err());
}

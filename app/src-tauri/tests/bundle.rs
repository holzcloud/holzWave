//! The facts of the two configuration files that decide what a build looks like and where it installs: the window
//! frame (research S10 and the owner decision of 2026-10-12: Windows and Linux keep the native frame in Phase 2.2, one
//! documented deviation from "the same look"; a page-drawn bar is Phase 10) and the bundle (identifier, icons, no
//! updater). Tauri documents `titleBarStyle` and `hiddenTitle` as macOS-only options, so the configuration keeps them
//! and gets no `decorations: false`; a later change of any of this fails here and is a decision, not an accident.

use std::path::Path;

use serde_json::Value;

const TAURI_CONF: &str = include_str!("../tauri.conf.json");

fn conf() -> Value {
    serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is JSON")
}

#[test]
fn windows_and_linux_keep_the_native_frame() {
    let conf = conf();
    let windows = conf["app"]["windows"].as_array().expect("app.windows is a list");
    assert_eq!(windows.len(), 1, "one window is configured");
    let window = &windows[0];
    // `decorations: false` would take the native frame away on Windows and Linux, where the page has no bar of its own
    // that could replace it (Phase 10). Absent or `true` both keep it; only an explicit `false` is a decision.
    assert_ne!(window.get("decorations"), Some(&Value::Bool(false)), "decorations: false removes the native frame");
    assert!(window.get("transparent").is_none(), "a transparent window has no frame of the system's");
    // Both are macOS-only: there they put the traffic lights over the page's bar; elsewhere they do nothing.
    assert_eq!(window["titleBarStyle"], "Overlay", "the overlay title bar is the macOS look");
    assert_eq!(window["hiddenTitle"], true, "the title text stays hidden on macOS");
}

#[test]
fn the_identifier_and_the_icons_are_fixed() {
    let conf = conf();
    // The identifier is the application id on every platform (bundle id, Wayland app id, config directory name).
    assert_eq!(conf["identifier"], "ch.holzcloud.holzwave");
    let icons = conf["bundle"]["icon"].as_array().expect("bundle.icon is a list");
    assert!(!icons.is_empty(), "no icon is configured");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for icon in icons {
        let icon = icon.as_str().expect("an icon is a path");
        assert!(root.join(icon).is_file(), "icon {icon} does not exist next to tauri.conf.json");
    }
}

#[test]
fn no_updater_is_configured() {
    // core/tests/project_policy.rs scans for the same; the keys are asserted here as well so the failure names this file.
    let conf = conf();
    assert!(conf.get("plugins").and_then(|p| p.get("updater")).is_none(), "an updater plugin is configured");
    assert!(!TAURI_CONF.to_lowercase().contains("endpoints"), "update endpoints are configured");
}

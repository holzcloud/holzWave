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

const LINUX_CONF: &str = include_str!("../tauri.linux.conf.json");
const DESKTOP_TEMPLATE: &str = include_str!("../linux/holzwave.desktop");

fn linux_conf() -> Value {
    serde_json::from_str(LINUX_CONF).expect("tauri.linux.conf.json is JSON")
}

#[test]
fn the_linux_file_builds_deb_rpm_and_appimage_from_the_desktop_template() {
    let linux = linux_conf();
    // The CLI merges tauri.<platform>.conf.json over tauri.conf.json (confirmed by scripts/linux-container-bundle.sh,
    // which builds with it). It holds only the bundle: nothing here may change the window or the app on Linux.
    let keys: Vec<&String> = linux.as_object().expect("an object").keys().filter(|k| *k != "$schema").collect();
    assert_eq!(keys, ["bundle"], "the Linux file changes only the bundle");
    // Never `all`, and no Flatpak here: the Flatpak is built from its own manifest, not by the bundler.
    assert_eq!(linux["bundle"]["targets"], serde_json::json!(["deb", "rpm", "appimage"]));
    for format in ["deb", "rpm"] {
        // Relative to the configuration file.
        let template = linux["bundle"]["linux"][format]["desktopTemplate"].as_str().expect("a desktop template");
        assert_eq!(template, "linux/holzwave.desktop", "the {format} desktop template");
        assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join(template).is_file(), "{template} does not exist");
    }
}

#[test]
fn the_desktop_template_files_holzwave_under_system_and_terminal_emulator() {
    let lines: Vec<&str> = DESKTOP_TEMPLATE.lines().collect();
    assert_eq!(lines[0], "[Desktop Entry]");
    for line in [
        "Type=Application",
        "Terminal=false",
        "Categories=System;TerminalEmulator;",
        "Keywords=terminal;shell;console;",
        // The window class of the app: the taskbar groups by it and Wayland takes it as the app id (research A12;
        // a wrong value is a one-line change, checked on hardware in test-plan row L7).
        "StartupWMClass=ch.holzcloud.holzwave",
        "Name={{name}}",
        "Exec={{exec}}",
        "Icon={{icon}}",
    ] {
        assert!(lines.contains(&line), "the desktop template lacks the line {line}");
    }
    assert!(!DESKTOP_TEMPLATE.contains("{{categories}}"), "the categories are fixed, not the bundler's default");
}

#[test]
fn no_linux_package_runs_an_install_script() {
    // T-2.2-26: a package a user installs as root runs nothing of ours. No Linux format names a script, and the
    // base file has no Linux section that could (scripts/linux-container-bundle.sh lists the package's control files).
    fn scripts(value: &Value, path: &str, found: &mut Vec<String>) {
        if let Value::Object(map) = value {
            for (key, inner) in map {
                let here = format!("{path}.{key}");
                if key.ends_with("Script") {
                    found.push(here.clone());
                }
                scripts(inner, &here, found);
            }
        }
    }
    for (name, conf) in [("tauri.linux.conf.json", linux_conf()), ("tauri.conf.json", conf())] {
        let mut found = Vec::new();
        scripts(&conf, name, &mut found);
        assert!(found.is_empty(), "install scripts are configured: {found:?}");
    }
}

#[test]
fn the_base_file_is_the_same_for_every_platform() {
    // Linux packaging lives in tauri.linux.conf.json only; macOS and Windows builds read the base file as before.
    let conf = conf();
    assert_eq!(conf["bundle"]["targets"], "all", "the base bundle targets");
    assert!(conf["bundle"].get("linux").is_none(), "the base file has a Linux bundle section");
}

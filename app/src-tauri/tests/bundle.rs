//! The facts of the configuration files that decide what a build looks like and where it installs: what every window is
//! like and the bundle (identifier, icons, no updater). The window frame is decided by code, not by the configuration:
//! `windowframe.rs` says whether a window has the system title bar and `lib.rs` builds every window, the first one
//! included, through one `build_window`. What this file pins is what the configuration must keep, because it is the one
//! description that `build_window` copies for every window: the macOS overlay, a resizable window with a minimum size, no
//! transparency and the default shadow. A later change of any of this fails here and is a decision, not an accident.

use std::path::Path;

use serde_json::Value;

const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const LIB_RS: &str = include_str!("../src/lib.rs");

fn conf() -> Value {
    serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is JSON")
}

#[test]
fn the_configuration_describes_one_window_and_leaves_the_frame_to_the_code() {
    let conf = conf();
    let windows = conf["app"]["windows"].as_array().expect("app.windows is a list");
    assert_eq!(windows.len(), 1, "one window is configured: build_window copies it for every window");
    let window = &windows[0];
    // The frame is code: macOS keeps the overlay of the configuration, and the own frame of Windows is `decorations(false)` in
    // `build_window`. An explicit `false` here would take the title bar from every platform, whatever the code decides.
    assert_ne!(window.get("decorations"), Some(&Value::Bool(false)), "decorations: false belongs to build_window");
    // Both are macOS-only: there they put the traffic lights over the page's bar; elsewhere they do nothing.
    assert_eq!(window["titleBarStyle"], "Overlay", "the overlay title bar is the macOS look");
    assert_eq!(window["hiddenTitle"], true, "the title text stays hidden on macOS");
    // A transparent undecorated window loses the system border, the shadow and the rounded corners of Windows 11.
    assert!(window.get("transparent").is_none(), "a transparent window has no border, shadow or rounded corners");
    // The edges that resize an undecorated window act only on a resizable window, and never below the minimum size.
    assert_eq!(window["resizable"], true, "the resize edges of the runtime need a resizable window");
    assert!(window["minWidth"].is_number() && window["minHeight"].is_number(), "a window has a minimum size");
    // The default is `true`, and an undecorated window gets its border and its shadow from it.
    assert!(matches!(window.get("shadow"), None | Some(Value::Bool(true))), "the shadow is the default or on");
}

/// The shipped part of `lib.rs` (before its test module) without its comment lines.
fn lib_code() -> String {
    let shipped = LIB_RS.split("\n#[cfg(test)]\nmod tests").next().expect("a first part");
    shipped.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// The text of the function that starts with `signature`, up to its closing brace at column zero.
fn body_of(code: &str, signature: &str) -> String {
    let rest = &code[code.find(signature).unwrap_or_else(|| panic!("{signature} is gone"))..];
    rest[..rest.find("\n}\n").expect("a closing brace")].to_owned()
}

#[test]
fn lib_rs_decides_the_frame_in_one_place_and_creates_no_window_from_the_configuration_on_a_desktop() {
    let code = lib_code();
    // One `.decorations(` call, in `build_window`: a second one is a second place that decides the frame.
    assert_eq!(code.matches(".decorations(").count(), 1, "one place decides the frame");
    let build = body_of(&code, "fn build_window(");
    assert!(build.contains(".decorations(false)"), "the one call is in build_window");
    assert!(build.contains("initialization_script("), "the page gets the facts before it runs");
    // The first window shows at once as a configuration window did; the others are shown by their pages.
    assert!(build.contains("visible: bool"), "build_window takes whether the window shows at once");
    assert!(build.contains(".visible("), "and sets it");
    assert!(code.contains("build_window(app.handle(), \"main\", true)"), "the first window is built visible, in setup");
    // `create = false` stops the configuration from making a window, and only on a desktop: a phone keeps its window.
    let lines: Vec<&str> = code.lines().collect();
    let sets: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].contains("create = false")).collect();
    assert!(!sets.is_empty(), "the configuration creates no window on a desktop");
    for at in sets {
        let gate = lines[..at].iter().rev().find(|line| line.trim_start().starts_with("#[cfg"));
        assert_eq!(
            gate.map(|line| line.trim()),
            Some("#[cfg(not(target_os = \"android\"))]"),
            "`create = false` is set only outside Android"
        );
    }
    // The configuration is read in one place only: every window comes from build_window.
    assert_eq!(code.matches("WebviewWindowBuilder::from_config").count(), 1, "one place builds a window");
    assert!(build.contains("WebviewWindowBuilder::from_config"), "and it is build_window");
}

#[test]
fn the_setup_decides_the_frame_from_the_choice_the_core_read_at_start() {
    let code = lib_code();
    // The stored file, the decision and the dialog's `pending` flag agree only if all three come from the one read at
    // start: the pair `started()` returns, never a literal choice and never a second read of the file.
    let started = code.find("window_frame().started()").expect("setup asks the core for the choice it started with");
    let call = code.find("windowframe::facts_for_this_system(").expect("setup decides through the one function that reads the system");
    assert!(started < call, "the started choice is read before the decision");
    let line = code[call..].lines().next().expect("a line");
    assert!(line.contains("facts_for_this_system(frame, buttons)"), "the pair of started() goes to the decision: {line}");
    let binding = code[..started].lines().last().expect("a line");
    assert!(binding.contains("let (frame, buttons) ="), "the pair is bound from started(): {binding}");
    for literal in ["FrameChoice::", "ButtonChoice::"] {
        assert!(!code.contains(literal), "{literal} is a literal choice in lib.rs: the stored one decides");
    }
    // `decide` and its inputs belong to windowframe.rs: lib.rs calls only the function that reads the system.
    assert!(!code.contains("windowframe::decide"), "lib.rs does not call decide itself");
    assert!(!code.contains("windowframe::Inputs"), "lib.rs builds no inputs of the decision itself");
    assert_eq!(code.matches("facts_for_this_system(").count(), 1, "the system is read in one place");
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

const WINDOWS_CONF: &str = include_str!("../tauri.windows.conf.json");

fn windows_conf() -> Value {
    serde_json::from_str(WINDOWS_CONF).expect("tauri.windows.conf.json is JSON")
}

#[test]
fn the_windows_file_builds_one_per_user_nsis_installer_and_nothing_else() {
    // The CLI merges tauri.<platform>.conf.json over tauri.conf.json, as it does for Linux. The file holds the bundle
    // and nothing else: the window and the app are the same on Windows as on every other platform.
    let windows = windows_conf();
    let keys: Vec<&String> = windows.as_object().expect("an object").keys().filter(|k| *k != "$schema").collect();
    assert_eq!(keys, ["bundle"], "the Windows file changes only the bundle");
    // NSIS only. No MSI: its WiX toolset is downloaded at build time and needs an optional Windows feature, and a
    // pre-release version such as 0.0.1-alpha1 is not a valid MSI version either.
    assert_eq!(windows["bundle"]["targets"], serde_json::json!(["nsis"]), "the Windows bundle targets");
    // Per user: no administrator prompt, and an upgrade of an installed copy needs none (T-2.2-31). Changing this later
    // changes where installed copies live and how they are upgraded, so it is pinned.
    assert_eq!(windows["bundle"]["windows"]["nsis"]["installMode"], "currentUser");
}

#[test]
fn the_windows_file_configures_no_signing_and_no_msi() {
    // The Windows configuration holds no signing key of any kind: an installer that is half configured for signing fails at
    // build time on a machine without the certificate, or worse, signs with the wrong one. Every key that would start
    // signing is refused here.
    let windows = windows_conf();
    let section = &windows["bundle"]["windows"];
    for key in ["certificateThumbprint", "signCommand", "timestampUrl", "tsp", "digestAlgorithm", "wix"] {
        assert!(section.get(key).is_none(), "bundle.windows.{key} is configured: no signing and no MSI in 0.0.1");
    }
    // The WebView2 install mode keeps its default (the Evergreen bootstrapper is downloaded when the runtime is
    // missing); the guide says so. A fixed or embedded runtime would be a decision with a size cost.
    assert!(section.get("webviewInstallMode").is_none(), "the WebView2 install mode is not the default");
    // No hook runs at install time (T-2.2-31): the same rule as the Linux packages.
    let nsis = section["nsis"].as_object().expect("an nsis section");
    assert_eq!(nsis.keys().collect::<Vec<_>>(), ["installMode"], "the nsis section holds the install mode only");
    let lowered = WINDOWS_CONF.to_lowercase();
    assert!(!lowered.contains("script") && !lowered.contains("hooks"), "an install script or hook is configured");
}

#[test]
fn the_windows_installer_has_its_icon_and_the_base_file_lists_it() {
    // The icon of the executable, the installer and the shortcuts comes from the base file's list.
    let conf = conf();
    let list = conf["bundle"]["icon"].as_array().expect("bundle.icon is a list");
    let icons: Vec<&str> = list.iter().filter_map(Value::as_str).collect();
    assert!(icons.contains(&"icons/icon.ico"), "icons/icon.ico is not in the base icon list");
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.ico").is_file(), "icons/icon.ico does not exist");
}

#[test]
fn the_windows_file_leaves_the_base_bundle_to_the_base_file() {
    // This plan changes no key of tauri.conf.json: the version, the identifier (the settings directory) and the
    // security policy are those of every platform.
    let windows = windows_conf();
    assert!(windows["bundle"].get("icon").is_none(), "the Windows file overrides the icon list");
    assert!(windows.get("identifier").is_none(), "the Windows file overrides the identifier");
    assert!(windows.get("version").is_none(), "the Windows file overrides the version");
    assert_eq!(conf()["bundle"]["targets"], "all", "the base bundle targets");
}

#[test]
fn a_release_build_on_windows_is_a_gui_program() {
    // Without `windows_subsystem = "windows"` the release executable is a console program and Windows opens a console
    // window beside the app (found on the first GNU-ABI build, 2026-10-14: PE subsystem CUI). The attribute applies to
    // release builds only, so `cargo run` keeps its console, and the MSVC build on a Windows host needs it as much.
    let main = include_str!("../src/main.rs");
    assert!(
        main.lines().any(|line| line.trim() == r#"#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]"#),
        "src/main.rs has no windows_subsystem attribute for release builds"
    );
}

// ---- the preview packages built from the Mac (plan 02.2-10) --------------------------------------------------------

const WINDOWS_CROSS: &str = include_str!("../../../scripts/release-windows-cross.sh");
const LINUX_CONTAINER: &str = include_str!("../../../scripts/release-linux-container.sh");
const BUILD_WINDOWS_DOC: &str = include_str!("../../../docs/build-windows.md");
const BUILD_LINUX_DOC: &str = include_str!("../../../docs/build-linux.md");

/// The lines of a shell script that run something: no blank line and no comment, and the text between double quotes is cut
/// out of each line, so a message that names a command for the reader (`need "... brew install mingw-w64"`) is not a
/// command. What a script says it does not do must hold for these lines.
fn commands(script: &str) -> Vec<String> {
    script
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut out = String::new();
            let mut quoted = false;
            for c in l.chars() {
                if c == '"' {
                    quoted = !quoted;
                } else if !quoted {
                    out.push(c);
                }
            }
            out
        })
        .collect()
}

#[test]
fn the_cross_release_scripts_only_build_and_never_publish() {
    // The owner publishes by hand with `gh release upload` (the scripts print the command and do not run it), and nothing
    // here installs a tool: a missing tool is exit 2 with the command that fixes it (the owner approved mingw-w64 only).
    for (name, script) in [("release-windows-cross.sh", WINDOWS_CROSS), ("release-linux-container.sh", LINUX_CONTAINER)] {
        for line in commands(script) {
            // (`apt-get install` inside the Dockerfile heredoc is the image's own build and is allowed.)
            for forbidden in ["gh release", "gh api", "git push", "git tag", "--publish", "brew install", "cargo install", "rustup target add", "sudo "] {
                assert!(!line.contains(forbidden), "{name} runs `{forbidden}`: {line}");
            }
        }
        assert!(script.contains("gh release upload"), "{name} does not tell how to attach the files");
        assert!(script.contains(".sha256"), "{name} writes no checksum file");
    }
}

#[test]
fn the_cross_release_scripts_name_the_artifacts_by_one_scheme() {
    assert!(WINDOWS_CROSS.contains(r#"name="holzWave_${version}_x64-setup.exe""#), "the Windows installer name");
    for name in [
        r#""holzWave_${version}_amd64.deb""#,
        r#""holzWave_${version}_x86_64.rpm""#,
        r#""holzWave_${version}_x86_64.AppImage""#,
    ] {
        assert!(LINUX_CONTAINER.contains(name), "the Linux scripts does not name {name}");
    }
    // One folder for all of them, the one `gh release upload` is pointed at.
    assert!(WINDOWS_CROSS.contains("out=target/release-cross") && LINUX_CONTAINER.contains("out=target/release-cross"));
}

#[test]
fn the_windows_cross_build_is_the_gnu_abi_and_says_so() {
    // Honest labelling: this build is not the MSVC build, and its runs on Windows are the owner's, recorded in the W rows
    // of the test plan. The script prints it, the build guide says it, and the target is the GNU one (the MSVC engine
    // cannot be built from a Mac).
    assert!(WINDOWS_CROSS.contains("TARGET=x86_64-pc-windows-gnu"), "the target is not the GNU ABI");
    assert!(!WINDOWS_CROSS.contains("windows-msvc"), "the cross build names the MSVC target");
    for text in ["GNU-ABI build", "not the MSVC build", "unsigned", "W rows of docs/test-plan.md"] {
        assert!(WINDOWS_CROSS.contains(text), "release-windows-cross.sh does not say `{text}`");
    }
    for text in ["GNU ABI", "run this GNU-ABI holzWave on Windows", "unsigned", "MSVC build"] {
        assert!(BUILD_WINDOWS_DOC.contains(text), "docs/build-windows.md does not say `{text}`");
    }
    // The link fix and the NSIS route the research found; losing either breaks the build or the installer.
    let code = |needle: &str| WINDOWS_CROSS.lines().any(|l| !l.trim_start().starts_with('#') && l.contains(needle));
    assert!(code("-Wl,--no-gc-sections"), "the link flag for ___chkstk_ms is not passed (a comment does not count)");
    assert!(code("CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS="), "the flag must apply to this target only");
    assert!(WINDOWS_CROSS.contains("makensis") && WINDOWS_CROSS.contains("docker run"), "NSIS runs in Docker");
}

#[test]
fn the_linux_container_build_is_amd64_and_says_what_it_is_not() {
    assert!(LINUX_CONTAINER.contains("HOLZWAVE_LINUX_ARCH=amd64"), "the build is not the amd64 container");
    assert!(LINUX_CONTAINER.contains("HOLZWAVE_BUNDLE_APPIMAGE=required"), "a missing AppImage must fail the release");
    for text in ["emulated", "glibc 2.39", "unsigned", "not on Linux hardware", "L rows record the runs on Linux"] {
        assert!(LINUX_CONTAINER.contains(text), "release-linux-container.sh does not say `{text}`");
    }
    assert!(BUILD_LINUX_DOC.contains("glibc 2.39") && BUILD_LINUX_DOC.contains("emulated"), "docs/build-linux.md lacks the limits");
}

// ---- the signing scripts (POL-03, D-10, D-12) ----------------------------------------------------------------

const SIGN_LINUX: &str = include_str!("../../../scripts/sign-linux.sh");
const VERIFY_RELEASE: &str = include_str!("../../../scripts/verify-release.sh");

/// The lines of a script that are not blank and not comments, as written (the text between quotes is kept).
fn code_lines(script: &str) -> Vec<&str> {
    script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

#[test]
fn the_signing_scripts_never_publish_install_or_reach_the_network() {
    // The owner uploads by hand: sign-linux.sh prints the `gh release upload` command and does not run it. Nothing here
    // installs a tool (a missing minisign is exit 2 with the command that fixes it), pushes, tags or opens a connection,
    // and nothing traces (`set -x` would print the password) or evaluates text.
    for (name, script) in [("sign-linux.sh", SIGN_LINUX), ("verify-release.sh", VERIFY_RELEASE)] {
        for line in commands(script) {
            for forbidden in [
                "gh release", "gh api", "git push", "git tag", "--publish", "brew install", "cargo install", "npm install", "apt-get",
                "sudo ", "curl", "wget", "nc ", "eval ", "set -x",
            ] {
                assert!(!line.contains(forbidden), "{name} runs `{forbidden}`: {line}");
            }
        }
    }

    // The password: looked up in the operating system's secret store, and handed to minisign on standard input only.
    let lines = commands(SIGN_LINUX);
    for needed in ["security find-generic-password", "secret-tool lookup", "| minisign"] {
        assert!(lines.iter().any(|l| l.contains(needed)), "sign-linux.sh has no command line with `{needed}`");
    }
    let raw = code_lines(SIGN_LINUX);
    for line in &raw {
        assert!(!line.starts_with("export ") && !line.contains(" export "), "sign-linux.sh exports something: {line}");
        if line.contains("$password") {
            // The only two places the variable is expanded: the printf that feeds minisign, and the test that it is not empty.
            let feeds = line.starts_with(r#"printf '%s\n' "$password" | minisign "#);
            let tests_empty = line.starts_with(r#"[ -n "$password" ] || fail "#);
            assert!(feeds || tests_empty, "sign-linux.sh expands the password outside the printf that feeds minisign: {line}");
        }
        assert!(line.matches("$password").count() <= 1, "the password is expanded twice on one line: {line}");
        for leak in ["echo \"$password", "echo $password", "printf \"$password", "-x \"$password", ">>\"$password"] {
            assert!(!line.contains(leak), "sign-linux.sh prints the password: {line}");
        }
    }
    assert!(raw.iter().any(|l| l.starts_with(r#"printf '%s\n' "$password" | minisign -S "#)), "the password does not go to minisign -S on standard input");
    assert!(raw.iter().any(|l| l.starts_with("unset password")), "the password variable is not unset after the last signature");
    // The key password never comes from a file, a prompt or the environment: no `read`, no `-w "$`, no HOLZWAVE_..._PASSWORD.
    assert!(!SIGN_LINUX.contains("MINISIGN_PASSWORD") && !SIGN_LINUX.contains("MINISIGN_PASS"), "a password variable of the environment");
    assert!(!raw.iter().any(|l| l.starts_with("read ") || l.contains(" read -")), "sign-linux.sh reads from the terminal or a file");
    // Only the checks the script claims to make: every signature is verified with the public key before it says it is done.
    assert!(SIGN_LINUX.contains("holzwave.pub") && SIGN_LINUX.contains("minisign -V -p"), "sign-linux.sh does not verify with the public key");
}

#[test]
fn the_secret_key_has_no_place_in_the_repository() {
    // The key file is encrypted and lives outside the tree; the script's default is under $HOME and it refuses a key inside
    // the repository. A tracked `.key` or `.minisig` file, or a public key anywhere but packaging/minisign/holzwave.pub,
    // fails here (the public half is meant to be committed, the signatures belong to a release and not to the tree).
    let default = SIGN_LINUX
        .lines()
        .find(|l| l.trim_start().starts_with("key=") && l.contains("HOLZWAVE_MINISIGN_KEY"))
        .expect("sign-linux.sh sets the key path from HOLZWAVE_MINISIGN_KEY");
    assert!(default.contains("$HOME") || default.contains("${HOME"), "the default key path is not under HOME: {default}");
    assert!(default.contains("/.minisign/holzwave.key"), "the default key path is not $HOME/.minisign/holzwave.key: {default}");
    assert!(SIGN_LINUX.contains("is inside the repository"), "sign-linux.sh does not refuse a key inside the repository");

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("git").arg("-C").arg(&root).args(["ls-files", "-z"]).output().expect("git runs");
    assert!(out.status.success(), "git ls-files failed: {}", String::from_utf8_lossy(&out.stderr));
    let listing = String::from_utf8(out.stdout).expect("file names are UTF-8");
    // A repository shell with an empty index lists nothing and must not pass: the workspace manifest is tracked.
    assert!(listing.split('\0').any(|p| p == "Cargo.toml"), "git ls-files lists no Cargo.toml: an empty index checks nothing");
    for path in listing.split('\0').filter(|p| !p.is_empty()) {
        assert!(!path.ends_with(".key"), "a secret key file is tracked: {path}");
        assert!(!path.ends_with(".minisig"), "a signature file is tracked: {path}");
        if path.ends_with(".pub") {
            assert_eq!(path, "packaging/minisign/holzwave.pub", "a public key file is tracked where none belongs: {path}");
        }
    }
}

const INFO_PLIST: &str = include_str!("../Info.plist");

#[test]
fn the_plist_declares_the_use_of_the_local_network() {
    // The Tauri bundler merges Info.plist (next to tauri.conf.json) into the app, and tauri-codegen embeds it into a
    // development binary. macOS shows its Local Network question with the usage description as text, and lets an app
    // browse only the Bonjour types listed in NSBonjourServices. Plain string checks: no plist crate.
    assert!(INFO_PLIST.contains("<key>NSLocalNetworkUsageDescription</key>"));
    assert!(INFO_PLIST.contains("<key>NSBonjourServices</key>"));
    let after = INFO_PLIST.split("<key>NSLocalNetworkUsageDescription</key>").nth(1).expect("the key is there");
    let text = after.split("<string>").nth(1).and_then(|rest| rest.split("</string>").next()).unwrap_or("");
    assert!(!text.trim().is_empty(), "the usage description is empty");
    assert!(text.len() <= 400, "the usage description is a sentence, not a page");
    assert!(Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist").is_file(), "Info.plist sits next to tauri.conf.json");
}

// ---- the aarch64 Linux packages (the second wave, built in the native arm64 container) -------------------------

const LINUX_ARM64: &str = include_str!("../../../scripts/release-linux-arm64.sh");

/// The text of a script with every run of white space cut to one space, so a phrase is found across a wrapped line.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs a script with `sh` from the repository root and returns its exit code and standard error.
fn run_script(script: &str, args: &[&str]) -> (i32, String) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("sh").current_dir(&root).arg(script).args(args).output().expect("sh runs");
    (out.status.code().expect("the script ended by itself"), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_arm64_script_refuses_what_is_not_a_version_before_it_touches_anything() {
    assert!(LINUX_ARM64.starts_with("#!/bin/sh\n"), "release-linux-arm64.sh is not a /bin/sh script");
    // Every refusal below happens before Docker, npm or git are asked, so it runs the same on any machine.
    for args in [&[][..], &["0.0.1", "extra"], &["bad/version"], &["0.0.1 beta"], &["-0.0.1"], &["0.0.1;id"], &[""]] {
        let (code, err) = run_script("scripts/release-linux-arm64.sh", args);
        assert_eq!(code, 2, "arguments {args:?} were not refused with 2: {err}");
        assert!(err.starts_with("release-linux-arm64: "), "the refusal does not carry the script's name: {err}");
    }
    // The usage line names the one argument.
    let (_, err) = run_script("scripts/release-linux-arm64.sh", &[]);
    assert!(err.contains("usage: sh scripts/release-linux-arm64.sh <version>"), "no usage line: {err}");
}

#[test]
fn the_arm64_script_checks_the_version_the_tree_and_the_host() {
    let text = squash(LINUX_ARM64);
    // The version is the one of tauri.conf.json, and a dirty tree is a failure (exit 1, `fail`).
    assert!(text.contains(r#"app/src-tauri/tauri.conf.json"#) && text.contains(r#"[ "$conf" = "$version" ] || fail "version"#), "the version is not compared with tauri.conf.json");
    assert!(text.contains(r#"[ -z "$(git status --porcelain)" ] || fail "the working tree is not clean""#), "a dirty tree is not refused");
    // An emulated arm64 build is untried and linuxdeploy cannot cross-compile ARM AppImages: a host that is not arm64 stops.
    assert!(text.contains("arm64 | aarch64) ;;") && text.contains("not arm64") && text.contains("cannot cross-compile"), "the host check or its reason is missing");
    // The tool checks are refusals (exit 2).
    for tool in ["docker is missing", "the Docker daemon does not answer", "npm is missing"] {
        assert!(text.contains(tool), "no refusal `{tool}`");
    }
}

#[test]
fn the_arm64_script_builds_natively_and_never_as_amd64() {
    // linux-container-bundle.sh is run with no architecture variable (the native arm64 container) and with the AppImage required.
    let lines = code_lines(LINUX_ARM64);
    let run = lines.iter().find(|l| l.contains("linux-container-bundle.sh")).expect("the script does not run linux-container-bundle.sh");
    assert!(run.contains("HOLZWAVE_BUNDLE_APPIMAGE=required"), "a missing AppImage must fail the build: {run}");
    for line in &lines {
        for forbidden in ["HOLZWAVE_LINUX_ARCH", "amd64", "--platform", "x86_64"] {
            assert!(!line.contains(forbidden), "release-linux-arm64.sh runs with `{forbidden}`: {line}");
        }
    }
    // The inspections run in the arm64 image, which is the default one of linux-container-bundle.sh.
    assert!(LINUX_ARM64.contains("IMAGE=holzwave-linux-bundle\n"), "the inspection image is not holzwave-linux-bundle");
    assert!(LINUX_ARM64.contains(r#"built="$root/target/linux-bundle""#), "the packages are not read from target/linux-bundle");
}

#[test]
fn the_arm64_script_demands_the_architecture_words_of_each_package() {
    let text = squash(LINUX_ARM64);
    for demand in [
        r#"grep -qx 'deb arm64' || fail "the deb is not arm64""#,
        r#"grep -qx 'rpm aarch64' || fail "the rpm is not aarch64""#,
        r#"grep -q '^appimage ELF 64-bit LSB .*ARM aarch64' || fail"#,
        r#"grep -q '^appimage-program ELF 64-bit LSB .*ARM aarch64' || fail "the program inside the AppImage is not an aarch64 ELF file""#,
        r#"grep -qx 'appimage-apprun yes' || fail"#,
        r#"grep -qx 'appimage-webkit yes' || fail"#,
    ] {
        assert!(text.contains(demand), "release-linux-arm64.sh does not demand `{demand}`");
    }
    // The tools that read the architecture, as the x86_64 script reads it, and the ELF header offset of the squashfs.
    for tool in ["dpkg-deb -f", "rpm -qp --qf", "file -b", "readelf -h", "unsquashfs -o"] {
        assert!(text.contains(tool), "release-linux-arm64.sh does not run `{tool}`");
    }
}

#[test]
fn the_arm64_script_writes_the_release_names_and_a_checksum_each() {
    for name in [r#""holzWave_${version}_arm64.deb""#, r#""holzWave_${version}_aarch64.rpm""#, r#""holzWave_${version}_aarch64.AppImage""#] {
        assert!(LINUX_ARM64.contains(name), "release-linux-arm64.sh does not name {name}");
    }
    // The same folder as the x86_64 packages, a .sha256 file for each, and nothing of theirs is removed or renamed.
    assert!(LINUX_ARM64.contains("out=target/release-cross") && LINUX_CONTAINER.contains("out=target/release-cross"));
    assert!(LINUX_ARM64.contains(r#"echo "$sum  $2" >"$out/$2.sha256""#), "no checksum file is written");
    for line in commands(LINUX_ARM64) {
        let padded = format!(" {line} ");
        assert!(!padded.contains(" rm ") && !padded.contains(" mv "), "the script removes or renames a file: {line}");
    }
}

#[test]
fn the_arm64_script_signs_and_publishes_nothing_and_says_what_it_is() {
    for line in commands(LINUX_ARM64) {
        for forbidden in ["gh ", "curl", "docker push", "minisign", "git push", "git tag", "sudo ", "brew install", "cargo install"] {
            assert!(!line.contains(forbidden), "release-linux-arm64.sh runs `{forbidden}`: {line}");
        }
    }
    let text = squash(LINUX_ARM64);
    for said in [
        "built in a native arm64 container on an Apple Silicon Mac",
        "were not run on ARM Linux hardware",
        "the container cannot start the AppImage",
        "unsigned until the owner signs",
        "sh scripts/sign-linux.sh $version --arch aarch64",
        "nothing was signed or published",
        "release-linux-arm64: ok",
    ] {
        assert!(text.contains(said), "release-linux-arm64.sh does not say `{said}`");
    }
}

#[test]
fn the_signing_script_signs_one_architecture_per_run_and_keeps_the_default() {
    let text = squash(SIGN_LINUX);
    // The default is the x86_64 set and `SHA256SUMS`, as before.
    assert!(text.contains("arch=x86_64"), "the default architecture is not x86_64");
    for kept in [
        r#"deb="holzWave_${version}_amd64.deb""#,
        r#"rpm="holzWave_${version}_x86_64.rpm""#,
        r#"appimage="holzWave_${version}_x86_64.AppImage""#,
        "sumsfile=SHA256SUMS\n",
    ] {
        assert!(SIGN_LINUX.contains(kept), "the x86_64 set changed: `{kept}` is gone");
    }
    // --arch aarch64: the names the build script writes, and a list of their own.
    for name in [
        r#"deb="holzWave_${version}_arm64.deb""#,
        r#"rpm="holzWave_${version}_aarch64.rpm""#,
        r#"appimage="holzWave_${version}_aarch64.AppImage""#,
        "sumsfile=SHA256SUMS-aarch64",
    ] {
        assert!(text.contains(name), "sign-linux.sh does not know `{name}`");
    }
    for name in ["_arm64.deb", "_aarch64.rpm", "_aarch64.AppImage"] {
        assert!(LINUX_ARM64.contains(name) && SIGN_LINUX.contains(name), "the build and the signing script disagree on {name}");
    }
    // An unknown architecture is a refusal (fail = exit 2) in the argument loop, before anything is read.
    assert!(text.contains("--arch) [ $# -ge 2 ] || fail") && text.contains("unknown architecture for --arch"), "--arch is not validated");
    // The cleanup removes only this run's list and signatures, which are named by the variable and never by a glob.
    assert!(text.contains(r#"for n in $files $sumsfile; do rm -f "$dir/$n.minisig"; done"#), "the cleanup does not name this architecture's files");
    for line in code_lines(SIGN_LINUX) {
        assert!(!line.contains("*.minisig") && !line.contains("SHA256SUMS*"), "a glob in sign-linux.sh could remove the other set: {line}");
    }
}

#[test]
fn the_verify_script_checks_either_list_and_treats_the_other_one_as_covering_its_own_files() {
    let text = squash(VERIFY_RELEASE);
    for needed in [
        "SHA256SUMS-aarch64",
        "--arch)",
        "--sums)",
        "no other list of the folder names it",
        "not a list name",
        "give --arch or --sums once, not both",
    ] {
        assert!(text.contains(needed), "verify-release.sh does not say `{needed}`");
    }
    // A list name given on the command line has no path part and no leading dash: the pattern anchors on SHA256SUMS and allows
    // only name characters, never a slash.
    assert!(VERIFY_RELEASE.contains(r#"grep -Eq '^SHA256SUMS[A-Za-z0-9._+-]*$'"#), "a list name is not checked against a strict pattern");
    // The names read out of the other lists are untrusted too: only lines of the strict shape are used, nothing is opened by them.
    assert!(VERIFY_RELEASE.contains("[0-9a-f]{64}  [A-Za-z0-9_][A-Za-z0-9._+-]*$"), "the names read from a list are not checked against the strict pattern");
    // It reads files and runs minisign and a checksum tool only (the existing rule of the two scripts, stated for the new lines).
    for line in commands(VERIFY_RELEASE) {
        for forbidden in ["curl", "wget", "eval ", "rm ", "mv ", "cp "] {
            assert!(!line.contains(forbidden), "verify-release.sh runs `{forbidden}`: {line}");
        }
    }
}

#[test]
fn the_password_rules_of_the_signing_script_hold_for_the_second_set() {
    // The same lines as before and no more: the password is fetched once, held in one unexported variable, expanded in the
    // printf that feeds minisign on standard input and in the empty test, and unset. `--arch` added no line that mentions it.
    let uses: Vec<&str> = code_lines(SIGN_LINUX).into_iter().filter(|l| l.contains("password")).collect();
    let feeds = uses.iter().filter(|l| l.starts_with(r#"printf '%s\n' "$password" | minisign -S "#)).count();
    assert_eq!(feeds, 1, "the password is fed to minisign on more or fewer than one line: {uses:?}");
    for line in &uses {
        let allowed = line.starts_with("password=$(security find-generic-password")
            || line.starts_with("*) password=$(secret-tool lookup")
            || line.starts_with(r#"[ -n "$password" ] || fail "#)
            || line.starts_with(r#"printf '%s\n' "$password" | minisign -S "#)
            || line.starts_with("unset password")
            || line.starts_with("Darwin) password=$(security find-generic-password")
            || (line.starts_with("|| fail ") && !line.contains("$password"))
            || line.starts_with("sign_one()")
            || line.contains("the password");
        assert!(allowed, "an unexpected line mentions the password: {line}");
    }
    // sign_one is called for every file and for the list of either architecture, with no password argument.
    assert!(code_lines(SIGN_LINUX).iter().any(|l| l.starts_with(r#"sign_one "$dir/$sumsfile" "holzWave $version $sumsfile" || die 1"#)), "the list is not signed through sign_one");
}

// ---- the pages about the aarch64 set say what ran and what did not -------------------------------------------------

const SIGNING_DOC: &str = include_str!("../../../docs/signing.md");

/// The first label of the private planning tree in a public text: a decision id `D-0..`, a plan id `13-0..` or a threat id
/// `T-13..`, each at the start of a word (so a date such as `2026-11-03` is not one).
fn has_planning_label(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    for i in 0..chars.len() {
        if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '-') {
            continue;
        }
        let rest: String = chars[i..chars.len().min(i + 5)].iter().collect();
        if ["D-0", "13-0", "T-13"].iter().any(|p| rest.starts_with(p)) {
            return Some(rest);
        }
    }
    None
}

#[test]
fn the_pages_describe_the_aarch64_set_and_its_limits() {
    for (name, page) in [("docs/signing.md", SIGNING_DOC), ("docs/build-linux.md", BUILD_LINUX_DOC)] {
        let text = squash(page);
        assert!(text.contains("SHA256SUMS-aarch64") || name.ends_with("build-linux.md"), "{name} does not name SHA256SUMS-aarch64");
        assert!(text.contains("never started on real ARM Linux hardware"), "{name} does not say the aarch64 packages never ran on real hardware");
        assert!(text.contains("release-linux-arm64.sh"), "{name} does not name release-linux-arm64.sh");
        assert!(text.contains("--arch aarch64"), "{name} does not name the aarch64 signing and verifying command");
        assert!(text.contains("container"), "{name} does not say the packages come from a container");
        assert_eq!(has_planning_label(page), None, "{name} carries a label of the planning tree");
        assert!(!page.contains(".planning/"), "{name} names a path of the private planning tree");
    }
    let signing = squash(SIGNING_DOC);
    // Nothing is signed yet, and the status box still says so: the key does not exist.
    assert!(signing.contains("No release has been signed yet"), "docs/signing.md no longer says that no release has been signed");
    assert!(signing.contains("the real key does not exist"), "docs/signing.md no longer says that the key does not exist");
    assert!(signing.contains("SHA256SUMS") && signing.contains("six files in two sets"), "the table row does not describe the two sets");
    assert!(signing.contains("the list of **your own architecture**"), "docs/signing.md does not tell a person to check the list of their own architecture");
    assert!(signing.contains("The container cannot start the AppImage"), "docs/signing.md does not say the AppImage was not started");
    // The aarch64 AppImage is built on ARM only; the page says why, and what is not covered.
    let build = squash(BUILD_LINUX_DOC);
    for said in [
        "built only on ARM",
        "does not cross-compile ARM AppImages",
        "FUSE 2",
        "libfuse2t64",
        "The container has no FUSE and no display",
        "no release has been signed yet",
        "Not covered:** any run on real ARM Linux hardware; every desktop",
    ] {
        assert!(build.contains(said), "docs/build-linux.md does not say `{said}`");
    }
}

#[test]
fn the_second_wave_is_not_marked_done_by_the_pages() {
    // The pages describe tools, not a release: no page says the aarch64 packages were released or signed.
    for page in [SIGNING_DOC, BUILD_LINUX_DOC] {
        let text = squash(page).to_lowercase();
        assert!(!text.contains("aarch64 packages are signed") && !text.contains("aarch64 release is signed"), "a page claims a signed aarch64 release");
    }
}

#[test]
fn the_label_scan_finds_the_shapes_it_is_meant_to_find() {
    // Built from pieces, so that this file holds no label itself (the repository guard reads it too).
    let phase = String::from("1") + "3";
    for bad in [
        format!("see plan {phase}-07 for it"),
        String::from("decision D-03 says"),
        format!("threat T-{phase}-16 covers"),
        format!("({phase}-07)"),
    ] {
        assert!(has_planning_label(&bad).is_some(), "the scan misses a label in `{bad}`");
    }
    for fine in ["on 2026-11-03 it", "sha256 of 0.0.1-beta2", "x86-64 and aarch64", "glibc 2.39", "ELF 64-bit LSB"] {
        assert_eq!(has_planning_label(fine), None, "the scan flags `{fine}`");
    }
}

// ---- the Android keystore script: the password reaches keytool on standard input and nowhere else -----------------------

const ANDROID_KEYSTORE: &str = include_str!("../../../scripts/android-keystore.sh");
const ANDROID_KEYSTORE_TEST: &str = include_str!("../../../scripts/test-android-keystore.sh");

/// The words of a command line, split at everything that is not part of a name, so `gh` is found as a command and not inside
/// `through`.
fn words(line: &str) -> Vec<&str> {
    line.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.')).filter(|w| !w.is_empty()).collect()
}

#[test]
fn the_keystore_script_never_publishes_installs_traces_or_reaches_the_network() {
    for line in commands(ANDROID_KEYSTORE) {
        for forbidden in [
            "set -x", "eval ", "git push", "git tag", "brew install", "cargo install", "npm install", "apt-get", "sudo ", "nc ",
        ] {
            assert!(!line.contains(forbidden), "android-keystore.sh runs `{forbidden}`: {line}");
        }
        for word in words(&line) {
            assert!(!["curl", "wget", "gh", "ssh", "scp", "rsync", "ftp"].contains(&word), "android-keystore.sh runs `{word}`: {line}");
        }
    }
    // It refuses to run while tracing is on, because a trace would print the password.
    assert!(code_lines(ANDROID_KEYSTORE).iter().any(|l| l.starts_with(r#"case "$-" in *x*) fail "tracing is on"#)), "no refusal while tracing");
}

#[test]
fn the_keystore_password_is_fed_on_standard_input_and_appears_nowhere_else() {
    let raw = code_lines(ANDROID_KEYSTORE);
    // A password on a command line shows in the process list.
    for line in &raw {
        for forbidden in ["-storepass", "-keypass", "-srcstorepass", "-deststorepass", "--ks-pass", "--key-pass", "pass:", "env:"] {
            assert!(!line.contains(forbidden), "android-keystore.sh puts a password in an argument (`{forbidden}`): {line}");
        }
        assert!(!line.starts_with("export ") && !line.contains(" export "), "android-keystore.sh exports something: {line}");
        assert!(!line.starts_with("read ") && !line.contains(" read -"), "android-keystore.sh reads from the terminal or a file: {line}");
        assert!(!line.contains("set -x") && !line.contains("set -o xtrace"), "android-keystore.sh traces: {line}");
    }
    // The variable is expanded in three places only: the empty test and the two feeding functions.
    for line in &raw {
        if line.contains("$password") {
            let tests_empty = line.starts_with(r#"[ -n "$password" ] || fail "#);
            let once = *line == r#"feed_once() { printf '%s\n' "$password"; }"#;
            let twice = *line == r#"feed_twice() { printf '%s\n' "$password"; printf '%s\n' "$password"; }"#;
            assert!(tests_empty || once || twice, "android-keystore.sh expands the password outside the feeding functions: {line}");
        }
        for leak in ["echo \"$password", "echo $password", "printf \"$password", ">\"$password", ">>\"$password"] {
            assert!(!line.contains(leak), "android-keystore.sh prints the password: {line}");
        }
    }
    // keytool receives it from those functions through a pipe: once to read a store, twice to make one.
    assert!(raw.iter().any(|l| l.starts_with("feed_twice | keytool -genkeypair ")), "the password does not go to keytool -genkeypair on standard input");
    assert!(raw.iter().any(|l| l.contains("feed_once | keytool -list ")), "the password does not go to keytool -list on standard input");
    assert_eq!(raw.iter().filter(|l| l.contains("-genkeypair")).count(), 1, "more than one line makes a key");
    assert!(raw.iter().any(|l| l.starts_with("unset password")), "the password variable is not unset after use");
    // It comes from the operating system's secret store, never from the environment.
    assert!(raw.iter().any(|l| l.contains("security find-generic-password") && l.contains("-w")), "no macOS keychain lookup");
    assert!(raw.iter().any(|l| l.contains("secret-tool lookup")), "no Linux secret store lookup");
    assert!(ANDROID_KEYSTORE.contains("SECRET_SERVICE=holzwave-android-keystore"), "the secret store entry is not holzwave-android-keystore");
    assert!(!ANDROID_KEYSTORE.contains("KEYSTORE_PASS") && !ANDROID_KEYSTORE.contains("KEY_PASSWORD"), "a password variable of the environment");
}

#[test]
fn the_keystore_script_keeps_the_key_outside_the_repository_and_never_overwrites_it() {
    let default = ANDROID_KEYSTORE
        .lines()
        .find(|l| l.trim_start().starts_with("ks=${HOLZWAVE_ANDROID_KEYSTORE:-"))
        .expect("android-keystore.sh sets the keystore path from HOLZWAVE_ANDROID_KEYSTORE");
    assert!(default.contains("${HOME") || default.contains("$HOME"), "the default keystore path is not under HOME: {default}");
    assert!(default.contains("/.android-keys/holzwave-release.p12"), "the default keystore path is not $HOME/.android-keys/holzwave-release.p12: {default}");
    assert!(ANDROID_KEYSTORE.contains("is inside the repository"), "android-keystore.sh does not refuse a keystore path inside the repository");
    assert!(ANDROID_KEYSTORE.contains("a link into the repository"), "android-keystore.sh does not refuse a link into the repository");
    assert!(ANDROID_KEYSTORE.contains("a key is never overwritten"), "android-keystore.sh does not refuse an existing keystore");
    assert!(ANDROID_KEYSTORE.contains("pwd -P"), "android-keystore.sh does not resolve the real path of the keystore directory");
    // The names every listing repeats.
    for constant in ["ALIAS=holzwave-release", "SUBJECT='CN=holzcloud.ch, OU=holzWave'", "STORE_TYPE=PKCS12"] {
        assert!(code_lines(ANDROID_KEYSTORE).contains(&constant), "android-keystore.sh lacks the constant `{constant}`");
    }
    // The script ends `create` with the backup instruction and prints the fingerprint a person can check an APK against.
    assert!(ANDROID_KEYSTORE.contains("off-machine backup of the keystore file and of the keychain entry"), "create does not tell the owner to back up the key");
    assert!(ANDROID_KEYSTORE.contains("apksigner verify --print-certs"), "the script does not name the command that shows an APK's certificate");

    // No keystore, and no file that names its password, is tracked.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::process::Command::new("git").arg("-C").arg(&root).args(["ls-files", "-z"]).output().expect("git runs");
    assert!(out.status.success(), "git ls-files failed: {}", String::from_utf8_lossy(&out.stderr));
    let listing = String::from_utf8(out.stdout).expect("file names are UTF-8");
    assert!(listing.split('\0').any(|p| p == "Cargo.toml"), "git ls-files lists no Cargo.toml: an empty index checks nothing");
    for path in listing.split('\0').filter(|p| !p.is_empty()) {
        for ext in [".jks", ".keystore", ".p12"] {
            assert!(!path.ends_with(ext), "a keystore file is tracked: {path}");
        }
        assert!(!path.ends_with("keystore.properties"), "a file that holds a keystore password is tracked: {path}");
    }
}

#[test]
fn the_keystore_script_and_its_test_carry_no_label_of_the_planning_tree() {
    for (name, text) in [("android-keystore.sh", ANDROID_KEYSTORE), ("test-android-keystore.sh", ANDROID_KEYSTORE_TEST)] {
        assert_eq!(has_planning_label(text), None, "{name} carries a label of the planning tree");
        assert!(!text.contains(".planning/"), "{name} names a path of the private planning tree");
    }
}

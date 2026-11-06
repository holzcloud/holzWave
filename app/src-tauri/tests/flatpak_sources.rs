//! The generated source files of the Flatpak under `packaging/flatpak/generated/`: the Ghostty commit that the engine
//! crate pins, the Zig packages that the engine build really fetches (each with a sha256 from two agreeing downloads), and
//! how the app is compiled without the Tauri CLI. The files are tool output of `scripts/flatpak-zig-sources.sh` and
//! `scripts/build-flatpak.sh`; these tests read them as text, so a hand edit, a lost hash or a drifted pin fails here.
//!
//! That the engine builds offline from these sources is shown by `sh scripts/build-flatpak.sh --engine-offline` (Docker, one
//! architecture); that the app builds with the plain command is shown by `--app-cli-free`. Neither runs in `cargo test`.
//!
//! Status: unverified beyond one architecture (arm64 in the Docker VM of the owner's Mac). The x86_64 engine build was not run.

use serde_json::Value;

const PIN: &str = include_str!("../../../packaging/flatpak/generated/ghostty-pin.txt");
const SOURCES: &str = include_str!("../../../packaging/flatpak/generated/zig-sources.json");
const APP_COMMAND: &str = include_str!("../../../packaging/flatpak/generated/app-build-command.txt");
const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");
const TOOL: &str = include_str!("../../../scripts/flatpak-zig-sources.sh");
const SCRIPT: &str = include_str!("../../../scripts/build-flatpak.sh");

/// How many Zig packages Ghostty lists in its own `build.zig.zon.json` at the pinned commit. The library build uses fewer.
const GHOSTTY_LISTED_PACKAGES: usize = 37;

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// The value of `key=` in `text`, comment lines skipped.
fn setting(text: &str, key: &str) -> Option<String> {
    text.lines().filter(|l| !l.starts_with('#')).find_map(|l| l.strip_prefix(&format!("{key}="))).map(|v| v.trim().to_owned())
}

fn sources() -> Vec<Value> {
    match serde_json::from_str::<Value>(SOURCES).expect("zig-sources.json is JSON") {
        Value::Array(entries) => entries,
        other => panic!("zig-sources.json is not a list of sources: {other}"),
    }
}

#[test]
fn the_pin_holds_a_ghostty_commit_and_an_engine_revision_that_cargo_lock_names() {
    let commit = setting(PIN, "ghostty-commit").expect("a ghostty-commit line");
    assert!(is_hex(&commit, 40), "the Ghostty commit is 40 lower-case hex digits: {commit}");
    let revision = setting(PIN, "libghostty-rs-revision").expect("a libghostty-rs-revision line");
    assert!(is_hex(&revision, 40), "the engine revision is 40 lower-case hex digits: {revision}");
    // The revision is the one the app builds: both engine packages of the lock file carry it, so the Ghostty commit that the
    // pin took from the crate's build script cannot belong to another revision.
    let at = |name: &str| CARGO_LOCK.find(&format!("name = \"{name}\"")).unwrap_or_else(|| panic!("{name} is not in Cargo.lock"));
    for name in ["libghostty-vt", "libghostty-vt-sys"] {
        let block = &CARGO_LOCK[at(name)..];
        let source = block.lines().find(|l| l.starts_with("source = ")).expect("a source line");
        assert!(source.contains(&format!("rev={revision}#{revision}")), "Cargo.lock names another revision for {name}: {source}");
    }
    assert!(setting(PIN, "ghostty-repository").is_some_and(|r| r.starts_with("https://github.com/")), "the repository line");
}

#[test]
fn every_zig_source_is_pinned_by_content_and_goes_below_zig_pkgs() {
    let entries = sources();
    let mut dests = Vec::new();
    for entry in &entries {
        let url = entry["url"].as_str().expect("every entry has a url");
        assert!(url.starts_with("https://"), "only https addresses: {url}");
        let dest = entry["dest"].as_str().expect("every entry has a dest");
        let name = dest.strip_prefix("zig-pkgs/").unwrap_or_else(|| panic!("a dest below zig-pkgs/: {dest}"));
        assert!(
            !name.is_empty() && !name.contains('/') && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)),
            "the dest is one folder named by the package hash: {dest}"
        );
        match entry["type"].as_str() {
            Some("archive") => {
                let sha = entry["sha256"].as_str().unwrap_or_else(|| panic!("an archive without a sha256: {url}"));
                assert!(is_hex(sha, 64), "a sha256 is 64 lower-case hex digits: {url}: {sha}");
                assert!(matches!(entry["strip-components"].as_u64(), Some(0 | 1)), "strip-components as Zig unpacks: {url}");
            }
            Some("git") => {
                let commit = entry["commit"].as_str().unwrap_or_else(|| panic!("a git source without a commit: {url}"));
                assert!(is_hex(commit, 40), "a git source is pinned by a 40-character commit: {url}");
            }
            other => panic!("a source that is neither archive nor git: {other:?} ({url})"),
        }
        assert!(!dests.contains(&dest), "two sources share the dest {dest}");
        dests.push(dest);
    }
    assert!(!entries.is_empty(), "no Zig package is listed");
}

#[test]
fn the_derived_set_is_smaller_than_the_whole_list_of_ghostty() {
    let used = sources().len();
    assert!(used >= 1, "at least one package is used");
    assert!(
        used < GHOSTTY_LISTED_PACKAGES,
        "{used} sources are not fewer than the {GHOSTTY_LISTED_PACKAGES} packages of Ghostty's list: the set must be derived from a build, not copied"
    );
}

#[test]
fn the_tool_and_the_script_mode_say_that_the_files_are_generated_and_may_be_regenerated() {
    let head = |text: &str| text.lines().take_while(|l| l.starts_with('#')).collect::<Vec<_>>().join(" ").to_lowercase();
    let tool = head(TOOL);
    assert!(tool.contains("generated") && tool.contains("regenerate"), "the header of scripts/flatpak-zig-sources.sh: {tool}");
    assert!(tool.contains("not hand-written") || tool.contains("do not edit"), "the tool says the files are not hand-written");
    let script = head(SCRIPT);
    assert!(script.contains("--engine-offline"), "the header of scripts/build-flatpak.sh names the mode");
    assert!(script.contains("generated by scripts/flatpak-zig-sources.sh"), "the mode says who generates the files");
    assert!(script.contains("may regenerate"), "the mode says that the owner may regenerate them");
    // The pin file carries the same statement as its first lines.
    assert!(PIN.lines().next().is_some_and(|l| l.starts_with("# GENERATED")), "the pin file starts with the statement");
    // Network off for the build itself, and the only mode that cuts it says so.
    assert!(SCRIPT.contains("--network none"), "stage B runs with the network cut");
    assert!(SCRIPT.contains("flatpak-engine-offline: ok"), "the last line of a passed run");
}

/// The lines of the app command file that are not comments or empty: the commands.
fn command_lines() -> Vec<&'static str> {
    APP_COMMAND.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect()
}

/// The comment lines of the app command file, without the leading `# `.
fn comment_lines() -> Vec<&'static str> {
    APP_COMMAND.lines().filter_map(|l| l.strip_prefix('#')).map(str::trim).collect()
}

/// The words after `--features` in a cargo command line.
fn features(command: &str) -> Vec<String> {
    let words: Vec<&str> = command.split_whitespace().collect();
    words.windows(2).filter(|w| w[0] == "--features").map(|w| w[1].to_owned()).collect()
}

#[test]
fn the_app_command_is_one_plain_cargo_command_with_every_feature_of_the_cli_path() {
    let commands = command_lines();
    assert_eq!(commands.len(), 1, "one command line, the rest are comments: {commands:?}");
    let command = commands[0];
    assert!(command.starts_with("cargo build "), "a plain cargo build: {command}");
    let words: Vec<&str> = command.split_whitespace().collect();
    for flag in ["--release", "--locked", "--offline"] {
        assert!(words.contains(&flag), "the command lacks {flag}: {command}");
    }
    assert!(words.windows(2).any(|w| w == ["-p", "holzwave-app"]), "the command builds the app package: {command}");
    assert!(!command.contains("tauri build") && !command.contains("npm"), "no Tauri CLI and no npm in the command: {command}");
    // The CLI's own command is copied into the comments; every feature it passed is passed here too.
    let printed = comment_lines()
        .into_iter()
        .find_map(|l| l.strip_prefix("Command `").and_then(|c| c.strip_suffix('`')))
        .expect("a comment line with the cargo command that the CLI printed");
    assert!(printed.starts_with("cargo build "), "the CLI's command: {printed}");
    let wanted = features(printed);
    assert!(!wanted.is_empty(), "the CLI passed a feature: without one the build is a development build: {printed}");
    assert!(wanted.iter().any(|f| f == "tauri/custom-protocol"), "the feature that embeds the page: {printed}");
    let given = features(command);
    for feature in &wanted {
        assert!(given.contains(feature), "the plain command lacks the feature {feature} that the CLI path used: {command}");
    }
}

#[test]
fn the_app_command_file_says_which_command_the_cli_ran_and_what_it_needs() {
    let comments = comment_lines().join(" ");
    assert!(APP_COMMAND.starts_with("# GENERATED by scripts/build-flatpak.sh --app-cli-free"), "the file starts with the statement");
    assert!(comments.contains("may regenerate"), "the file says that the owner may regenerate it");
    assert!(comments.contains("tauri build --no-bundle -v -- --locked"), "the file names the CLI command that was run");
    assert!(comments.contains("copied from its verbose output"), "the file says where the CLI's command comes from");
    assert!(comments.contains("cargo:rustc-cfg=dev"), "the file says what shows that the build is not a development build");
    for name in ["CARGO_HOME", "GHOSTTY_SOURCE_DIR", "GHOSTTY_ZIG_SYSTEM_DIR", "ZIG"] {
        assert!(comments.contains(name), "the file does not name the environment variable {name}");
    }
    // The mode that writes it only reports success when no build is a development build.
    assert!(SCRIPT.contains("app-cli-free: ok") && SCRIPT.contains("cargo:rustc-cfg=dev"), "the script mode");
}

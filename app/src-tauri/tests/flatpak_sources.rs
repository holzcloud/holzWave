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

// ---- The Cargo and npm source files and the tool that writes them -----------------------------------------------------

const CARGO_SOURCES: &str = include_str!("../../../packaging/flatpak/generated/cargo-sources.json");
const NODE_SOURCES: &str = include_str!("../../../packaging/flatpak/generated/node-sources.json");
const GENERATOR_PIN: &str = include_str!("../../../packaging/flatpak/generated/generator-pin.txt");
const GENERATE_SCRIPT: &str = include_str!("../../../scripts/flatpak-generate-sources.sh");
const PACKAGE_LOCK: &str = include_str!("../../package-lock.json");

/// The commit of `flatpak/flatpak-builder-tools` that the owner approved, and the Python packages (the direct dependencies of its
/// two generators and their dependencies, as resolved when the owner approved them) at exactly these versions. A change of
/// this list is a new decision of the owner, not an edit of a test.
const APPROVED_COMMIT: &str = "9d86d336a39b9af38b86f114b6ce5820fe260f91";
const APPROVED_PACKAGES: [&str; 12] = [
    "aiohttp==3.14.5",
    "PyYAML==6.0.3",
    "tomlkit==0.15.2",
    "aiohappyeyeballs==2.7.1",
    "aiosignal==1.4.0",
    "attrs==26.1.0",
    "frozenlist==1.8.0",
    "multidict==7.2.1",
    "propcache==0.5.4",
    "yarl==1.25.1",
    "idna==3.20",
    "typing-extensions==4.16.0",
];

fn json_list(name: &str, text: &str) -> Vec<Value> {
    match serde_json::from_str::<Value>(text).unwrap_or_else(|e| panic!("{name} is not JSON: {e}")) {
        Value::Array(entries) => entries,
        other => panic!("{name} is not a list of sources: {other}"),
    }
}

/// A package of a lock file as the generated sources must carry it.
struct Locked {
    name: String,
    version: String,
    checksum: String,
}

/// The registry packages of a `Cargo.lock`: name, version and the checksum of the `.crate` file. Packages from a git address or
/// from the workspace carry no checksum and are not registry packages.
fn registry_crates(lock: &str) -> Vec<Locked> {
    let (mut crates, mut name, mut version, mut registry, mut checksum) = (Vec::new(), None, None, false, None);
    let mut flush = |name: &mut Option<String>, version: &mut Option<String>, registry: &mut bool, checksum: &mut Option<String>| {
        if let (Some(n), Some(v), true, Some(c)) = (name.take(), version.take(), *registry, checksum.take()) {
            crates.push(Locked { name: n, version: v, checksum: c });
        }
        *registry = false;
        *name = None;
        *version = None;
        *checksum = None;
    };
    let value = |line: &str, key: &str| line.strip_prefix(&format!("{key} = \"")).and_then(|v| v.strip_suffix('"')).map(str::to_owned);
    for line in lock.lines() {
        if line == "[[package]]" {
            flush(&mut name, &mut version, &mut registry, &mut checksum);
        } else if let Some(v) = value(line, "name") {
            name = Some(v);
        } else if let Some(v) = value(line, "version") {
            version = Some(v);
        } else if let Some(v) = value(line, "source") {
            registry = v.starts_with("registry+");
        } else if let Some(v) = value(line, "checksum") {
            checksum = Some(v);
        }
    }
    flush(&mut name, &mut version, &mut registry, &mut checksum);
    crates
}

/// The registry crates of `lock` that `sources` (the generated Cargo sources) does not carry as a hashed `.crate` archive with the
/// checksum of the lock file. Matched by name and version through the address crates.io serves the file from.
fn crates_without_source(lock: &str, sources: &[Value]) -> Vec<String> {
    registry_crates(lock)
        .into_iter()
        .filter(|c| {
            let url = format!("https://static.crates.io/crates/{0}/{0}-{1}.crate", c.name, c.version);
            !sources.iter().any(|s| s["url"] == url.as_str() && s["sha256"] == c.checksum.as_str())
        })
        .map(|c| format!("{} {}", c.name, c.version))
        .collect()
}

/// The `resolved` addresses of an npm lock file (text) that `sources` (the generated npm sources) does not fetch, and the
/// `integrity` values of the lock file that appear nowhere in the generated text. The address is matched whole: it names the
/// registry, the package and the version, so the tarball's file name is included.
fn npm_packages_without_source(lock: &str, sources_text: &str) -> Vec<String> {
    let lock: Value = serde_json::from_str(lock).expect("package-lock.json is JSON");
    let sources = json_list("node-sources.json", sources_text);
    let mut missing = Vec::new();
    for (path, package) in lock["packages"].as_object().expect("a packages table") {
        let Some(resolved) = package["resolved"].as_str() else { continue };
        if !sources.iter().any(|s| s["url"] == resolved) {
            missing.push(format!("{path}: {resolved}"));
        }
        if let Some(integrity) = package["integrity"].as_str() {
            if !sources_text.contains(integrity) {
                missing.push(format!("{path}: integrity {integrity}"));
            }
        }
    }
    missing
}

#[test]
fn every_registry_crate_of_cargo_lock_is_a_hashed_source_and_every_git_source_is_pinned() {
    let sources = json_list("cargo-sources.json", CARGO_SOURCES);
    assert!(!sources.is_empty(), "no Cargo source");
    for source in &sources {
        match source["type"].as_str() {
            Some("archive" | "file") => {
                let url = source["url"].as_str().unwrap_or_else(|| panic!("a source without a url: {source}"));
                assert!(url.starts_with("https://"), "only https addresses: {url}");
                let sha = source["sha256"].as_str().unwrap_or_else(|| panic!("a download without a sha256: {url}"));
                assert!(is_hex(sha, 64), "a sha256 is 64 lower-case hex digits: {url}: {sha}");
            }
            Some("git") => {
                let url = source["url"].as_str().unwrap_or_else(|| panic!("a git source without a url: {source}"));
                assert!(url.starts_with("https://"), "only https addresses: {url}");
                let commit = source["commit"].as_str().unwrap_or_else(|| panic!("a git source without a commit: {url}"));
                assert!(is_hex(commit, 40), "a git source is pinned by a 40-character commit: {url}");
            }
            // Text the tool writes (the checksum files cargo wants, the vendoring configuration) and the copy commands that put the
            // git checkout into the vendor folder: neither fetches anything.
            Some("inline" | "shell") => assert!(source.get("url").is_none(), "an inline or shell source with an address: {source}"),
            other => panic!("a source of an unexpected type {other:?}: {source}"),
        }
    }
    let missing = crates_without_source(CARGO_LOCK, &sources);
    assert!(missing.is_empty(), "Cargo.lock changed after cargo-sources.json was written; run scripts/flatpak-generate-sources.sh: {missing:?}");
    let locked = registry_crates(CARGO_LOCK).len();
    let downloads = sources.iter().filter(|s| s["type"] == "archive").count();
    assert_eq!(downloads, locked, "the sources hold {downloads} crate archives, Cargo.lock {locked} registry crates: a crate that left the lock file is still declared");
    // The git dependency of the lock file (the engine wrapper) is a git source at the same commit.
    let engine = setting(PIN, "libghostty-rs-revision").expect("the engine revision of the pin");
    assert!(
        sources.iter().any(|s| s["type"] == "git" && s["commit"] == engine.as_str()),
        "the engine wrapper is not a git source at the commit of Cargo.lock ({engine})"
    );
}

#[test]
fn every_resolved_address_of_the_npm_lock_file_is_a_hashed_source() {
    let sources = json_list("node-sources.json", NODE_SOURCES);
    assert!(!sources.is_empty(), "no npm source");
    for source in &sources {
        match source["type"].as_str() {
            Some("archive" | "file") => {
                let url = source["url"].as_str().unwrap_or_else(|| panic!("a download without a url: {source}"));
                assert!(url.starts_with("https://"), "only https addresses: {url}");
                let pinned = source["sha256"].as_str().is_some_and(|h| is_hex(h, 64)) || source["sha512"].as_str().is_some_and(|h| is_hex(h, 128));
                assert!(pinned, "a download without a sha256 or sha512: {url}");
            }
            Some("git") => assert!(source["commit"].as_str().is_some_and(|c| is_hex(c, 40)), "a git source without a commit: {source}"),
            Some("inline" | "script" | "shell") => assert!(source.get("url").is_none(), "an inline, script or shell source with an address: {source}"),
            other => panic!("a source of an unexpected type {other:?}: {source}"),
        }
    }
    let missing = npm_packages_without_source(PACKAGE_LOCK, NODE_SOURCES);
    assert!(missing.is_empty(), "app/package-lock.json changed after node-sources.json was written; run scripts/flatpak-generate-sources.sh: {missing:?}");
}

#[test]
fn the_generator_pin_names_the_approved_commit_and_exactly_the_approved_packages() {
    let commit = setting(GENERATOR_PIN, "flatpak-builder-tools-commit").expect("a flatpak-builder-tools-commit line");
    assert!(is_hex(&commit, 40), "the commit is 40 lower-case hex digits: {commit}");
    assert_eq!(commit, APPROVED_COMMIT, "the pinned commit is not the one the owner approved");
    assert_eq!(setting(GENERATOR_PIN, "flatpak-builder-tools-repository").as_deref(), Some("https://github.com/flatpak/flatpak-builder-tools.git"));
    let packages: Vec<&str> = GENERATOR_PIN.lines().filter_map(|l| l.strip_prefix("python-package=")).collect();
    assert_eq!(packages, APPROVED_PACKAGES, "the Python packages are not the approved list at the approved versions, in that order");
    // Every package has at least one hash line, and no hash line names another package.
    let hashed: Vec<&str> = GENERATOR_PIN.lines().filter_map(|l| l.strip_prefix("python-hash=")).collect();
    for line in &hashed {
        let mut parts = line.split(' ');
        let (name, hash) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        assert!(packages.iter().any(|p| p.split("==").next() == Some(name)), "a hash for a package that is not approved: {line}");
        assert!(hash.strip_prefix("sha256:").is_some_and(|h| is_hex(h, 64)), "a hash line without a sha256: {line}");
    }
    for package in &packages {
        let name = package.split("==").next().expect("a name");
        assert!(hashed.iter().any(|l| l.split(' ').next() == Some(name)), "no hash for {name}");
    }
    let locks: Vec<&str> = GENERATOR_PIN.lines().filter_map(|l| l.strip_prefix("lock-file=")).collect();
    assert_eq!(locks, ["Cargo.lock", "app/package-lock.json"], "the lock files the sources came from");
    assert!(setting(GENERATOR_PIN, "generated-on").is_some_and(|d| d.len() == 10 && d.as_bytes()[4] == b'-'), "a date line");
    assert!(
        GENERATOR_PIN.lines().any(|l| l == "These files are tool output; regenerate with scripts/flatpak-generate-sources.sh"),
        "the sentence that says the files are tool output"
    );
}

#[test]
fn the_generating_script_installs_only_the_approved_packages_inside_a_container_and_never_publishes() {
    // The script names no package of its own: the list comes from the pin file, which the test above pins to the approved one.
    let pips: Vec<&str> = GENERATE_SCRIPT.lines().filter(|l| l.contains("pip install")).collect();
    assert_eq!(pips.len(), 1, "one pip install, from the requirements built out of the pin file: {pips:?}");
    for word in ["--require-hashes", "--only-binary :all:", "--no-deps", "-r /tmp/requirements.txt"] {
        assert!(pips[0].contains(word), "the pip install lacks {word}: {}", pips[0]);
    }
    for word in ["pip install", "pipx", "poetry"] {
        let at: Vec<&str> = GENERATE_SCRIPT.lines().filter(|l| !l.trim_start().starts_with('#') && l.contains(word)).collect();
        assert!(word == "pip install" && at.len() == 1 || at.is_empty(), "the script uses {word} outside the one hashed install: {at:?}");
    }
    assert!(GENERATE_SCRIPT.contains("docker run --rm"), "the generators run in a throw-away container");
    assert!(GENERATE_SCRIPT.contains("python3 -m venv /venv"), "the packages go into a virtual environment of the container");
    assert!(GENERATE_SCRIPT.contains("git -C /tools rev-parse HEAD") && GENERATE_SCRIPT.contains("is not the pinned commit"), "the checkout is verified against the pin");
    assert!(GENERATE_SCRIPT.contains("flatpak-builder-tools") && GENERATE_SCRIPT.contains("generator-pin.txt"));
    assert!(GENERATE_SCRIPT.contains("-v \"$root\":/src:ro"), "the repository is mounted read-only");
    assert!(GENERATE_SCRIPT.contains("flatpak-sources: up to date") && GENERATE_SCRIPT.contains("--check"), "the drift check");
    for forbidden in [
        "--share=network",
        "git push",
        "git tag",
        "gh pr",
        "gh release",
        "flathub/flathub",
        "set -x",
        "sudo",
        "pip install --user",
        "--break-system-packages",
        "npm install",
        "cargo install",
    ] {
        assert!(!GENERATE_SCRIPT.contains(forbidden), "the script contains {forbidden}");
    }
    let head = GENERATE_SCRIPT.lines().take_while(|l| l.starts_with('#')).collect::<Vec<_>>().join(" ").to_lowercase();
    assert!(head.contains("tool output") && head.contains("never edit"), "the header says that the files are tool output");
    assert!(head.contains("needs the network"), "the header says that running the tool needs the network");
}

#[test]
fn a_lock_file_package_without_a_source_is_found() {
    // The in-memory examples of the two checks: the tests above are only as good as these.
    let sha = "a".repeat(64);
    let lock = format!(
        "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{sha}\"\n\n\
         [[package]]\nname = \"holzwave-app\"\nversion = \"0.0.1\"\n\n\
         [[package]]\nname = \"engine\"\nversion = \"0.2.1\"\nsource = \"git+https://github.com/example/engine?rev=abc#abc\"\n"
    );
    assert_eq!(registry_crates(&lock).len(), 1, "only the registry package counts, not the workspace package or the git package");
    let source = |name: &str, version: &str, sha: &str| {
        serde_json::json!({"type": "archive", "url": format!("https://static.crates.io/crates/{name}/{name}-{version}.crate"), "sha256": sha})
    };
    assert!(crates_without_source(&lock, &[source("serde", "1.0.0", &sha)]).is_empty(), "a complete source file");
    assert_eq!(crates_without_source(&lock, &[]), ["serde 1.0.0"], "a crate with no source");
    assert_eq!(crates_without_source(&lock, &[source("serde", "1.0.1", &sha)]), ["serde 1.0.0"], "a crate whose version moved");
    assert_eq!(crates_without_source(&lock, &[source("serde", "1.0.0", &"b".repeat(64))]), ["serde 1.0.0"], "a crate whose checksum changed");
    let lock = format!("{lock}\n[[package]]\nname = \"rand\"\nversion = \"0.9.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{sha}\"\n");
    assert_eq!(crates_without_source(&lock, &[source("serde", "1.0.0", &sha)]), ["rand 0.9.0"], "a crate added to the lock file only");

    let npm_lock = |extra: &str| {
        format!(
            "{{\"lockfileVersion\": 3, \"packages\": {{\"\": {{\"name\": \"app\"}}, \"node_modules/a\": {{\"resolved\": \"https://registry.npmjs.org/a/-/a-1.0.0.tgz\", \"integrity\": \"sha512-AAAA\"}}{extra}}}}}"
        )
    };
    let sources = "[{\"type\": \"file\", \"url\": \"https://registry.npmjs.org/a/-/a-1.0.0.tgz\", \"sha512\": \"00\"}, {\"type\": \"inline\", \"contents\": \"{\\\"integrity\\\": \\\"sha512-AAAA\\\"}\"}]";
    assert!(npm_packages_without_source(&npm_lock(""), sources).is_empty(), "a complete source file");
    let extra = ", \"node_modules/b\": {\"resolved\": \"https://registry.npmjs.org/b/-/b-2.0.0.tgz\", \"integrity\": \"sha512-BBBB\"}";
    assert_eq!(npm_packages_without_source(&npm_lock(extra), sources).len(), 2, "a package added to the lock file only: no address and no integrity");
    let moved = "[{\"type\": \"file\", \"url\": \"https://registry.npmjs.org/a/-/a-1.0.1.tgz\"}, {\"contents\": \"sha512-AAAA\"}]";
    assert_eq!(npm_packages_without_source(&npm_lock(""), moved).len(), 1, "a package whose version moved");
}

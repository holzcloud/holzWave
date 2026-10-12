//! The guard of DESK-26 (F33: no telemetry and no account) and of the dependency budget (D-03, D-04).
//!
//! A negative claim such as "nothing phones home" needs a test that fails when someone adds the wrong crate or relaxes
//! the policy. This file does that with std only (no crate is added for it, D-03): it reads `Cargo.lock`, the
//! manifests, `tauri.conf.json`, `app/package.json` and the first-party sources as text, from the workspace root that
//! `CARGO_MANIFEST_DIR` points into.
//!
//! What is pinned:
//!
//! * the direct dependency list of every workspace crate and of the app package (a new dependency fails the test with
//!   its name, which forces a conscious D-03 decision in this file);
//! * the dependency closure of the app: no telemetry or network client crate, except the named, commented exceptions
//!   with their exact set of parents;
//! * the sources: no network API in first-party Rust (non-test code) or in the page;
//! * the page policy (no remote origin in the CSP, no dev server URL, no updater) and the absence of CI (D-04);
//! * the examples of `docs/config.md`, which are parsed by the real loaders, so the document cannot drift from the code;
//! * the owner's Windows and Linux list: every row W1 to W13 and L1 to L10 is in `docs/test-plan.md` exactly once.
//!
//! The capability allow-list and the exact CSP are pinned by `app/src-tauri/tests/acl.rs`; this file does not repeat
//! them, it only re-checks the properties that DESK-26 depends on.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use holzwave_core::keybindings::{Keybindings, actions_for};
use holzwave_core::layout::{Layout, LoadStatus};
use holzwave_core::session::Session;
use holzwave_core::settings::Settings;
use holzwave_core::theme::{ThemeRegistry, blend, contrast};

// ---- the policy -----------------------------------------------------------------------------------------

/// The direct dependencies of the workspace crates, as `Cargo.lock` lists them (normal, build and optional ones).
///
/// Adding a crate here is the D-03 decision: say in the plan why std and the platform are not enough.
const WORKSPACE_ALLOWLIST: [(&str, &[&str]); 3] = [
    ("holzwave-core", &["libghostty-vt", "portable-pty", "serde", "serde_json"]),
    ("holzwave-app", &["holzwave-core", "serde", "serde_json", "tauri", "tauri-build", "tauri-runtime-wry"]),
    ("holzwave-spike", &["holzwave-core"]),
];

/// The only git repository the lock file may fetch from: the terminal engine, pinned by revision.
const ENGINE_REPOSITORY: &str = "https://github.com/Uzaaft/libghostty-rs";

/// The dev dependencies of the app package. It has no `dependencies` at all: the page ships no third-party code.
const NPM_DEV_DEPENDENCIES: [&str; 3] = ["@tauri-apps/cli", "playwright-core", "typescript"];

/// Crates that talk to a network or report somewhere. None of them may be in the dependency closure of the app
/// unless it is named in [`EXCEPTIONS`].
const DENIED_CRATES: [&str; 14] = [
    "reqwest",
    "hyper-util",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "sentry",
    "sentry-core",
    "posthog-rs",
    "opentelemetry",
    "tauri-plugin-updater",
    "tauri-plugin-http",
    "tauri-plugin-opener",
];

/// Name prefixes that are denied as well: any sentry or opentelemetry crate, and any Tauri plugin (the app uses none,
/// so a plugin in the closure is a decision, not an accident).
const DENIED_PREFIXES: [&str; 3] = ["sentry", "opentelemetry", "tauri-plugin-"];

/// A denied crate that is in the closure anyway: its name, the exact set of packages allowed to depend on it, and why.
type Exception = (&'static str, &'static [&'static str], &'static str);

/// Honest evidence, not a weakened list. `tauri` 3.0.0-alpha.4 lists `reqwest` as a plain dependency (`default-features
/// = false`, features `json` and `stream`: no TLS backend is selected, so it cannot even speak https). Its only use is
/// `protocol/tauri.rs`, the proxy to a dev server URL, which `manager/webview.rs` switches on with
/// `PROXY_DEV_SERVER = cfg!(all(dev, mobile))`: a desktop build never creates the client, and `tauri.conf.json`
/// sets no `devUrl` (checked by `page_policy_has_no_remote_origin`). `hyper-util` is below `reqwest` only.
const EXCEPTIONS: [Exception; 2] = [
    ("reqwest", &["tauri"], "tauri's dev-server proxy; not instantiated on desktop and devUrl is unset"),
    ("hyper-util", &["reqwest"], "the HTTP client of reqwest, reachable only through it"),
];

/// Hits of the network scan that are named and explained. The platform opener starts `open`, `xdg-open` or `explorer`
/// through `std::process::Command` and holds no URL literal, so nothing is permitted today: `(file, token, reason)`.
const PERMITTED_HITS: [(&str, &str, &str); 0] = [];

/// Network APIs and clients that must not appear in non-test first-party Rust code.
const RUST_NETWORK_TOKENS: [&str; 10] = [
    "std::net",
    "TcpStream",
    "TcpListener",
    "UdpSocket",
    "reqwest",
    "hyper",
    "ureq",
    "curl",
    "http://",
    "https://",
];

/// Network APIs that must not appear in the page: `fetch(` and friends, and any absolute http(s) URL.
const PAGE_NETWORK_TOKENS: [&str; 7] =
    ["fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "sendBeacon", "http://", "https://"];

/// Build and CI configurations that must not exist (D-04: local only, no GitHub Actions).
const CI_PATHS: [&str; 6] = [
    ".github/workflows",
    ".circleci",
    ".gitlab-ci.yml",
    ".travis.yml",
    "azure-pipelines.yml",
    "Jenkinsfile",
];

// ---- reading ----------------------------------------------------------------------------------------------

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

/// Every file below `dir` (relative to the workspace root) with one of `extensions`, sorted.
fn files_below(dir: &str, extensions: &[&str]) -> Vec<PathBuf> {
    fn walk(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, extensions, out);
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.contains(&e)) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&root().join(dir), extensions, &mut out);
    out.sort();
    out
}

fn shown(path: &Path) -> String {
    path.strip_prefix(root()).unwrap_or(path).display().to_string()
}

// ---- Cargo.lock -------------------------------------------------------------------------------------------

/// Package name to the names of the packages it lists as dependencies, merged over all versions of the name.
type Graph = BTreeMap<String, BTreeSet<String>>;

/// The `[[package]]` blocks of a lock file: names and `dependencies` lists. A dependency entry is `"name"`,
/// `"name version"` or `"name version (source)"`; only the name matters here.
fn parse_lock(text: &str) -> Graph {
    let mut graph = Graph::new();
    let mut current: Option<String> = None;
    let mut in_dependencies = false;
    for line in text.lines() {
        if line == "[[package]]" {
            current = None;
            in_dependencies = false;
        } else if in_dependencies {
            if line.trim() == "]" {
                in_dependencies = false;
            } else if let Some(owner) = &current {
                let entry = line.trim().trim_end_matches(',').trim_matches('"');
                if let Some(name) = entry.split_whitespace().next() {
                    graph.entry(owner.clone()).or_default().insert(name.to_owned());
                }
            }
        } else if let Some(rest) = line.strip_prefix("name = \"") {
            let name = rest.trim_end_matches('"').to_owned();
            graph.entry(name.clone()).or_default();
            current = Some(name);
        } else if line == "dependencies = [" {
            in_dependencies = true;
        }
    }
    graph
}

fn lock_graph() -> Graph {
    let graph = parse_lock(&read("Cargo.lock"));
    assert!(graph.len() > 100, "only {} packages parsed from Cargo.lock: is the parser broken?", graph.len());
    graph
}

/// `start` and everything reachable from it.
fn closure(graph: &Graph, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![start.to_owned()];
    while let Some(name) = todo.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(dependencies) = graph.get(&name) {
            todo.extend(dependencies.iter().cloned());
        }
    }
    seen
}

/// The packages of the lock file that list `name` as a dependency.
fn parents_of(graph: &Graph, name: &str) -> BTreeSet<String> {
    graph.iter().filter(|(_, deps)| deps.contains(name)).map(|(owner, _)| owner.clone()).collect()
}

fn is_denied(name: &str) -> bool {
    DENIED_CRATES.contains(&name) || DENIED_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// The repositories (without revision) of all `git+` sources of a lock file.
fn git_repositories(lock: &str) -> BTreeSet<String> {
    lock.lines()
        .filter_map(|line| line.strip_prefix("source = \"git+"))
        .map(|rest| rest.split(['?', '#', '"']).next().unwrap_or_default().to_owned())
        .collect()
}

// ---- source scanning --------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Syntax {
    Rust,
    Script,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// `source` with every comment replaced by spaces (newlines are kept, so line numbers stay) and string literals left
/// as they are. Rust: nested block comments, raw strings, char literals against lifetimes. Script (TypeScript, HTML,
/// CSS): block comments do not nest, `'` and `"` strings end at a line break, template strings may span lines.
fn without_comments(source: &str, syntax: Syntax) -> String {
    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < len {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < len && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while i < len {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') && (syntax == Syntax::Rust || depth == 0) {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth = depth.saturating_sub(1);
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
        } else if syntax == Syntax::Rust && c == 'r' && (i == 0 || !is_word(chars[i - 1])) && raw_string_end(&chars, i).is_some()
        {
            let end = raw_string_end(&chars, i).unwrap_or(len);
            out.extend(&chars[i..end]);
            i = end;
        } else if syntax == Syntax::Rust && c == '\'' {
            // `'\n'`, `'"'` and `'\u{1F600}'` are char literals, `'a` is a lifetime or a label.
            let literal_end = if next == Some('\\') {
                (i + 3..(i + 14).min(len)).find(|&j| chars[j] == '\'').map(|j| j + 1)
            } else if chars.get(i + 2) == Some(&'\'') {
                Some(i + 3)
            } else {
                None
            };
            let end = literal_end.unwrap_or(i + 1);
            out.extend(&chars[i..end]);
            i = end;
        } else if c == '"' || (syntax == Syntax::Script && (c == '\'' || c == '`')) {
            out.push(c);
            i += 1;
            while i < len {
                let d = chars[i];
                out.push(d);
                i += 1;
                if d == '\\' && i < len {
                    out.push(chars[i]);
                    i += 1;
                } else if d == c || (d == '\n' && syntax == Syntax::Script && c != '`') {
                    break;
                }
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The index just past a raw string literal (`r"..."`, `r#"..."#`) that starts at `start`, or `None` if none does.
fn raw_string_end(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start + 1;
    let mut hashes = 0;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    while j < chars.len() {
        if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
            return Some(j + 1 + hashes);
        }
        j += 1;
    }
    Some(chars.len())
}

/// The part of a Rust file before its test module: everything that ships.
fn production_part(source: &str) -> &str {
    source.split("\n#[cfg(test)]\nmod ").next().unwrap_or(source)
}

/// Whether `line` holds `token` with a word boundary on every side where the token itself ends in a word character
/// (`hyper` must not match `hyperlink`, `curl` not `curly`; `https://` is followed by anything).
fn contains_token(line: &str, token: &str) -> bool {
    let first_is_word = token.chars().next().is_some_and(is_word);
    let last_is_word = token.chars().next_back().is_some_and(is_word);
    let mut from = 0;
    while let Some(found) = line[from..].find(token) {
        let start = from + found;
        let end = start + token.len();
        let before_ok = !first_is_word || line[..start].chars().next_back().is_none_or(|c| !is_word(c));
        let after_ok = !last_is_word || line[end..].chars().next().is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            return true;
        }
        from = start + token.len().max(1);
    }
    false
}

/// `(line, token)` for every token found in the code lines of `code` (comments already removed).
fn token_hits(code: &str, tokens: &[&'static str]) -> Vec<(usize, &'static str)> {
    let mut hits = Vec::new();
    for (index, line) in code.lines().enumerate() {
        for token in tokens {
            if contains_token(line, token) {
                hits.push((index + 1, *token));
            }
        }
    }
    hits
}

/// The network hits of one Rust source text: comments and the test module are not code that ships.
fn rust_network_hits(source: &str) -> Vec<(usize, &'static str)> {
    token_hits(&without_comments(production_part(source), Syntax::Rust), &RUST_NETWORK_TOKENS)
}

/// The hits of one page file. TypeScript loses its comments; HTML and CSS are scanned as they are, because an unquoted
/// `url(https://...)` or `href=https://...` holds `//` that is not a comment there, and a hit in a comment of those
/// files is only a reason to reword it.
fn page_network_hits(source: &str, is_script: bool) -> Vec<(usize, &'static str)> {
    let code = if is_script { without_comments(source, Syntax::Script) } else { source.to_owned() };
    token_hits(&code, &PAGE_NETWORK_TOKENS)
}

fn is_permitted(file: &str, token: &str) -> bool {
    PERMITTED_HITS.iter().any(|(f, t, _)| *f == file && *t == token)
}

// ---- dependencies -----------------------------------------------------------------------------------------

#[test]
fn workspace_crates_keep_their_dependency_allowlist() {
    let graph = lock_graph();
    let mut problems = Vec::new();

    let members: BTreeSet<String> = graph.keys().filter(|name| name.starts_with("holzwave-")).cloned().collect();
    let expected_members: BTreeSet<String> = WORKSPACE_ALLOWLIST.iter().map(|(name, _)| (*name).to_owned()).collect();
    if members != expected_members {
        problems.push(format!("the workspace crates are {members:?}, the allowlist names {expected_members:?}"));
    }

    for (crate_name, allowed) in WORKSPACE_ALLOWLIST {
        let Some(actual) = graph.get(crate_name) else {
            problems.push(format!("{crate_name} is not in Cargo.lock"));
            continue;
        };
        let allowed = set(allowed);
        for added in actual.difference(&allowed) {
            problems.push(format!(
                "{crate_name} gained the direct dependency {added}: D-03 asks for as few dependencies as possible; if \
                 it is wanted, name it in WORKSPACE_ALLOWLIST of this test with the reason in the plan"
            ));
        }
        for gone in allowed.difference(actual) {
            problems.push(format!("{crate_name} no longer depends on {gone}: remove it from WORKSPACE_ALLOWLIST"));
        }
    }
    assert!(problems.is_empty(), "the dependency allowlist is out of step:\n  {}", problems.join("\n  "));
}

#[test]
fn the_lock_file_fetches_from_crates_io_and_one_pinned_engine_repository_only() {
    let lock = read("Cargo.lock");
    assert_eq!(git_repositories(&lock), set(&[ENGINE_REPOSITORY]), "a git dependency other than the engine appeared");
    for line in lock.lines().filter(|l| l.starts_with("source = ")) {
        let ok = line.starts_with("source = \"registry+https://github.com/rust-lang/crates.io-index\"")
            || line.starts_with(&format!("source = \"git+{ENGINE_REPOSITORY}?rev="));
        assert!(ok, "an unexpected package source: {line}");
    }
}

#[test]
fn app_closure_contains_no_telemetry_or_network_client_crates() {
    let graph = lock_graph();
    let closure = closure(&graph, "holzwave-app");
    assert!(closure.contains("tauri") && closure.contains("holzwave-core"), "the walk did not reach the dependencies");
    let mut problems = Vec::new();
    for name in closure.iter().filter(|name| is_denied(name)) {
        let parents = parents_of(&graph, name);
        match EXCEPTIONS.iter().find(|(exception, _, _)| exception == name) {
            None => problems.push(format!(
                "{name} is in the dependency closure of holzwave-app and denied (parents: {parents:?}): DESK-26 says \
                 nothing phones home; remove it, or record a commented exception in EXCEPTIONS with its exact parents"
            )),
            Some((_, allowed, reason)) if parents != set(allowed) => problems.push(format!(
                "{name} ({reason}) has the parents {parents:?}, the exception allows exactly {allowed:?}"
            )),
            Some(_) => {}
        }
    }
    assert!(problems.is_empty(), "the telemetry and network guard failed:\n  {}", problems.join("\n  "));
}

#[test]
fn reqwest_parents_are_the_documented_set() {
    // tauri links reqwest for its dev-server proxy (see EXCEPTIONS). The set of crates that depend on it must stay
    // exactly {tauri}: a second parent means another crate brought an HTTP client in.
    let graph = lock_graph();
    let parents = parents_of(&graph, "reqwest");
    if !graph.contains_key("reqwest") {
        assert!(parents.is_empty(), "reqwest is absent from the lock but listed as a dependency of {parents:?}");
        return;
    }
    assert_eq!(parents, set(&["tauri"]), "the parents of reqwest changed");
    // hyper-util is the HTTP client below reqwest: nothing else may reach it.
    assert_eq!(parents_of(&graph, "hyper-util"), set(&["reqwest"]), "hyper-util has a parent besides reqwest");
}

#[test]
fn npm_package_has_only_the_expected_dev_dependencies() {
    let package: serde_json::Value = serde_json::from_str(&read("app/package.json")).expect("package.json is JSON");
    let dev: BTreeSet<String> = package
        .get("devDependencies")
        .and_then(|d| d.as_object())
        .expect("package.json has devDependencies")
        .keys()
        .cloned()
        .collect();
    assert_eq!(dev, set(&NPM_DEV_DEPENDENCIES), "the dev dependencies of the app package changed (D-03)");

    // The page ships no third-party code: no runtime dependency of any kind.
    for key in ["dependencies", "optionalDependencies", "peerDependencies", "bundledDependencies", "bundleDependencies"] {
        let empty = match package.get(key) {
            None => true,
            Some(value) => {
                value.as_object().is_some_and(|o| o.is_empty()) || value.as_array().is_some_and(|a| a.is_empty())
            }
        };
        assert!(empty, "package.json has {key}: {:?}", package.get(key));
    }

    // Install hooks run code of whoever published a dependency; there are none to run.
    let scripts = package.get("scripts").and_then(|s| s.as_object()).expect("package.json has scripts");
    for hook in ["preinstall", "install", "postinstall", "prepare", "prepublish", "prepublishOnly"] {
        assert!(!scripts.contains_key(hook), "package.json has the install hook {hook}");
    }
}

// ---- the page policy and CI -------------------------------------------------------------------------------

#[test]
fn page_policy_has_no_remote_origin() {
    let conf: serde_json::Value = serde_json::from_str(&read("app/src-tauri/tauri.conf.json")).expect("conf is JSON");
    let csp = conf["app"]["security"]["csp"].as_str().expect("tauri.conf.json sets a CSP");

    let directives: BTreeMap<&str, Vec<&str>> = csp
        .split(';')
        .filter_map(|directive| {
            let mut parts = directive.split_whitespace();
            Some((parts.next()?, parts.collect()))
        })
        .collect();
    assert_eq!(directives.get("default-src"), Some(&vec!["'self'"]), "default-src must be 'self' alone");

    // The IPC host of the Tauri bridge is the one origin that may appear, and only to connect to.
    for (directive, sources) in &directives {
        for source in sources {
            let remote = source.contains("://") || source.starts_with("http:") || source.starts_with("https:");
            if remote {
                assert!(
                    *directive == "connect-src" && *source == "http://ipc.localhost",
                    "{directive} lists the remote origin {source}"
                );
            }
            for scheme in ["ws:", "wss:", "*", "data:", "blob:", "'unsafe-inline'", "'unsafe-eval'"] {
                assert_ne!(*source, scheme, "{directive} lists {scheme}");
            }
        }
    }

    // The dev-server proxy (and with it the HTTP client of tauri) is only used with a devUrl; there is none, and no
    // window loads a remote page and no updater is configured.
    assert!(conf["build"].get("devUrl").is_none(), "tauri.conf.json sets build.devUrl");
    for window in conf["app"]["windows"].as_array().expect("conf lists windows") {
        let url = window.get("url").and_then(|u| u.as_str()).unwrap_or("index.html");
        assert!(!url.contains("://"), "a window loads the remote page {url}");
    }
    let lowered = read("app/src-tauri/tauri.conf.json").to_lowercase();
    assert!(!lowered.contains("updater"), "tauri.conf.json mentions an updater");
    assert!(!lowered.contains("endpoints"), "tauri.conf.json lists update endpoints");
}

#[test]
fn no_ci_workflows_exist() {
    for relative in CI_PATHS {
        assert!(!root().join(relative).exists(), "{relative} exists: D-04 says local builds and tests only");
    }
}

// ---- first-party sources ----------------------------------------------------------------------------------

#[test]
fn first_party_sources_use_no_network_apis() {
    let mut rust_files: Vec<PathBuf> = ["core/src", "app/src-tauri/src", "spike/src"]
        .iter()
        .flat_map(|dir| files_below(dir, &["rs"]))
        .collect();
    rust_files.push(root().join("app/src-tauri/build.rs"));
    assert!(rust_files.len() >= 15, "only {} Rust files found: is the walk broken?", rust_files.len());

    let mut page_files = files_below("app/src", &["ts", "html", "css"]);
    assert!(page_files.len() >= 20, "only {} page files found: is the walk broken?", page_files.len());
    page_files.sort();

    let mut problems = Vec::new();
    for file in &rust_files {
        let relative = shown(file);
        let source = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {relative}: {e}"));
        for (line, token) in rust_network_hits(&source) {
            if !is_permitted(&relative, token) {
                problems.push(format!("{relative}:{line}: {token}"));
            }
        }
    }
    for file in &page_files {
        let relative = shown(file);
        let source = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {relative}: {e}"));
        for (line, token) in page_network_hits(&source, relative.ends_with(".ts")) {
            if !is_permitted(&relative, token) {
                problems.push(format!("{relative}:{line}: {token}"));
            }
        }
        if relative.ends_with(".html") {
            for attribute in ["src", "href", "action"] {
                if source.contains(&format!("{attribute}=\"//")) {
                    problems.push(format!("{relative}: a protocol-relative {attribute} URL"));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "a network API or URL appeared in first-party code (DESK-26); name it in PERMITTED_HITS only with a reason:\n  {}",
        problems.join("\n  ")
    );
}

// ---- the scanners can fail --------------------------------------------------------------------------------

#[test]
fn the_rust_scanner_flags_code_and_skips_comments_tests_and_words() {
    let source = [
        "use std::net::TcpStream;",
        "// reqwest is only mentioned here",
        "/// A doc comment with https://example.org in it.",
        "/* hyper and curl */ let hyperlink = \"curly\";",
        "let url = \"https://example.org\"; // trailing comment with ureq",
        "let quote = '\"'; let lifetime: &'static str = r#\"UdpSocket \"quoted\"\"#;",
        "",
        "#[cfg(test)]",
        "mod tests {",
        "    use std::net::TcpListener;",
        "}",
    ]
    .join("\n");

    let hits = rust_network_hits(&source);

    assert_eq!(
        hits,
        [(1, "std::net"), (1, "TcpStream"), (5, "https://"), (6, "UdpSocket")],
        "a comment, a longer word or the test module was flagged, or a real hit was missed"
    );
}

#[test]
fn the_rust_scanner_keeps_line_numbers_across_block_comments() {
    let source = "fn a() {}\n/* one\n   two\n   /* nested */\n   three */\nuse std::net::UdpSocket;\n";

    assert_eq!(rust_network_hits(source), [(6, "std::net"), (6, "UdpSocket")]);
}

#[test]
fn the_page_scanner_flags_network_apis_and_absolute_urls_and_skips_comments() {
    let source = [
        "const a = await fetch(\"x\");",
        "// new WebSocket(\"wss://example.org\")",
        "const b = refetch(1); /* sendBeacon */",
        "const c = new XMLHttpRequest();",
        "const d = `${EventSource}`;",
        "<script src=\"https://cdn.example.org/a.js\"></script>",
        "const e = 'it\\'s a string with http://example.org';",
    ]
    .join("\n");

    let hits = page_network_hits(&source, true);

    assert_eq!(
        hits,
        [(1, "fetch("), (4, "XMLHttpRequest"), (5, "EventSource"), (6, "https://"), (7, "http://")],
        "a comment or a longer word was flagged, or a real hit was missed"
    );
}

#[test]
fn the_css_and_html_scan_sees_an_unquoted_url() {
    assert_eq!(page_network_hits("a { background: url(https://example.org/x.png); }", false), [(1, "https://")]);
    assert_eq!(page_network_hits("<link rel=stylesheet href=http://example.org/a.css>", false), [(1, "http://")]);
    assert!(page_network_hits("a { background: url(x.png); } /* local */", false).is_empty());
}

#[test]
fn the_lock_parser_reads_versions_sources_and_the_closure() {
    let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"1.0.0\"\ndependencies = [\n \"a\",\n \"b 0.2.0\",\n \
                \"c 0.3.0 (registry+https://example.org)\",\n]\n\n[[package]]\nname = \"a\"\nversion = \"1.0.0\"\n\
                source = \"git+https://example.org/x?rev=1#1\"\ndependencies = [\n \"b 0.1.0\",\n]\n\n[[package]]\n\
                name = \"b\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"b\"\nversion = \"0.2.0\"\n\n[[package]]\n\
                name = \"c\"\nversion = \"0.3.0\"\n\n[[package]]\nname = \"unused\"\nversion = \"1.0.0\"\n\
                dependencies = [\n \"c\",\n]\n";

    let graph = parse_lock(lock);

    assert_eq!(graph["app"], set(&["a", "b", "c"]));
    assert_eq!(closure(&graph, "app"), set(&["app", "a", "b", "c"]));
    assert_eq!(parents_of(&graph, "c"), set(&["app", "unused"]));
    assert_eq!(git_repositories(lock), set(&["https://example.org/x"]));
}

#[test]
fn the_denylist_matches_names_and_prefixes() {
    for name in ["reqwest", "ureq", "sentry-tracing", "opentelemetry-sdk", "tauri-plugin-shell", "tauri-plugin-http"] {
        assert!(is_denied(name), "{name} should be denied");
    }
    for name in ["tauri", "tauri-runtime-wry", "serde", "hyperlink", "portable-pty", "holzwave-core"] {
        assert!(!is_denied(name), "{name} should be allowed");
    }
}

// ---- docs/config.md ---------------------------------------------------------------------------------------

/// The JSON of the fenced block that follows the marker `<!-- example: NAME -->` in `docs/config.md`.
fn documented_example(name: &str) -> String {
    let doc = read("docs/config.md");
    let marker = format!("<!-- example: {name} -->");
    let after = doc.split_once(&marker).unwrap_or_else(|| panic!("docs/config.md has no example {name}")).1;
    let fence = after.trim_start().strip_prefix("```json\n").unwrap_or_else(|| panic!("no json fence after {name}"));
    fence.split_once("\n```").unwrap_or_else(|| panic!("the fence of {name} is not closed")).0.to_owned()
}

#[test]
fn the_examples_of_config_md_load_cleanly_through_the_real_loaders() {
    let (settings, status) = Settings::from_json(&documented_example("settings.json"));
    assert_eq!(status, LoadStatus::Clean, "the settings example is not clean");
    assert_eq!(settings.theme, "Schiefer");
    assert_eq!(settings.keybindings.len(), 2);
    let (_, warnings) = Keybindings::resolve(&settings.keybindings);
    assert!(warnings.is_empty(), "the key binding example is refused: {warnings:?}");

    let dir = common::scratch_dir("policy-docs");
    let themes = dir.join("themes");
    fs::create_dir(&themes).expect("themes directory");
    fs::write(themes.join("moos.json"), documented_example("themes/moos.json")).expect("theme file");
    let (registry, warnings) = ThemeRegistry::load(&dir);
    fs::remove_dir_all(&dir).expect("remove scratch directory");
    assert!(warnings.is_empty(), "the theme example is skipped: {warnings:?}");
    assert!(registry.contains("Moos"), "the theme example is not listed");

    // The contrast guidance of the document, held for the example it carries.
    let theme = registry.get("Moos");
    let (t, c) = (&theme.terminal, &theme.chrome);
    let selected = blend(t.selection, t.selection_alpha, t.background);
    assert!(contrast(t.foreground, t.background) >= 7.0);
    assert!(contrast(t.foreground, selected) >= 3.0);
    assert!(contrast(t.ansi[8], t.background) >= 3.0 && contrast(t.cursor, t.background) >= 3.0);
    for i in (1..=7).chain(9..=15) {
        assert!(contrast(t.ansi[i], t.background) >= 4.5, "ANSI {i} on the background");
        assert!(contrast(t.ansi[i], selected) >= 3.0, "ANSI {i} on the selection");
    }
    assert!(contrast(c.ink, c.ground) >= 7.0 && contrast(c.ink_3, c.ground) >= 4.5);

    let (session, status) = Session::from_json(&documented_example("session.json"));
    assert_eq!(status, LoadStatus::Clean, "the session example is not clean");
    assert_eq!(session.cwds.len(), 1);
    assert!(session.window.is_some());

    let (layout, status) = Layout::from_json(&documented_example("layout.json"));
    assert_eq!(status, LoadStatus::Clean, "the layout example is not clean");
    assert_eq!(layout.all_panes().len(), 2);
}

#[test]
fn config_md_names_every_setting_and_every_action() {
    let doc = read("docs/config.md");
    let defaults = serde_json::to_value(Settings::default()).expect("settings serialise");
    for key in defaults.as_object().expect("settings are an object").keys() {
        assert!(doc.contains(&format!("`{key}`")), "docs/config.md does not document the setting {key}");
    }
    for os in ["macos", "windows", "linux"] {
        for action in actions_for(os) {
            // Tab and workspace selection are one row each: `tab.select_1` to `_9`.
            let numbered =
                action.id.rsplit_once('_').is_some_and(|(_, n)| matches!(n, "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
            if numbered {
                continue;
            }
            let id = format!("`{}`", action.id);
            assert!(doc.contains(&id), "docs/config.md does not list the action {}", action.id);
            if let Some(default) = action.default.filter(|_| !action.id.contains("select_")) {
                // The row of the action names the default of every platform, so the two are on one line.
                let default = format!("`{default}`");
                assert!(
                    doc.lines().any(|line| line.contains(&id) && line.contains(&default)),
                    "docs/config.md does not list {default} for {} on {os}",
                    action.id
                );
            }
        }
    }
}

// ---- docs/test-plan.md ------------------------------------------------------------------------------------

/// The cells of a Markdown table row, split at every `|` that is not escaped as `\|`, with the outer bars dropped and
/// each cell trimmed. `None` for a line that is not a table row.
fn table_cells(line: &str) -> Option<Vec<String>> {
    let line = line.trim();
    let inner = line.strip_prefix('|')?;
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut previous = ' ';
    for c in inner.chars() {
        if c == '|' && previous != '\\' {
            cells.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(c);
        }
        previous = c;
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

/// What is wrong with the rows `ids` in the document `doc`, one sentence each: a row that is missing, one that appears
/// twice, one without its plan pointer, its instruction or its expected result. A row is a table row whose first cell
/// is exactly the id. The Result column (the fifth cell) is never looked at: it is the owner's to fill in.
fn test_plan_row_problems(doc: &str, ids: &[String]) -> Vec<String> {
    let rows: Vec<Vec<String>> = doc.lines().filter_map(table_cells).collect();
    let mut problems = Vec::new();
    for id in ids {
        let found: Vec<&Vec<String>> = rows.iter().filter(|cells| cells.first().is_some_and(|c| c == id)).collect();
        match found.as_slice() {
            [] => problems.push(format!("row {id} is missing")),
            [row] => {
                // Columns: #, Plan, What to do, Expected, Result.
                if row.len() < 5 {
                    problems.push(format!("row {id} has {} cells, the table has five columns", row.len()));
                } else if row[1..4].iter().any(String::is_empty) {
                    problems.push(format!("row {id} has an empty Plan, What to do or Expected cell"));
                }
            }
            more => problems.push(format!("row {id} appears {} times", more.len())),
        }
    }
    problems
}

fn windows_and_linux_row_ids() -> Vec<String> {
    (1..=13).map(|n| format!("W{n}")).chain((1..=10).map(|n| format!("L{n}"))).collect()
}

#[test]
fn test_plan_has_every_windows_and_linux_row_exactly_once() {
    let problems = test_plan_row_problems(&read("docs/test-plan.md"), &windows_and_linux_row_ids());
    assert!(problems.is_empty(), "docs/test-plan.md Part 4: {problems:?}");
}

#[test]
fn the_row_check_of_the_test_plan_sees_missing_duplicate_and_empty_rows_and_ignores_the_result_column() {
    let ids = vec!["W1".to_owned(), "W2".to_owned(), "W3".to_owned()];
    let doc = "\
| # | Plan | What to do | Expected | Result |
|---|---|---|---|---|
| W1 | 02.2-04 | run it | it runs | pass: it ran \\| twice |
| W2 | 02.2-04 | | it ends | |
| W2 | 02.2-04 | again | it ends | |
| W10 | 02.2-04 | not W1 | at all | |
";
    let problems = test_plan_row_problems(doc, &ids);
    assert_eq!(
        problems,
        [
            "row W2 appears 2 times".to_owned(), // two rows: the empty cell of the first is not reported twice
            "row W3 is missing".to_owned(),
        ]
    );
    let empty = "| W1 | 02.2-04 | | it runs | |\n";
    assert_eq!(
        test_plan_row_problems(empty, &["W1".to_owned()]),
        ["row W1 has an empty Plan, What to do or Expected cell".to_owned()]
    );
    let short = "| W1 | 02.2-04 | run it | it runs |\n";
    assert_eq!(test_plan_row_problems(short, &["W1".to_owned()]), ["row W1 has 4 cells, the table has five columns".to_owned()]);
    // A filled Result column changes nothing.
    let done = "| W1 | 02.2-04 | run it | it runs | fail: a long line | with | bars |\n";
    assert!(test_plan_row_problems(done, &["W1".to_owned()]).is_empty());
}

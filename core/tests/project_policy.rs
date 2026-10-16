//! The guard of DESK-26 (F33: no telemetry and no account) and of the dependency budget (D-03, D-04).
//!
//! A negative claim such as "nothing phones home" needs a test that fails when someone adds the wrong crate or relaxes
//! the policy. This file does that with std only (no crate is added for it, D-03): it reads `Cargo.lock`, the
//! manifests, `tauri.conf.json`, `app/package.json` and the first-party sources as text, from the workspace root that
//! `CARGO_MANIFEST_DIR` points into.
//!
//! What is pinned:
//!
//! * the direct dependency list of every workspace crate (the members are read from the manifests) and the dev
//!   dependencies of the app package, each entry with a reason (a new dependency fails the test with its name, which
//!   forces a conscious D-03 decision in this file);
//! * the dependency closure of the app: no telemetry or network client crate, except the named exceptions with a
//!   reason and their exact set of parents;
//! * the lock holds one TLS stack, rustls with the ring provider: no second crypto provider, no native TLS, no OpenSSL;
//! * the sources: no network API in the production Rust of every workspace member or in the page, except the named
//!   hits with a reason (a file is skipped only when it is a declared `#[cfg(test)] mod tests;`);
//! * the page policy (no remote origin in the CSP except the named ones, no dev server URL, no updater) and the
//!   absence of CI (D-04);
//! * the examples of `docs/config.md`, which are parsed by the real loaders, so the document cannot drift from the code;
//! * the owner's Windows and Linux list: every row W1 to W14 and L1 to L11 is in `docs/test-plan.md` exactly once.
//!
//! Every exception is data with a reason and an exact scope, and every guard is a small function that returns
//! sentences, so a test can hand it a wrong input and see it fail. An exception that matches nothing any more fails
//! too: a list can only shrink by deleting a line, never by being forgotten.
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

/// A direct dependency of a workspace crate and the reason it is there.
type Dependency = (&'static str, &'static str);

/// The direct dependencies of the workspace crates, as `Cargo.lock` lists them (normal, build, optional and dev ones),
/// each with its reason. The crates themselves are the members of the workspace, read from the manifests.
///
/// Adding a crate here is the D-03 decision: say why std and the platform are not enough, in the reason and in the
/// plan. A later plan adds one `(name, reason)` line to the list of its crate, and one entry for a new member.
const WORKSPACE_ALLOWLIST: &[(&str, &[Dependency])] = &[
    (
        "holzwave-core",
        &[
            ("ammonia", "allow-list sanitiser for the Markdown HTML of previews (preview.rs)"),
            ("argon2", "Argon2id stretching of the sync passphrase into the file key (sync/crypto.rs)"),
            ("chacha20poly1305", "XChaCha20-Poly1305 authenticated encryption of every synced file (sync/crypto.rs)"),
            (
                "getrandom",
                "operating-system randomness: the 128-bit tokens of file grants (blocks.rs) and the salts and nonces of \
                 the sync (sync/crypto.rs)",
            ),
            ("infer", "content type of a file by its magic bytes (preview.rs)"),
            ("keyring", "OS keychain backend of SecretStore, optional through the keyring feature (secrets.rs)"),
            (
                "libghostty-vt",
                "the terminal engine behind TerminalEngine, optional through the ghostty feature, pinned by revision",
            ),
            ("nucleo-matcher", "fuzzy matcher of the history search (history.rs)"),
            ("portable-pty", "PTY and child process of local panes (pty.rs)"),
            ("pulldown-cmark", "Markdown parser of previews (preview.rs)"),
            ("quick-xml", "parser of the WebDAV multistatus answers (sync/webdav.rs)"),
            ("rand", "dev dependency only: random SSH host keys for the in-process test servers (ssh.rs, files.rs tests)"),
            (
                "reqwest",
                "the HTTP client of the AI chat, built in ai/http.rs and named in that file only (see EXCEPTIONS and \
                 PERMITTED_HITS)",
            ),
            ("rusqlite", "SQLite store of the command history, bundled (history.rs)"),
            ("russh", "SSH client for shell and SFTP connections (ssh.rs, files.rs)"),
            ("russh-sftp", "SFTP file access over an SSH connection (files.rs)"),
            (
                "rustls",
                "TLS of the AI client (ai/http.rs): the ring provider is passed to the configuration explicitly and no \
                 process default is installed; the rustls and ring that ureq already links, so the lock holds one TLS stack",
            ),
            (
                "rustls-platform-verifier",
                "certificate checks of the AI client against the operating system's trust store (ai/http.rs), the choice \
                 the WebDAV transport made",
            ),
            ("serde", "derive of every file format and wire type the user or the page can send"),
            ("serde_json", "JSON of every file format the user can edit and of the wire to the page"),
            ("sha2", "SHA-256 content hashes of the sync engine (sync/engine)"),
            ("sysinfo", "the numbers of the local system monitor (monitor.rs)"),
            ("tokio", "runtime of the SSH and SFTP connections (ssh.rs, files.rs)"),
            ("ureq", "WebDAV transport of the sync, a named exception in EXCEPTIONS (sync/webdav.rs)"),
            ("zeroize", "wipes the derived sync key and the decrypted content from memory (sync/crypto.rs)"),
        ],
    ),
    (
        "holzwave-app",
        &[
            ("holzwave-core", "everything the shell shows and controls"),
            ("holzwave-ipc", "server side of the local socket that wsh talks to (lib.rs, state.rs)"),
            ("serde", "arguments of the commands and the events to the page"),
            ("serde_json", "JSON of the event payloads and of the small app files (state.rs, sync.rs)"),
            ("tauri", "the app shell"),
            ("tauri-build", "build script of the shell: the command manifest and the capabilities (build.rs)"),
            ("tauri-runtime-wry", "the wry runtime (the system web view) handed to the Tauri builder (lib.rs)"),
        ],
    ),
    (
        "holzwave-ipc",
        &[
            ("getrandom", "the per-run token of the local socket (new_token)"),
            ("interprocess", "local sockets: a Unix socket or a named pipe, no TCP"),
            ("serde", "the request and answer lines"),
            ("serde_json", "the request and answer lines"),
        ],
    ),
    ("holzwave-spike", &[("holzwave-core", "the Phase 1 measurements run the real PTY and engine")]),
    ("wsh", &[("holzwave-ipc", "the client of the local socket")]),
];

/// The only git repository the lock file may fetch from: the terminal engine, pinned by revision.
const ENGINE_REPOSITORY: &str = "https://github.com/Uzaaft/libghostty-rs";

/// The dev dependencies of the app package, each with the reason it is there, pinned to an exact version. The package
/// has no `dependencies` at all and `app/src` has no third-party code; `dist/vendor` carries the code editor bundle
/// (with a collected licence file) and PDF.js, both built from these packages.
const NPM_DEV_DEPENDENCIES: &[(&str, &str)] = &[
    ("@codemirror/commands", "editing commands of the code editor block"),
    ("@codemirror/lang-cpp", "syntax highlighting of C and C++ files in the editor"),
    ("@codemirror/lang-css", "syntax highlighting of CSS files in the editor"),
    ("@codemirror/lang-go", "syntax highlighting of Go files in the editor"),
    ("@codemirror/lang-html", "syntax highlighting of HTML files in the editor"),
    ("@codemirror/lang-java", "syntax highlighting of Java files in the editor"),
    ("@codemirror/lang-javascript", "syntax highlighting of JavaScript and TypeScript files in the editor"),
    ("@codemirror/lang-json", "syntax highlighting of JSON files in the editor"),
    ("@codemirror/lang-markdown", "syntax highlighting of Markdown files in the editor"),
    ("@codemirror/lang-python", "syntax highlighting of Python files in the editor"),
    ("@codemirror/lang-rust", "syntax highlighting of Rust files in the editor"),
    ("@codemirror/lang-sql", "syntax highlighting of SQL files in the editor"),
    ("@codemirror/lang-xml", "syntax highlighting of XML files in the editor"),
    ("@codemirror/lang-yaml", "syntax highlighting of YAML files in the editor"),
    ("@codemirror/language", "languages, highlighting, folding and bracket matching of the code editor block"),
    (
        "@codemirror/legacy-modes",
        "stream modes for languages without a Lezer grammar (shell, TOML, Swift, Ruby, Kotlin, Zig)",
    ),
    ("@codemirror/search", "search and selection matches of the code editor block"),
    ("@codemirror/state", "editor state of the code editor block"),
    ("@codemirror/view", "editor view of the code editor block"),
    ("@lezer/highlight", "highlight tags of the editor theme (vendor-src/editor.mjs)"),
    ("@tauri-apps/cli", "builds and bundles the app"),
    ("esbuild", "bundles the editor in scripts/vendor-editor.mjs, the one place the page uses a bundler"),
    ("pdfjs-dist", "PDF previews, copied to dist/vendor/pdfjs by scripts/copy-static.mjs"),
    ("playwright-core", "the browser tests"),
    ("typescript", "compiles the page"),
];

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
///
/// `holzwave-core` is the second parent of `reqwest` since Phase 7: the HTTP client of the AI chat, built in
/// `core/src/ai/http.rs` (the one file `PERMITTED_HITS` lets name it). `hyper-rustls` is its rustls connector and the
/// second parent of `hyper-util`.
///
/// `ureq` is the WebDAV client of the sync and has exactly one parent, `holzwave-core`. A slice, so that a later plan
/// appends one entry without editing a length; every entry needs a reason and a crate that is still in the closure.
const EXCEPTIONS: &[Exception] = &[
    (
        "reqwest",
        &["holzwave-core", "tauri"],
        "two uses: tauri's dev-server proxy, which is not instantiated on desktop and has no devUrl; and the HTTP client \
         of the AI chat in core/src/ai/http.rs, rustls with the ring provider and the platform verifier, no redirects",
    ),
    (
        "hyper-util",
        &["hyper-rustls", "reqwest"],
        "the HTTP client below reqwest and its rustls connector, reachable through them only",
    ),
    (
        "ureq",
        &["holzwave-core"],
        "WebDAV and Nextcloud transport of the sync (core/src/sync/webdav.rs): it connects only to the endpoint the user \
         entered, when the account is saved or a run is started, and in the background only while sync is set up \
         (SyncRuntime::background does nothing while sync is off); https is required except for a loopback address, \
         redirects are not followed and every answer is bounded",
    ),
];

/// Hits of the network scan that are named and explained: `(file, token, reason)`, one entry per file and token, with
/// the file relative to the workspace root and written with `/`. Each entry must match a real hit, or the scan names it
/// as stale. The platform opener starts `open`, `xdg-open` or `explorer` through `std::process::Command` and holds no
/// URL literal, so it is not here.
const PERMITTED_HITS: &[(&str, &str, &str)] = &[
    (
        "core/src/sync/webdav.rs",
        "ureq",
        "the WebDAV transport of the sync, a named exception of the closure guard (see EXCEPTIONS)",
    ),
    (
        "core/src/ai/http.rs",
        "reqwest",
        "the one HTTP chokepoint of the AI client: it builds the client and opens no socket by itself (building a client \
         connects to nothing); no other first-party file may name the client crate",
    ),
    (
        "core/src/widgets.rs",
        "https://",
        "the scheme check valid_url of a list item: it fetches nothing, the page passes the address to open_url \
         after a click",
    ),
    (
        "core/src/widgets.rs",
        "http://",
        "the scheme check valid_url of a list item: it fetches nothing, the page passes the address to open_url \
         after a click",
    ),
    (
        "app/src-tauri/src/files_protocol.rs",
        "http://",
        "Windows and Android spell the custom scheme holzfile as http://holzfile.localhost; the in-process handler answers \
         it and no socket is opened",
    ),
    (
        "app/src/sync.ts",
        "https://",
        "the example address shown inside the endpoint field of the sync form, not a request",
    ),
];

/// Remote origins the CSP may list: `(directive, source, reason)`. Everything else that names an origin fails
/// `page_policy_has_no_remote_origin`. The exact string is pinned by `app/src-tauri/tests/acl.rs`.
const REMOTE_ORIGIN_EXCEPTIONS: &[(&str, &str, &str)] = &[
    (
        "connect-src",
        "http://ipc.localhost",
        "the Tauri IPC bridge, which Windows and Android spell as the origin http://ipc.localhost",
    ),
    (
        "img-src",
        "http://holzfile.localhost",
        "Windows and Android spell the holzfile scheme this way (files_protocol.rs url_of); the app's own handler \
         answers it",
    ),
    (
        "media-src",
        "http://holzfile.localhost",
        "Windows and Android spell the holzfile scheme this way (files_protocol.rs url_of); the app's own handler \
         answers it",
    ),
];

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

/// `path` relative to the workspace root, written with `/` on every OS, so the keys of [`PERMITTED_HITS`] match on
/// Windows as well.
fn shown(path: &Path) -> String {
    match path.strip_prefix(root()) {
        Ok(relative) => {
            relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
        }
        Err(_) => path.display().to_string().replace('\\', "/"),
    }
}

// ---- the workspace ----------------------------------------------------------------------------------------

/// The quoted strings of the `members` array of the `[workspace]` table of a root manifest, in order; empty if the
/// manifest has no such array. The array may span lines. A member written as a glob is refused: the policy test cannot
/// guard a crate it cannot name.
fn members_of(manifest: &str) -> Vec<String> {
    let mut in_workspace = false;
    let mut collecting = false;
    let mut text = String::new();
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if collecting {
            text.push(' ');
            text.push_str(line);
        } else if line.starts_with('[') {
            in_workspace = line == "[workspace]";
        } else if in_workspace {
            let Some(rest) = line.strip_prefix("members").map(str::trim_start).and_then(|r| r.strip_prefix('=')) else {
                continue;
            };
            collecting = true;
            text.push_str(rest);
        }
        if collecting && text.contains(']') {
            break;
        }
    }
    let members: Vec<String> =
        text.split('"').skip(1).step_by(2).map(str::to_owned).filter(|member| !member.is_empty()).collect();
    for member in &members {
        assert!(
            !member.contains('*'),
            "the workspace member {member:?} is a glob: list every crate by name, so that this test can guard it"
        );
    }
    members
}

/// The name under the `[package]` header of a crate manifest: the first `name = "..."` before the next header, never
/// the name of a dependency or of a library target.
fn package_name_of(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if in_package {
            let Some(value) = line.strip_prefix("name").map(str::trim_start).and_then(|rest| rest.strip_prefix('=')) else {
                continue;
            };
            return value.trim().strip_prefix('"').and_then(|rest| rest.split('"').next()).map(str::to_owned);
        }
    }
    None
}

/// The package names of the workspace members, read from the root manifest and the manifest of each member.
fn workspace_member_names() -> BTreeSet<String> {
    let members = members_of(&read("Cargo.toml"));
    assert!(!members.is_empty(), "Cargo.toml lists no workspace members: is the reader broken?");
    members
        .iter()
        .map(|member| {
            let manifest = format!("{member}/Cargo.toml");
            package_name_of(&read(&manifest)).unwrap_or_else(|| panic!("{manifest} has no package name"))
        })
        .collect()
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

/// TLS stacks that must not be in the lock, each with the reason: the AI client and the SSH and WebDAV clients share
/// one stack, rustls with the ring provider. Whole package names only (`openssl-probe`, which finds certificate paths,
/// is not `openssl`).
const FORBIDDEN_TLS_STACKS: &[(&str, &str)] = &[
    ("aws-lc-rs", "a second crypto provider with a C toolchain; the owner chose ring"),
    ("aws-lc-sys", "the C library behind aws-lc-rs, which needs a C toolchain on every platform"),
    ("native-tls", "a second TLS backend that bypasses the pinned rustls configuration"),
    ("hyper-tls", "the native TLS connector of hyper, a second TLS backend"),
    ("tokio-native-tls", "the native TLS adapter of tokio, a second TLS backend"),
    ("openssl", "the system OpenSSL: absent on Windows and a supply-chain surface of its own"),
    ("openssl-sys", "the bindings to the system OpenSSL: absent on Windows and a supply-chain surface of its own"),
];

/// What is wrong with the TLS stacks of a lock, one sentence each: every forbidden package that exists in `graph`,
/// naming the packages that depend on it, and a graph in which `rustls` is absent or does not depend on `ring` (the one
/// stack must be there, and it must be ring).
fn tls_stack_problems(graph: &Graph) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, why) in FORBIDDEN_TLS_STACKS {
        if graph.contains_key(*name) {
            problems.push(format!(
                "{name} is in the lock (parents: {:?}) and the lock must hold one TLS stack, rustls with ring: {why}",
                parents_of(graph, name)
            ));
        }
    }
    match graph.get("rustls") {
        None => problems.push("rustls is not in the lock: the one TLS stack is missing".to_owned()),
        Some(dependencies) if !dependencies.contains("ring") => {
            problems.push("rustls does not depend on ring: the one TLS stack must use the ring provider".to_owned());
        }
        Some(_) => {}
    }
    problems
}

#[test]
fn the_lock_has_one_tls_stack_rustls_with_ring() {
    let problems = tls_stack_problems(&lock_graph());
    assert!(problems.is_empty(), "the TLS stack of the lock changed:\n  {}", problems.join("\n  "));
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

/// What is wrong with the hits of a scan against `permitted`, one sentence each: a hit that no entry permits
/// (`file:line: token`), an entry without a reason, and an entry that matched no hit at all (stale: the code it excused
/// is gone or changed, so the entry must go too). `hits` are `(file, line, token)` as found.
fn permitted_hit_problems(hits: &[(String, usize, &str)], permitted: &[(&str, &str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (file, line, token) in hits {
        if !permitted.iter().any(|(f, t, _)| f == file && t == token) {
            problems.push(format!("{file}:{line}: {token}"));
        }
    }
    for (file, token, reason) in permitted {
        if reason.trim().is_empty() {
            problems.push(format!("the permitted hit {token} in {file} has no reason: say why it is not a network call"));
        }
        if !hits.iter().any(|(f, _, t)| f == file && t == token) {
            problems.push(format!(
                "the permitted hit {token} in {file} matches nothing: it is stale, remove it from PERMITTED_HITS"
            ));
        }
    }
    problems
}

/// Whether `file` is an out-of-line test module: it is named `tests.rs`, and the module that owns it (`mod.rs` of the
/// same directory, or the `.rs` file named like the directory next to it) declares it with the line `#[cfg(test)]`
/// directly followed by the line `mod tests;`. A file is never skipped by its name alone.
fn is_declared_test_module(file: &Path) -> bool {
    if file.file_name().and_then(|n| n.to_str()) != Some("tests.rs") {
        return false;
    }
    let Some(dir) = file.parent() else { return false };
    let mut owners = vec![dir.join("mod.rs")];
    if let (Some(grandparent), Some(name)) = (dir.parent(), dir.file_name()) {
        owners.push(grandparent.join(name).with_extension("rs"));
    }
    owners.iter().filter_map(|owner| fs::read_to_string(owner).ok()).any(|text| {
        let text = text.replace("\r\n", "\n");
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        lines.windows(2).any(|pair| pair[0] == "#[cfg(test)]" && pair[1] == "mod tests;")
    })
}

/// The Rust files that ship: the `.rs` files below `src` and the `build.rs` of every workspace member, minus the
/// declared out-of-line test modules.
fn first_party_rust_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for member in members_of(&read("Cargo.toml")) {
        files.extend(files_below(&format!("{member}/src"), &["rs"]));
        let build = root().join(&member).join("build.rs");
        if build.is_file() {
            files.push(build);
        }
    }
    files.retain(|file| !is_declared_test_module(file));
    files.sort();
    files.dedup();
    files
}

// ---- dependencies -----------------------------------------------------------------------------------------

/// What is wrong with the direct dependencies of the workspace crates, one sentence each: a member with no entry in
/// `allowlist`, an entry for a crate that is no member, a dependency the lock file has and the entry lacks, one the
/// entry has and the lock file lacks, and a dependency listed without a reason.
fn allowlist_problems(
    graph: &Graph,
    members: &BTreeSet<String>,
    allowlist: &[(&str, &[(&str, &str)])],
) -> Vec<String> {
    let mut problems = Vec::new();
    let listed: BTreeSet<String> = allowlist.iter().map(|(name, _)| (*name).to_owned()).collect();
    for member in members.difference(&listed) {
        problems.push(format!(
            "the workspace crate {member} has no entry in WORKSPACE_ALLOWLIST: list its direct dependencies, each \
             with a reason"
        ));
    }
    for extra in listed.difference(members) {
        problems.push(format!("WORKSPACE_ALLOWLIST names {extra}, which is no workspace crate: remove the entry"));
    }
    for (crate_name, allowed) in allowlist {
        let Some(actual) = graph.get(*crate_name) else {
            problems.push(format!("{crate_name} is not in Cargo.lock"));
            continue;
        };
        let names: BTreeSet<String> = allowed.iter().map(|(name, _)| (*name).to_owned()).collect();
        for added in actual.difference(&names) {
            problems.push(format!(
                "{crate_name} gained the direct dependency {added}: D-03 asks for as few dependencies as possible; if \
                 it is wanted, name it in WORKSPACE_ALLOWLIST of this test with the reason in the plan"
            ));
        }
        for gone in names.difference(actual) {
            problems.push(format!("{crate_name} no longer depends on {gone}: remove it from WORKSPACE_ALLOWLIST"));
        }
        for (dependency, reason) in *allowed {
            if reason.trim().is_empty() {
                problems.push(format!(
                    "{crate_name} lists the dependency {dependency} without a reason: say why it is needed"
                ));
            }
        }
    }
    problems
}

#[test]
fn workspace_crates_keep_their_dependency_allowlist() {
    let problems = allowlist_problems(&lock_graph(), &workspace_member_names(), WORKSPACE_ALLOWLIST);
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

/// What is wrong with the denied crates in the closure of `start`, one sentence each: a denied crate with no exception,
/// one whose parents differ from the set its exception names, an exception without a reason, and an exception for a
/// crate that is not in the closure any more.
fn closure_problems(graph: &Graph, start: &str, exceptions: &[Exception]) -> Vec<String> {
    let reached = closure(graph, start);
    let mut problems = Vec::new();
    for name in reached.iter().filter(|name| is_denied(name)) {
        let parents = parents_of(graph, name);
        match exceptions.iter().find(|(exception, _, _)| exception == name) {
            None => problems.push(format!(
                "{name} is in the dependency closure of {start} and denied (parents: {parents:?}): DESK-26 says \
                 nothing phones home; remove it, or record a commented exception in EXCEPTIONS with its exact parents"
            )),
            Some((_, allowed, reason)) if parents != set(allowed) => problems.push(format!(
                "{name} ({reason}) has the parents {parents:?}, the exception allows exactly {allowed:?}"
            )),
            Some(_) => {}
        }
    }
    for (name, _, reason) in exceptions {
        if reason.trim().is_empty() {
            problems.push(format!("the exception for {name} has no reason: say why the crate is allowed"));
        }
        if !reached.contains(*name) {
            problems.push(format!(
                "the exception for {name} matches nothing: it is not in the dependency closure of {start} any more, \
                 remove the exception from EXCEPTIONS"
            ));
        }
    }
    problems
}

#[test]
fn app_closure_contains_no_telemetry_or_network_client_crates() {
    let graph = lock_graph();
    let closure = closure(&graph, "holzwave-app");
    assert!(closure.contains("tauri") && closure.contains("holzwave-core"), "the walk did not reach the dependencies");
    let problems = closure_problems(&graph, "holzwave-app", EXCEPTIONS);
    assert!(problems.is_empty(), "the telemetry and network guard failed:\n  {}", problems.join("\n  "));
}

#[test]
fn reqwest_parents_are_the_documented_set() {
    // tauri links reqwest for its dev-server proxy and holzwave-core for the HTTP client of the AI chat (see
    // EXCEPTIONS). The set of crates that depend on it must stay exactly these two: a third parent means another crate
    // brought an HTTP client in.
    let graph = lock_graph();
    let parents = parents_of(&graph, "reqwest");
    if !graph.contains_key("reqwest") {
        assert!(parents.is_empty(), "reqwest is absent from the lock but listed as a dependency of {parents:?}");
        return;
    }
    assert_eq!(parents, set(&["holzwave-core", "tauri"]), "the parents of reqwest changed");
    // hyper-util is the HTTP client below reqwest and its rustls connector: nothing else may reach it.
    assert_eq!(
        parents_of(&graph, "hyper-util"),
        set(&["hyper-rustls", "reqwest"]),
        "hyper-util has a parent besides hyper-rustls and reqwest"
    );
    // hyper-rustls is the connector of reqwest: nothing else may use it.
    assert_eq!(parents_of(&graph, "hyper-rustls"), set(&["reqwest"]), "hyper-rustls has a parent besides reqwest");
}

/// Whether `version` is an exact pin: it starts with an ASCII digit, holds only ASCII letters, digits, `.`, `-` and
/// `+`, and the part before any `-` or `+` is dot-separated numbers. A range (`^1.2.3`, `>=1`, `1.x`, `*`), a tag
/// (`latest`) or an address is not.
fn is_exact_version(version: &str) -> bool {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        && core.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// What is wrong with the `devDependencies` of a package.json against `expected`, one sentence each: a package that is
/// not expected, one that is missing, one expected without a reason, and a version that is not an exact pin.
fn npm_dev_problems(package: &serde_json::Value, expected: &[(&str, &str)]) -> Vec<String> {
    let Some(dev) = package.get("devDependencies").and_then(|d| d.as_object()) else {
        return vec!["package.json has no devDependencies object".to_owned()];
    };
    let mut problems = Vec::new();
    let names: BTreeSet<&str> = expected.iter().map(|(name, _)| *name).collect();
    for added in dev.keys().filter(|name| !names.contains(name.as_str())) {
        problems.push(format!(
            "the dev dependency {added} is not in NPM_DEV_DEPENDENCIES: D-03 asks for as few dependencies as possible; \
             if it is wanted, name it there with a reason"
        ));
    }
    for (name, reason) in expected {
        match dev.get(*name) {
            None => problems.push(format!("{name} is no longer a dev dependency: remove it from NPM_DEV_DEPENDENCIES")),
            Some(version) => match version.as_str() {
                Some(version) if is_exact_version(version) => {}
                other => problems.push(format!("{name} is not pinned to an exact version: {other:?}")),
            },
        }
        if reason.trim().is_empty() {
            problems.push(format!("the dev dependency {name} has no reason in NPM_DEV_DEPENDENCIES"));
        }
    }
    problems
}

#[test]
fn npm_package_has_only_the_expected_dev_dependencies() {
    let package: serde_json::Value = serde_json::from_str(&read("app/package.json")).expect("package.json is JSON");
    let problems = npm_dev_problems(&package, NPM_DEV_DEPENDENCIES);
    assert!(problems.is_empty(), "the dev dependencies of the app package changed (D-03):\n  {}", problems.join("\n  "));

    // No runtime dependency of any kind: `app/src` has no third-party code. The editor bundle and PDF.js in
    // `dist/vendor` are built from the dev dependencies above.
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

/// What is wrong with a content security policy, one sentence each: a `default-src` that is not `'self'` alone, a
/// remote origin that no entry of `exceptions` allows in that directive, a forbidden source, an exception the policy
/// no longer lists, and an exception without a reason. `exceptions` are `(directive, source, reason)`.
fn csp_origin_problems(csp: &str, exceptions: &[(&str, &str, &str)]) -> Vec<String> {
    let directives: BTreeMap<&str, Vec<&str>> = csp
        .split(';')
        .filter_map(|directive| {
            let mut parts = directive.split_whitespace();
            Some((parts.next()?, parts.collect()))
        })
        .collect();
    let mut problems = Vec::new();
    if directives.get("default-src") != Some(&vec!["'self'"]) {
        problems.push("default-src must be 'self' alone".to_owned());
    }
    for (directive, sources) in &directives {
        for source in sources {
            let remote = source.contains("://") || source.starts_with("http:") || source.starts_with("https:");
            if remote && !exceptions.iter().any(|(d, s, _)| d == directive && s == source) {
                problems.push(format!("{directive} lists the remote origin {source}"));
            }
            for scheme in ["ws:", "wss:", "*", "data:", "blob:", "'unsafe-inline'", "'unsafe-eval'"] {
                if *source == scheme {
                    problems.push(format!("{directive} lists {scheme}"));
                }
            }
        }
    }
    for (directive, source, reason) in exceptions {
        if !directives.get(directive).is_some_and(|sources| sources.contains(source)) {
            problems.push(format!(
                "the exception for {source} in {directive} matches nothing: the policy no longer lists it, remove it \
                 from REMOTE_ORIGIN_EXCEPTIONS"
            ));
        }
        if reason.trim().is_empty() {
            problems.push(format!("the exception for {source} in {directive} has no reason"));
        }
    }
    problems
}

#[test]
fn page_policy_has_no_remote_origin() {
    let conf: serde_json::Value = serde_json::from_str(&read("app/src-tauri/tauri.conf.json")).expect("conf is JSON");
    let csp = conf["app"]["security"]["csp"].as_str().expect("tauri.conf.json sets a CSP");

    // The IPC host of the Tauri bridge (to connect to) and the holzfile host (images and media) are the only origins
    // that may appear, each in its own directive.
    let problems = csp_origin_problems(csp, REMOTE_ORIGIN_EXCEPTIONS);
    assert!(problems.is_empty(), "the page policy is out of step:\n  {}", problems.join("\n  "));

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
    let rust_files = first_party_rust_files();
    assert!(rust_files.len() >= 15, "only {} Rust files found: is the walk broken?", rust_files.len());

    let mut page_files = files_below("app/src", &["ts", "html", "css"]);
    assert!(page_files.len() >= 20, "only {} page files found: is the walk broken?", page_files.len());
    page_files.sort();

    let mut hits: Vec<(String, usize, &str)> = Vec::new();
    for file in &rust_files {
        let relative = shown(file);
        let source = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {relative}: {e}"));
        hits.extend(rust_network_hits(&source).into_iter().map(|(line, token)| (relative.clone(), line, token)));
    }
    let mut relative_url_problems = Vec::new();
    for file in &page_files {
        let relative = shown(file);
        let source = fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {relative}: {e}"));
        let page_hits = page_network_hits(&source, relative.ends_with(".ts"));
        hits.extend(page_hits.into_iter().map(|(line, token)| (relative.clone(), line, token)));
        if relative.ends_with(".html") {
            for attribute in ["src", "href", "action"] {
                if source.contains(&format!("{attribute}=\"//")) {
                    relative_url_problems.push(format!("{relative}: a protocol-relative {attribute} URL"));
                }
            }
        }
    }
    let mut problems = permitted_hit_problems(&hits, PERMITTED_HITS);
    problems.extend(relative_url_problems);
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

/// A toy lock file: each package with the names it depends on.
fn toy_lock(packages: &[(&str, &[&str])]) -> String {
    let mut lock = String::from("version = 4\n");
    for (name, dependencies) in packages {
        lock.push_str(&format!("\n[[package]]\nname = \"{name}\"\nversion = \"1.0.0\"\n"));
        if !dependencies.is_empty() {
            lock.push_str("dependencies = [\n");
            for dependency in *dependencies {
                lock.push_str(&format!(" \"{dependency}\",\n"));
            }
            lock.push_str("]\n");
        }
    }
    lock
}

/// The shape of the real `ureq` exception (one named parent), with the parent `core` of the toy locks.
const TOY_UREQ: &[Exception] = &[("ureq", &["core"], "the sync transport")];

#[test]
fn closure_guard_accepts_an_exception_with_exactly_its_parents() {
    let graph = parse_lock(&toy_lock(&[("app", &["core"]), ("core", &["ureq"]), ("ureq", &[])]));

    assert_eq!(closure_problems(&graph, "app", TOY_UREQ), Vec::<String>::new());
}

#[test]
fn closure_guard_reports_a_second_parent() {
    // `other` is in the closure and also lists ureq: the exception names `core` alone.
    let graph = parse_lock(&toy_lock(&[
        ("app", &["core", "other"]),
        ("core", &["ureq"]),
        ("other", &["ureq"]),
        ("ureq", &[]),
    ]));

    let problems = closure_problems(&graph, "app", TOY_UREQ);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains(r#"{"core", "other"}"#), "the parents found are not named: {problems:?}");
    assert!(problems[0].contains(r#"["core"]"#), "the parents allowed are not named: {problems:?}");
}

#[test]
fn closure_guard_reports_a_denied_crate_without_an_exception() {
    let graph = parse_lock(&toy_lock(&[("app", &["core"]), ("core", &["reqwest"]), ("reqwest", &[])]));

    let problems = closure_problems(&graph, "app", &[]);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("reqwest is in the dependency closure of app and denied"), "{problems:?}");
}

#[test]
fn closure_guard_reports_an_exception_without_a_reason() {
    let graph = parse_lock(&toy_lock(&[("app", &["core"]), ("core", &["ureq"]), ("ureq", &[])]));

    for reason in ["", "   \t"] {
        let exceptions: &[Exception] = &[("ureq", &["core"], reason)];
        let problems = closure_problems(&graph, "app", exceptions);
        assert_eq!(problems.len(), 1, "reason {reason:?}: {problems:?}");
        assert!(problems[0].contains("the exception for ureq has no reason"), "{problems:?}");
    }
}

#[test]
fn closure_guard_reports_a_stale_exception() {
    // ureq is no longer reachable from app: the exception outlived its cause.
    let graph = parse_lock(&toy_lock(&[("app", &["core"]), ("core", &[]), ("ureq", &[])]));

    let problems = closure_problems(&graph, "app", TOY_UREQ);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("ureq") && problems[0].contains("remove the exception"), "{problems:?}");
}

// ---- the guards can fail ----------------------------------------------------------------------------------

#[test]
fn tls_guard_reports_a_second_tls_stack() {
    let graph = parse_lock(&toy_lock(&[
        ("app", &["rustls", "native-tls", "openssl"]),
        ("rustls", &["ring"]),
        ("ring", &[]),
        ("native-tls", &["openssl"]),
        ("openssl", &[]),
    ]));

    let problems = tls_stack_problems(&graph);

    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("native-tls is in the lock")), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("openssl is in the lock")), "{problems:?}");
}

#[test]
fn tls_guard_names_the_parent_of_the_forbidden_crate() {
    let graph = parse_lock(&toy_lock(&[
        ("app", &["rustls"]),
        ("rustls", &["ring", "aws-lc-rs"]),
        ("ring", &[]),
        ("aws-lc-rs", &[]),
    ]));

    let problems = tls_stack_problems(&graph);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("aws-lc-rs is in the lock"), "{problems:?}");
    assert!(problems[0].contains(r#"{"rustls"}"#), "the parent is not named: {problems:?}");
}

#[test]
fn tls_guard_matches_whole_names_only() {
    // `openssl-probe` finds certificate paths and is linked today; it is not the system OpenSSL.
    let graph = parse_lock(&toy_lock(&[
        ("app", &["rustls", "openssl-probe"]),
        ("rustls", &["ring"]),
        ("ring", &[]),
        ("openssl-probe", &[]),
    ]));

    assert_eq!(tls_stack_problems(&graph), Vec::<String>::new());
}

#[test]
fn tls_guard_reports_a_rustls_without_ring() {
    let without_ring = parse_lock(&toy_lock(&[("app", &["rustls"]), ("rustls", &[])]));
    let problems = tls_stack_problems(&without_ring);
    assert_eq!(problems, ["rustls does not depend on ring: the one TLS stack must use the ring provider".to_owned()]);

    let no_rustls = parse_lock(&toy_lock(&[("app", &["ring"]), ("ring", &[])]));
    let problems = tls_stack_problems(&no_rustls);
    assert_eq!(problems, ["rustls is not in the lock: the one TLS stack is missing".to_owned()]);
}

#[test]
fn members_of_reads_the_workspace_array() {
    let one_line =
        "[workspace]\nresolver = \"2\"\nmembers = [\"core\", \"app/src-tauri\"]\n\n[workspace.package]\nversion = \"1\"\n";
    assert_eq!(members_of(one_line), ["core", "app/src-tauri"]);

    let several_lines = "[workspace]\nmembers = [\n    \"core\", # the shared core\n    \"ipc\",\n    \"wsh\"\n]\n\n\
                         [profile.release]\nlto = \"thin\"\n";
    assert_eq!(members_of(several_lines), ["core", "ipc", "wsh"]);

    // A `members` key in another table is not the workspace array, and a crate manifest has none.
    assert!(members_of("[package]\nname = \"x\"\nmembers = [\"nope\"]\n").is_empty());
    assert!(members_of("[workspace]\nresolver = \"2\"\n[other]\nmembers = [\"nope\"]\n").is_empty());
}

#[test]
#[should_panic(expected = "is a glob")]
fn members_of_refuses_a_glob() {
    members_of("[workspace]\nmembers = [\"core\", \"plugins/*\"]\n");
}

#[test]
fn package_name_of_reads_the_package_table_not_a_dependency() {
    let manifest = "[dependencies]\nfoo = \"1\"\n\n[dependencies.bar]\nname = \"not-this\"\n\n[package]\n\
                    description = \"d\"\n\nname = \"real\"\nversion = \"1\"\n\n[lib]\nname = \"other\"\n";
    assert_eq!(package_name_of(manifest).as_deref(), Some("real"));
    assert_eq!(package_name_of("[dependencies]\nname = \"x\"\n"), None);
    assert_eq!(package_name_of("[package]\nversion = \"1\"\n\n[lib]\nname = \"other\"\n"), None);
}

#[test]
fn workspace_members_include_wsh_and_the_holzwave_crates() {
    let members = workspace_member_names();
    for name in ["holzwave-core", "holzwave-app", "holzwave-ipc", "holzwave-spike", "wsh"] {
        assert!(members.contains(name), "{name} is not a workspace member: {members:?}");
    }
}

/// A toy workspace: `app` depends on `a` and `b`.
fn toy_graph() -> Graph {
    parse_lock(&toy_lock(&[("app", &["a", "b"]), ("a", &[]), ("b", &[]), ("ghost", &[])]))
}

#[test]
fn allowlist_guard_accepts_a_matching_workspace() {
    let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", "first"), ("b", "second")])];

    assert_eq!(allowlist_problems(&toy_graph(), &set(&["app"]), allowlist), Vec::<String>::new());
}

#[test]
fn allowlist_guard_reports_an_added_dependency() {
    let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", "first")])];

    let problems = allowlist_problems(&toy_graph(), &set(&["app"]), allowlist);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("app gained the direct dependency b"), "{problems:?}");
}

#[test]
fn allowlist_guard_reports_a_removed_dependency() {
    let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", "first"), ("b", "second"), ("c", "third")])];

    let problems = allowlist_problems(&toy_graph(), &set(&["app"]), allowlist);

    assert_eq!(problems, ["app no longer depends on c: remove it from WORKSPACE_ALLOWLIST".to_owned()]);
}

#[test]
fn allowlist_guard_reports_an_unlisted_member() {
    let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", "first"), ("b", "second")])];

    let problems = allowlist_problems(&toy_graph(), &set(&["app", "ghost"]), allowlist);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("the workspace crate ghost has no entry in WORKSPACE_ALLOWLIST"), "{problems:?}");
}

#[test]
fn allowlist_guard_reports_an_entry_for_a_crate_that_is_no_member() {
    let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", "first"), ("b", "second")]), ("ghost", &[])];

    let problems = allowlist_problems(&toy_graph(), &set(&["app"]), allowlist);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("names ghost, which is no workspace crate"), "{problems:?}");
}

#[test]
fn allowlist_guard_reports_an_empty_reason() {
    for reason in ["", "  "] {
        let allowlist: &[(&str, &[(&str, &str)])] = &[("app", &[("a", reason), ("b", "second")])];

        let problems = allowlist_problems(&toy_graph(), &set(&["app"]), allowlist);

        assert_eq!(problems, ["app lists the dependency a without a reason: say why it is needed".to_owned()]);
    }
}

fn toy_package(dev: &[(&str, &str)]) -> serde_json::Value {
    let dev: serde_json::Map<String, serde_json::Value> =
        dev.iter().map(|(name, version)| ((*name).to_owned(), serde_json::Value::String((*version).to_owned()))).collect();
    serde_json::json!({ "devDependencies": dev })
}

#[test]
fn npm_guard_accepts_exact_pins() {
    let versions = ["6.11.1", "3.0.0-alpha.4", "0.28.2", "1.0.0+build.5", "7.0.2"];
    for version in versions {
        let package = toy_package(&[("tool", version)]);
        assert_eq!(npm_dev_problems(&package, &[("tool", "builds")]), Vec::<String>::new(), "version {version}");
    }
}

#[test]
fn npm_guard_reports_an_added_package() {
    let package = toy_package(&[("tool", "1.0.0"), ("extra", "2.0.0")]);

    let problems = npm_dev_problems(&package, &[("tool", "builds")]);

    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("the dev dependency extra is not in NPM_DEV_DEPENDENCIES"), "{problems:?}");
}

#[test]
fn npm_guard_reports_a_removed_package() {
    let package = toy_package(&[("tool", "1.0.0")]);

    let problems = npm_dev_problems(&package, &[("tool", "builds"), ("gone", "tests")]);

    assert_eq!(problems, ["gone is no longer a dev dependency: remove it from NPM_DEV_DEPENDENCIES".to_owned()]);
}

#[test]
fn npm_guard_reports_a_range_version() {
    for version in ["^1.2.3", "~1.2.3", ">=1", "1.x", "*", "latest", "next", "", "file:../x", "https://example.org/x.tgz"] {
        let package = toy_package(&[("tool", version)]);

        let problems = npm_dev_problems(&package, &[("tool", "builds")]);

        assert_eq!(problems.len(), 1, "version {version:?}: {problems:?}");
        assert!(problems[0].starts_with("tool is not pinned to an exact version"), "{problems:?}");
    }
}

#[test]
fn npm_guard_reports_an_empty_reason() {
    let package = toy_package(&[("tool", "1.0.0")]);

    let problems = npm_dev_problems(&package, &[("tool", " ")]);

    assert_eq!(problems, ["the dev dependency tool has no reason in NPM_DEV_DEPENDENCIES".to_owned()]);
}

#[test]
fn shown_uses_forward_slashes() {
    assert_eq!(shown(&root().join("core").join("src").join("lib.rs")), "core/src/lib.rs");
    assert_eq!(shown(&root().join("app").join("src-tauri").join("build.rs")), "app/src-tauri/build.rs");
    // A path outside the workspace keeps its place but loses its backslashes.
    assert!(!shown(Path::new("elsewhere\\file.rs")).contains('\\'));
}

#[test]
fn test_module_skip_applies_only_to_a_declared_cfg_test_module() {
    let dir = common::scratch_dir("policy-skip");
    let write = |relative: &str, text: &str| {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().expect("a parent")).expect("directory");
        fs::write(path, text).expect("file");
    };
    // Declared by the mod.rs of its directory.
    write("declared/mod.rs", "pub fn f() {}\n\n#[cfg(test)]\nmod tests;\n");
    write("declared/tests.rs", "use std::net::TcpStream;\n");
    // Declared by the .rs file next to its directory, with Windows line ends.
    write("sibling.rs", "#[cfg(test)]\r\nmod tests;\r\n");
    write("sibling/tests.rs", "");
    // Named tests.rs, but the module declares it without the attribute: production code.
    write("plain/mod.rs", "mod tests;\n");
    write("plain/tests.rs", "use std::net::TcpStream;\n");
    // The attribute is not directly before the declaration.
    write("gap/mod.rs", "#[cfg(test)]\nfn helper() {}\nmod tests;\n");
    write("gap/tests.rs", "");
    // Declared, but this file is not called tests.rs.
    write("named/mod.rs", "#[cfg(test)]\nmod tests;\n");
    write("named/helpers.rs", "");
    // No module owns it.
    write("orphan/tests.rs", "");

    let skipped: Vec<bool> = [
        "declared/tests.rs",
        "sibling/tests.rs",
        "plain/tests.rs",
        "gap/tests.rs",
        "named/helpers.rs",
        "named/mod.rs",
        "orphan/tests.rs",
    ]
    .iter()
    .map(|relative| is_declared_test_module(&dir.join(relative)))
    .collect();
    fs::remove_dir_all(&dir).expect("remove scratch directory");

    assert_eq!(skipped, [true, true, false, false, false, false, false]);
}

#[test]
fn first_party_rust_files_include_ipc_and_wsh() {
    let files: BTreeSet<String> = first_party_rust_files().iter().map(|file| shown(file)).collect();

    let expected_files =
        ["core/src/lib.rs", "core/src/sync/engine/mod.rs", "ipc/src/lib.rs", "wsh/src/main.rs", "app/src-tauri/build.rs"];
    for expected in expected_files {
        assert!(files.contains(expected), "{expected} is not scanned: {files:?}");
    }
    assert!(!files.contains("core/src/sync/engine/tests.rs"), "the declared test module is scanned");
}

#[test]
fn permitted_hits_guard_reports_an_unpermitted_hit_a_stale_entry_and_an_empty_reason() {
    let hits = [("a.rs".to_owned(), 3, "ureq"), ("b.rs".to_owned(), 7, "http://")];
    let permitted = [("a.rs", "ureq", "the transport"), ("c.rs", "https://", "an example"), ("a.rs", "std::net", " ")];

    let problems = permitted_hit_problems(&hits, &permitted);

    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(problems.contains(&"b.rs:7: http://".to_owned()), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("the permitted hit https:// in c.rs matches nothing")), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("the permitted hit std::net in a.rs has no reason")), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("the permitted hit std::net in a.rs matches nothing")), "{problems:?}");
    // An exact match is quiet.
    assert!(permitted_hit_problems(&hits[..1], &permitted[..1]).is_empty());
}

/// The shape of the real policy (`tauri.conf.json`), as text.
const TOY_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
    img-src 'self' holzfile: http://holzfile.localhost; media-src 'self' holzfile: http://holzfile.localhost; \
    font-src 'self'; connect-src 'self' ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'";

#[test]
fn csp_guard_accepts_the_real_shape() {
    assert_eq!(csp_origin_problems(TOY_CSP, REMOTE_ORIGIN_EXCEPTIONS), Vec::<String>::new());
}

#[test]
fn csp_guard_reports_the_holzfile_origin_in_script_src() {
    let csp = TOY_CSP.replace("script-src 'self'", "script-src 'self' http://holzfile.localhost");

    let problems = csp_origin_problems(&csp, REMOTE_ORIGIN_EXCEPTIONS);

    assert_eq!(problems, ["script-src lists the remote origin http://holzfile.localhost".to_owned()]);
}

#[test]
fn csp_guard_reports_a_remote_origin_in_img_src() {
    let (from, to) = ("img-src 'self' holzfile: http://holzfile.localhost", "img-src 'self' holzfile: https://example.org");
    let csp = TOY_CSP.replace(from, to);

    let problems = csp_origin_problems(&csp, REMOTE_ORIGIN_EXCEPTIONS);

    assert!(problems.contains(&"img-src lists the remote origin https://example.org".to_owned()), "{problems:?}");
}

#[test]
fn csp_guard_reports_a_stale_exception() {
    let csp = TOY_CSP.replace("media-src 'self' holzfile: http://holzfile.localhost; ", "");

    let problems = csp_origin_problems(&csp, REMOTE_ORIGIN_EXCEPTIONS);

    assert_eq!(problems.len(), 1, "{problems:?}");
    let said = "the exception for http://holzfile.localhost in media-src matches nothing";
    assert!(problems[0].starts_with(said), "{problems:?}");
}

#[test]
fn csp_guard_reports_an_empty_reason() {
    let exceptions = [
        ("connect-src", "http://ipc.localhost", " "),
        ("img-src", "http://holzfile.localhost", "r"),
        ("media-src", "http://holzfile.localhost", "r"),
    ];

    let problems = csp_origin_problems(TOY_CSP, &exceptions);

    assert_eq!(problems, ["the exception for http://ipc.localhost in connect-src has no reason".to_owned()]);
}

#[test]
fn csp_guard_reports_an_unsafe_source() {
    for (from, to, said) in [
        ("style-src 'self'", "style-src 'self' 'unsafe-inline'", "style-src lists 'unsafe-inline'"),
        ("script-src 'self'", "script-src 'self' 'unsafe-eval'", "script-src lists 'unsafe-eval'"),
        ("font-src 'self'", "font-src 'self' data:", "font-src lists data:"),
        ("object-src 'none'", "object-src blob:", "object-src lists blob:"),
        ("connect-src 'self' ipc:", "connect-src 'self' ipc: ws: wss:", "connect-src lists ws:"),
        ("form-action 'none'", "form-action *", "form-action lists *"),
    ] {
        let problems = csp_origin_problems(&TOY_CSP.replace(from, to), REMOTE_ORIGIN_EXCEPTIONS);

        assert!(problems.contains(&said.to_owned()), "{to}: {problems:?}");
    }
}

#[test]
fn csp_guard_reports_a_default_src_that_is_not_self_alone() {
    for default in ["default-src 'self' https:", "default-src *", "default-src 'none'"] {
        let problems = csp_origin_problems(&TOY_CSP.replace("default-src 'self'", default), REMOTE_ORIGIN_EXCEPTIONS);

        assert!(problems.contains(&"default-src must be 'self' alone".to_owned()), "{default}: {problems:?}");
    }
    let without = TOY_CSP.replace("default-src 'self'; ", "");
    let problems = csp_origin_problems(&without, REMOTE_ORIGIN_EXCEPTIONS);
    assert!(problems.contains(&"default-src must be 'self' alone".to_owned()), "{problems:?}");
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
    (1..=14).map(|n| format!("W{n}")).chain((1..=11).map(|n| format!("L{n}"))).collect()
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

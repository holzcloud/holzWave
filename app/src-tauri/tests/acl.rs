//! The deny-by-default guard of the app shell (T-02-55): the commands the page may call are listed three times, in the
//! invoke handler of `lib.rs`, in the app manifest of `build.rs` and in the permissions of the capability, and this
//! test fails when the three lists disagree. It reads the three files as text and scans them with plain string
//! functions, so adding a command to one list and forgetting another is caught at `cargo test`.

use std::collections::BTreeSet;

const LIB_RS: &str = include_str!("../src/lib.rs");
const BUILD_RS: &str = include_str!("../build.rs");
const MENU_RS: &str = include_str!("../src/menu.rs");
const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const CAPABILITY: &str = include_str!("../capabilities/default.json");

/// Permissions of the windows that are not commands of the app: the defaults of the core plugin set and the one
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

/// The window labels the capability grants: `main` and the pattern of the windows the app makes itself. Nothing else,
/// and above all never a bare `*`: a label that is not made by the core has no permission to anything.
const GRANTED_WINDOWS: [&str; 2] = ["main", "window-*"];

/// Whether `label` matches one of the granted patterns (`window-*` is a prefix pattern, the others are exact).
fn granted(label: &str) -> bool {
    GRANTED_WINDOWS.iter().any(|pattern| match pattern.strip_suffix('*') {
        Some(prefix) => label.starts_with(prefix),
        None => label == *pattern,
    })
}

#[test]
fn the_capability_targets_only_main_and_the_windows_the_app_makes_and_no_remote_url() {
    let value = capability();
    let windows: Vec<&str> = value
        .get("windows")
        .and_then(|w| w.as_array())
        .expect("capability has a windows array")
        .iter()
        .map(|w| w.as_str().expect("a window label is a string"))
        .collect();
    assert_eq!(windows, GRANTED_WINDOWS, "the capability must grant exactly main and window-*");
    assert!(!windows.contains(&"*"), "a bare * would grant every window there is");
    for key in ["remote", "webviews", "platforms"] {
        assert!(value.get(key).is_none(), "the capability has a {key} entry; it must grant the app's own windows only");
    }
    assert!(!CAPABILITY.contains("http:"), "the capability names a remote URL");
    assert!(!CAPABILITY.contains("https:"), "the capability names a remote URL");
}

#[test]
fn every_label_the_core_makes_is_granted_and_one_it_refuses_never_exists() {
    use holzwave_core::layout::{MAIN_WINDOW, valid_window_label};

    for label in [MAIN_WINDOW, "window-1", "window-4711", "window-1234567890123456"] {
        assert!(valid_window_label(label), "{label} is a label the core makes");
        assert!(granted(label), "{label} is made by the core but has no permissions");
    }
    // The pattern is wider than the labels the core makes (`window-*` also matches `window-evil`). That is safe only
    // because the core refuses such labels, so no window can be given one, and the page cannot create windows.
    for label in ["window-evil", "window-", "window-1x", "window-12345678901234567", "main2", "Main", " main", "*"] {
        assert!(!valid_window_label(label), "{label} must not be a window label");
    }
    for label in ["main2", "Main", "other", "windows", "wind"] {
        assert!(!granted(label), "{label} must not be granted");
    }
}

#[test]
fn the_page_cannot_create_windows_or_webviews() {
    // A page that could make a window could make one of the label `window-evil` or open any URL in it. Windows are
    // made by the shell only (`build_window` in lib.rs, from the one configured window, with its label from the core).
    let permissions = capability_permissions().expect("capability permissions");
    for permission in &permissions {
        assert!(
            !permission.contains("create") && !permission.contains("webview"),
            "{permission} would let the page make a window or a webview"
        );
    }
    let lib: String = code_lines(LIB_RS).join("\n");
    assert!(lib.contains("WebviewWindowBuilder::from_config"), "a window is made from the configured window");
    for forbidden in ["WebviewUrl::External", "WebviewUrl::CustomProtocol", ".navigate(", ".on_navigation("] {
        assert!(!lib.contains(forbidden), "lib.rs uses {forbidden}: a window must load the app's own page only");
    }
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    assert!(
        !manifest.iter().any(|name| name.contains("create") || name.contains("open_window")),
        "a command that makes a window would let the page make one"
    );
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
    for name in ["note_window", "placement", "shutdown", "window_closed", "restored_windows", "open_window", "close_window"] {
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

/// The lines of a Rust source file that are not comments (line or doc comments).
fn code_lines(source: &str) -> Vec<&str> {
    source.lines().filter(|line| !line.trim_start().starts_with("//")).collect()
}

#[test]
fn the_native_menu_has_no_close_window_and_replaces_the_default_one() {
    // Cmd+W belongs to the page (D-14): a Close Window item would swallow it.
    for line in code_lines(MENU_RS) {
        assert!(!line.contains("close_window"), "menu.rs builds a Close Window item: {line}");
        assert!(!line.contains("fullscreen"), "menu.rs builds a full screen item: {line}");
    }
    // The clipboard items stay so that copy and paste reach the web view.
    for item in ["PredefinedMenuItem::cut", "PredefinedMenuItem::copy", "PredefinedMenuItem::paste", "PredefinedMenuItem::select_all"] {
        assert!(MENU_RS.contains(item), "menu.rs lost {item}");
    }
    let lib: String = code_lines(LIB_RS).join("\n");
    assert!(lib.contains("enable_macos_default_menu(false)"), "lib.rs must switch the default macOS menu off");
    assert!(lib.contains(".menu(menu::build)"), "lib.rs must install the explicit menu");
}

// ---- the page policy and the window configuration (T-02-56, T-02-57, DESK-26) ---------------------------------------

fn tauri_conf() -> serde_json::Value {
    serde_json::from_str(TAURI_CONF).expect("tauri.conf.json is JSON")
}

/// The directives of a policy: name to the list of its sources.
fn directives(policy: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut out = std::collections::BTreeMap::new();
    for part in policy.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let mut words = part.split_whitespace();
        let name = words.next().expect("a directive has a name").to_ascii_lowercase();
        assert!(!out.contains_key(&name), "the policy sets {name} twice");
        out.insert(name, words.map(str::to_owned).collect());
    }
    out
}

fn csp() -> String {
    let conf = tauri_conf();
    conf.pointer("/app/security/csp").and_then(|c| c.as_str()).expect("app.security.csp is a string").to_owned()
}

#[test]
fn the_policy_allows_nothing_inline_nothing_dynamic_and_no_remote_origin() {
    let policy = csp();
    let d = directives(&policy);
    let want = |name: &str, sources: &[&str]| {
        let have = d.get(name).unwrap_or_else(|| panic!("the policy has no {name} directive"));
        assert_eq!(have, sources, "{name} must be exactly {sources:?}");
    };
    want("default-src", &["'self'"]);
    want("script-src", &["'self'"]);
    want("style-src", &["'self'"]);
    want("font-src", &["'self'"]);
    // Images, video, audio and PDFs come from the `holzfile` scheme (files_protocol.rs): one granted file per token, no
    // path, no directory. `http://holzfile.localhost` is the same scheme on Windows and Android.
    want("img-src", &["'self'", "holzfile:", "http://holzfile.localhost"]);
    want("media-src", &["'self'", "holzfile:", "http://holzfile.localhost"]);
    // `'self'` is for the PDF viewer, which fetches its CMaps and standard fonts from the app's own bundle.
    want("connect-src", &["'self'", "ipc:", "http://ipc.localhost"]);
    want("object-src", &["'none'"]);
    want("base-uri", &["'none'"]);
    want("form-action", &["'none'"]);

    assert!(!policy.contains("unsafe-inline"), "the policy allows inline code or style");
    assert!(!policy.contains("unsafe-eval"), "the policy allows eval");
    assert!(!policy.contains("data:") && !policy.contains("blob:"), "the policy allows data or blob sources");
    // The only remote-looking origins are the IPC host and the file host of the Windows and Android webviews.
    for (name, sources) in &d {
        for source in sources {
            let remote = source.starts_with("http:") || source.starts_with("https:") || source.starts_with("ws");
            let ipc = name == "connect-src" && source == "http://ipc.localhost";
            let files = (name == "img-src" || name == "media-src") && source == "http://holzfile.localhost";
            assert!(!remote || ipc || files, "{name} allows the origin {source}");
            assert!(source != "*", "{name} allows every origin");
        }
    }
}

#[test]
fn the_main_window_draws_its_own_title_bar_and_has_a_minimum_size() {
    let conf = tauri_conf();
    let windows = conf.pointer("/app/windows").and_then(|w| w.as_array()).expect("app.windows is an array");
    // The other windows are made at run time from this one (`from_config`), with a label of the core's making.
    assert_eq!(windows.len(), 1, "one window is configured");
    let window = &windows[0];
    assert!(window.get("label").is_none_or(|l| l == "main"), "the configured window must be the one labelled main");
    assert_eq!(window.get("titleBarStyle").and_then(|v| v.as_str()), Some("Overlay"));
    assert_eq!(window.get("hiddenTitle").and_then(|v| v.as_bool()), Some(true));
    assert!(window.get("minWidth").and_then(|v| v.as_u64()).is_some_and(|w| w >= 480), "minWidth must be at least 480");
    assert!(window.get("minHeight").and_then(|v| v.as_u64()).is_some_and(|h| h >= 320), "minHeight must be at least 320");
}

#[test]
fn the_global_tauri_object_stays_and_nothing_points_at_the_network() {
    let conf = tauri_conf();
    // The page uses window.__TAURI__ and no bundler; the manifest and the capability are the guard.
    assert_eq!(conf.pointer("/app/withGlobalTauri").and_then(|v| v.as_bool()), Some(true));
    // A development CSP or a dev server URL would be a second, weaker policy.
    assert!(conf.pointer("/app/security/devCsp").is_none_or(|v| v.is_null()), "devCsp must not be set");
    assert!(conf.pointer("/build/devUrl").is_none_or(|v| v.is_null()), "devUrl must not be set");
    let dist = conf.pointer("/build/frontendDist").and_then(|v| v.as_str()).expect("build.frontendDist is a string");
    assert!(!dist.contains("://"), "frontendDist points at {dist}");
    assert!(conf.pointer("/app/security/dangerousDisableAssetCspModification").is_none(), "asset CSP is modified by Tauri");
}

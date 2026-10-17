//! The deny-by-default guard of the app shell (T-02-55): the commands the page may call are listed three times, in the
//! invoke handler of `lib.rs`, in the app manifest of `build.rs` and in the permissions of the capability, and this
//! test fails when the three lists disagree. It reads the three files as text and scans them with plain string
//! functions, so adding a command to one list and forgetting another is caught at `cargo test`.

use std::collections::BTreeSet;

const LIB_RS: &str = include_str!("../src/lib.rs");
const COMMANDS_RS: &str = include_str!("../src/commands.rs");
const HOST_RS: &str = include_str!("../src/ai_host.rs");
const IPC_RS: &str = include_str!("../../../ipc/src/lib.rs");
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

// ---- the AI commands ----------------------------------------------------------------------------------------------

/// The commands of the AI chat the page may call, reviewed one by one and sorted. A command that is added to the
/// manifest with an `ai_` name fails `the_ai_commands_are_exactly_the_reviewed_list` until someone has asked of it:
/// can it return a key, return terminal or file text, accept consent for terminal content, or decide a tool call? If
/// yes, it does not belong here.
const AI_COMMANDS: [&str; 19] = [
    "ai_ask_answer",
    "ai_ask_pending",
    "ai_cancel",
    "ai_context_preview",
    "ai_context_share",
    "ai_context_state",
    "ai_conversation_delete",
    "ai_conversation_list",
    "ai_conversation_new",
    "ai_decide",
    "ai_has_key",
    "ai_key_clear",
    "ai_key_set",
    "ai_profile_remove",
    "ai_profile_save",
    "ai_profiles",
    "ai_send",
    "ai_turn_snapshot",
    "ai_undo",
];

#[test]
fn the_ai_commands_are_exactly_the_reviewed_list() {
    let handler = handler_commands(LIB_RS).expect("handler list");
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    let permissions = capability_permissions().expect("capability permissions");
    let reviewed: BTreeSet<String> = AI_COMMANDS.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(reviewed.len(), AI_COMMANDS.len(), "AI_COMMANDS lists a command twice");
    assert!(AI_COMMANDS.windows(2).all(|pair| pair[0] < pair[1]), "AI_COMMANDS is sorted");
    let ai = |names: &BTreeSet<String>| -> BTreeSet<String> { names.iter().filter(|n| n.starts_with("ai_")).cloned().collect() };

    for (what, found) in [("the build.rs manifest", ai(&manifest)), ("the invoke handler", ai(&handler))] {
        let unreviewed: Vec<&String> = found.difference(&reviewed).collect();
        assert!(
            unreviewed.is_empty(),
            "{what} lists ai_ commands that nobody reviewed: {unreviewed:?}. Before adding one to AI_COMMANDS in acl.rs, ask whether it \
             can return a key, return terminal or file text, accept context, decide a tool call (the only command that \
             decides one is ai_decide) or release a request of wsh ai without the person (the only command that does is \
             ai_ask_answer, and it needs the seal of the card)"
        );
        let missing: Vec<&String> = reviewed.difference(&found).collect();
        assert!(missing.is_empty(), "{what} lost reviewed ai_ commands: {missing:?}");
    }
    let granted: BTreeSet<String> = reviewed.iter().map(|name| permission_of(name)).collect();
    let in_capability: BTreeSet<String> = permissions.iter().filter(|p| p.starts_with("allow-ai-")).cloned().collect();
    assert_eq!(in_capability, granted, "the capability grants the reviewed ai_ commands and no other");
    // Every one of them is a command function of commands.rs.
    for name in AI_COMMANDS {
        assert!(return_type(COMMANDS_RS, name).is_some(), "{name} is not a command function of commands.rs");
    }
}

/// The text between `->` and the body of the function `name` in `source`; empty for a function that returns nothing.
/// `None` when there is no such function.
fn return_type(source: &str, name: &str) -> Option<String> {
    let at = source.find(&format!("pub fn {name}("))?;
    let rest = &source[at..];
    let open = rest.find('(')?;
    let (mut depth, mut close) = (0i32, None);
    for (i, c) in rest[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let after = &rest[close? + 1..];
    let head = after[..after.find('{')?].trim();
    Some(head.strip_prefix("->").map_or(String::new(), |ty| ty.trim().to_owned()))
}

/// What is wrong with the key commands and the `ai_` commands among `names`, as sentences. A command whose name holds
/// `key` (the keyboard command `key` aside) may answer nothing or a flag, never data; no `ai_` command may mention a
/// key type in its answer.
fn key_command_problems(commands_rs: &str, names: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for name in names {
        let Some(answer) = return_type(commands_rs, name) else {
            problems.push(format!("{name} is in the manifest but is not a command function of commands.rs"));
            continue;
        };
        if name.contains("key") && name != "key" && answer != "Result<(), AiFault>" && answer != "Result<bool, AiFault>" {
            problems.push(format!("{name} answers `{answer}`: a command named for a key may answer nothing or a flag, never data"));
        }
        if name.starts_with("ai_") {
            for word in ["ApiKey", "Secret", "Zeroizing"] {
                if answer.contains(word) {
                    problems.push(format!("{name} answers `{answer}`, which mentions {word}"));
                }
            }
        }
    }
    problems
}

/// The public fields in the part of the host that ships whose names say they hold a key, a secret, a token or a
/// password.
fn host_field_problems(host_rs: &str) -> Vec<String> {
    let shipped = host_rs.split("\n#[cfg(test)]\nmod ").next().unwrap_or(host_rs);
    code_lines(shipped)
        .into_iter()
        .filter_map(|line| line.trim_start().strip_prefix("pub "))
        .filter_map(|rest| rest.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
        .filter(|name| ["key", "secret", "token", "password"].iter().any(|word| name.contains(word)))
        .map(|name| format!("the host has a public field `{name}`, which could hold a key"))
        .collect()
}

#[test]
fn no_command_returns_a_key_and_the_key_commands_return_nothing_or_a_flag() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    let problems = key_command_problems(COMMANDS_RS, &manifest);
    assert!(problems.is_empty(), "a command could hand a key to the page:\n  {}", problems.join("\n  "));
    assert_eq!(return_type(COMMANDS_RS, "ai_key_set").as_deref(), Some("Result<(), AiFault>"));
    assert_eq!(return_type(COMMANDS_RS, "ai_key_clear").as_deref(), Some("Result<(), AiFault>"));
    assert_eq!(return_type(COMMANDS_RS, "ai_has_key").as_deref(), Some("Result<bool, AiFault>"));
    let fields = host_field_problems(HOST_RS);
    assert!(fields.is_empty(), "{}", fields.join("\n"));
}

#[test]
fn the_return_type_scanner_flags_a_getter() {
    let source = "\
pub fn ai_key_get(core: State<Arc<AppCore>>, profile: String) -> Result<String, AiFault> {\n    todo()\n}\n\
pub fn ai_key_put(core: State<Arc<AppCore>>, profile: String, api_key: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_key_peek(core: State<Arc<AppCore>>) -> Option<ApiKey> {\n    todo()\n}\n\
pub fn ai_note(core: State<Arc<AppCore>>) -> Result<Zeroizing<String>, AiFault> {\n    todo()\n}\n\
pub fn key(core: State<Arc<AppCore>>, pane: PaneId) {\n    todo()\n}\n";
    assert_eq!(return_type(source, "ai_key_get").as_deref(), Some("Result<String, AiFault>"));
    assert_eq!(return_type(source, "key").as_deref(), Some(""));
    assert_eq!(return_type(source, "nothing"), None);

    assert_eq!(key_command_problems(source, &set(&["ai_key_put", "key"])), Vec::<String>::new(), "a setter and the keyboard pass");
    let getter = key_command_problems(source, &set(&["ai_key_get"]));
    assert_eq!(getter.len(), 1, "{getter:?}");
    assert!(getter[0].contains("ai_key_get") && getter[0].contains("never data"));
    let typed = key_command_problems(source, &set(&["ai_key_peek"]));
    assert!(typed.iter().any(|p| p.contains("ApiKey")), "{typed:?}");
    let secret = key_command_problems(source, &set(&["ai_note"]));
    assert!(secret.iter().any(|p| p.contains("Zeroizing")), "{secret:?}");
    let missing = key_command_problems(source, &set(&["ai_gone"]));
    assert!(missing[0].contains("not a command function"));

    let fields = host_field_problems("pub struct S {\n    pub api_key: String,\n    pub name: String,\n}\n#[cfg(test)]\nmod tests {\n    pub token: u8,\n}\n");
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert!(fields[0].contains("api_key"));
}

// ---- what the page may send for shared content ----------------------------------------------------------------------

/// The arguments of the function `name` in `source`, as (name, type) pairs in order. The `State` parameter that Tauri
/// fills in is left out. `None` when there is no such function.
fn arguments(source: &str, name: &str) -> Option<Vec<(String, String)>> {
    let at = source.find(&format!("pub fn {name}("))?;
    let rest = &source[at..];
    let open = rest.find('(')?;
    let (mut depth, mut close) = (0i32, None);
    for (i, c) in rest[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let list = &rest[open + 1..close?];
    // Split at the commas outside brackets, parentheses and angle brackets.
    let (mut parts, mut depth, mut start) = (Vec::new(), 0i32, 0usize);
    for (i, c) in list.char_indices() {
        match c {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '>' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    Some(
        parts
            .into_iter()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.split_once(':'))
            .map(|(arg, ty)| (arg.trim().to_owned(), ty.trim().to_owned()))
            .filter(|(_, ty)| !ty.starts_with("State<"))
            .collect(),
    )
}

/// What is wrong with the arguments of the commands `names` in `commands_rs`, as sentences: a context command takes
/// only a chat, a pane, a profile, a model, a flag and kind names; no `ai_` command takes content, a context, a body
/// or an envelope; `ai_send` takes the typed text and the ids of an offer, in that order and with those types.
fn context_argument_problems(commands_rs: &str, names: &BTreeSet<String>) -> Vec<String> {
    const ALLOWED: [&str; 6] = ["conversation", "pane", "profile", "model", "shared", "kinds"];
    const NEVER: [&str; 5] = ["content", "context", "body", "envelope", "excerpt"];
    let mut problems = Vec::new();
    for name in names.iter().filter(|name| name.starts_with("ai_")) {
        let Some(args) = arguments(commands_rs, name) else {
            problems.push(format!("{name} is in the manifest but is not a command function of commands.rs"));
            continue;
        };
        for (arg, _) in &args {
            if NEVER.contains(&arg.as_str()) {
                problems.push(format!("{name} takes an argument named {arg}: the page sends ids, never content"));
            }
            if name.starts_with("ai_context_") && !ALLOWED.contains(&arg.as_str()) {
                problems.push(format!("{name} takes an argument named {arg}, which is not an id, a flag or a kind"));
            }
        }
        if name == "ai_send" {
            let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
            let expected = [
                ("conversation", "String"),
                ("text", "String"),
                ("pane", "PaneId"),
                ("profile", "Option<String>"),
                ("model", "Option<String>"),
                ("offer", "Option<String>"),
                ("picked", "Option<Vec<u32>>"),
            ];
            if shown != expected {
                problems.push(format!("ai_send takes {shown:?}, not the reviewed list {expected:?}"));
            }
        }
    }
    problems
}

#[test]
fn no_context_command_takes_text_and_ai_send_takes_only_ids_for_context() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    let problems = context_argument_problems(COMMANDS_RS, &manifest);
    assert!(problems.is_empty(), "a command could take shared content from the page:\n  {}", problems.join("\n  "));
    for name in ["ai_context_state", "ai_context_share", "ai_context_preview"] {
        assert!(manifest.contains(name), "{name} is not a command");
        let args = arguments(COMMANDS_RS, name).expect("a command function");
        assert!(args.iter().all(|(arg, _)| arg != "text"), "{name} takes text");
    }
    let preview = arguments(COMMANDS_RS, "ai_context_preview").expect("a command function");
    assert_eq!(preview.last().map(|(arg, ty)| (arg.as_str(), ty.as_str())), Some(("kinds", "Vec<ContextKind>")));
}

#[test]
fn the_argument_scanner_flags_a_command_that_takes_content() {
    let source = "\
pub fn ai_context_leak(core: State<Arc<AppCore>>, conversation: String, pane: PaneId, context: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_context_text(core: State<Arc<AppCore>>, conversation: String, text: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_context_fine(core: State<Arc<AppCore>>, conversation: String, pane: PaneId, kinds: Vec<ContextKind>) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_note(core: State<Arc<AppCore>>, body: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_send(core: State<Arc<AppCore>>, conversation: String, text: String, pane: PaneId, envelope: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert_eq!(
        arguments(source, "ai_context_fine").expect("found"),
        [
            ("conversation".to_owned(), "String".to_owned()),
            ("pane".to_owned(), "PaneId".to_owned()),
            ("kinds".to_owned(), "Vec<ContextKind>".to_owned())
        ]
    );
    assert_eq!(arguments(source, "nothing"), None);
    let all = set(&["ai_context_leak", "ai_context_text", "ai_context_fine", "ai_note", "ai_send"]);
    let problems = context_argument_problems(source, &all);
    let said = |needle: &str| problems.iter().any(|problem| problem.contains(needle));
    assert!(said("ai_context_leak takes an argument named context"), "{problems:?}");
    assert!(said("ai_context_text takes an argument named text, which is not an id"), "{problems:?}");
    assert!(said("ai_note takes an argument named body"), "{problems:?}");
    assert!(said("ai_send takes"), "{problems:?}");
    assert!(!problems.iter().any(|problem| problem.contains("ai_context_fine")), "{problems:?}");
}

// ---- the one command that decides a tool call -----------------------------------------------------------------------

/// What is wrong with the way the commands in `names` could decide a tool call, as sentences: `ai_decide` takes the chat,
/// the call, the verdict and the hash, as strings and in that order, and answers nothing; `ai_undo` takes the chat and the
/// call and answers nothing (it is no verdict: it puts back a file the assistant changed); no other `ai_` command takes a
/// call, a verdict or a hash.
fn decision_problems(commands_rs: &str, names: &BTreeSet<String>) -> Vec<String> {
    const OWN: [&str; 3] = ["call_id", "decision", "hash"];
    let mut problems = Vec::new();
    for name in names.iter().filter(|name| name.starts_with("ai_")) {
        let Some(args) = arguments(commands_rs, name) else {
            problems.push(format!("{name} is in the manifest but is not a command function of commands.rs"));
            continue;
        };
        if name == "ai_decide" {
            let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
            let expected = [("conversation", "String"), ("call_id", "String"), ("decision", "String"), ("hash", "String")];
            if shown != expected {
                problems.push(format!("ai_decide takes {shown:?}, not the reviewed list {expected:?}"));
            }
            let answer = return_type(commands_rs, name).unwrap_or_default();
            if answer != "Result<(), AiFault>" {
                problems.push(format!("ai_decide answers `{answer}`: it answers nothing"));
            }
            continue;
        }
        if name == "ai_undo" {
            let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
            let expected = [("conversation", "String"), ("call_id", "String")];
            if shown != expected {
                problems.push(format!("ai_undo takes {shown:?}, not the reviewed list {expected:?}"));
            }
            let answer = return_type(commands_rs, name).unwrap_or_default();
            if answer != "Result<(), AiFault>" {
                problems.push(format!("ai_undo answers `{answer}`: it answers nothing"));
            }
            continue;
        }
        for (arg, _) in &args {
            if OWN.contains(&arg.as_str()) {
                problems.push(format!("{name} takes an argument named {arg}: only ai_decide names a call, a verdict or a hash"));
            }
        }
    }
    problems
}

#[test]
fn ai_decide_takes_ids_and_a_verdict_and_answers_nothing() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    assert!(manifest.contains("ai_decide"), "ai_decide is not a command");
    let problems = decision_problems(COMMANDS_RS, &manifest);
    assert!(problems.is_empty(), "a command could decide a tool call in a way nobody reviewed:\n  {}", problems.join("\n  "));
    assert_eq!(return_type(COMMANDS_RS, "ai_decide").as_deref(), Some("Result<(), AiFault>"));
    let args = arguments(COMMANDS_RS, "ai_decide").expect("a command function");
    let names: Vec<&str> = args.iter().map(|(arg, _)| arg.as_str()).collect();
    assert_eq!(names, ["conversation", "call_id", "decision", "hash"]);
    for forbidden in ["text", "path", "content", "command"] {
        assert!(!names.contains(&forbidden), "ai_decide takes {forbidden}");
    }
    for name in manifest.iter().filter(|name| name.starts_with("ai_") && *name != "ai_decide") {
        let others = arguments(COMMANDS_RS, name).expect("a command function");
        // Only `ai_decide` names a verdict or a hash; `ai_undo` names a call, as the press on a finished write does.
        let forbidden: &[&str] = if name == "ai_undo" { &["decision", "hash"] } else { &["call_id", "decision", "hash"] };
        assert!(others.iter().all(|(arg, _)| !forbidden.contains(&arg.as_str())), "{name} names a call, a verdict or a hash");
    }

    // The scanner flags a decision that answers data and a second command that names a call, and accepts the real one.
    let source = "\
pub fn ai_decide(core: State<Arc<AppCore>>, conversation: String, call_id: String, decision: String, hash: String) -> Result<String, AiFault> {\n    todo()\n}\n\
pub fn ai_approve(core: State<Arc<AppCore>>, conversation: String, call_id: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_note(core: State<Arc<AppCore>>, conversation: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    let flagged = decision_problems(source, &set(&["ai_decide", "ai_approve", "ai_note"]));
    let said = |needle: &str| flagged.iter().any(|problem| problem.contains(needle));
    assert!(said("ai_decide answers `Result<String, AiFault>`"), "{flagged:?}");
    assert!(said("ai_approve takes an argument named call_id"), "{flagged:?}");
    assert!(!flagged.iter().any(|problem| problem.contains("ai_note")), "{flagged:?}");
    let real = "pub fn ai_decide(core: State<Arc<AppCore>>, conversation: String, call_id: String, decision: String, hash: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert_eq!(decision_problems(real, &set(&["ai_decide"])), Vec::<String>::new());
}

#[test]
fn ai_undo_takes_two_ids_and_answers_nothing() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    assert!(manifest.contains("ai_undo"), "ai_undo is not a command");
    let args = arguments(COMMANDS_RS, "ai_undo").expect("a command function");
    let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
    assert_eq!(shown, [("conversation", "String"), ("call_id", "String")], "ai_undo takes the two ids and nothing else");
    assert_eq!(return_type(COMMANDS_RS, "ai_undo").as_deref(), Some("Result<(), AiFault>"));
    // Nothing that carries a path, a text or a verdict.
    for forbidden in ["text", "path", "content", "command", "decision", "hash"] {
        assert!(!args.iter().any(|(arg, _)| arg == forbidden), "ai_undo takes {forbidden}");
    }
    // The scanner flags an undo that answers data or takes more than the two ids, and accepts the real one.
    let source = "\
pub fn ai_undo(core: State<Arc<AppCore>>, conversation: String, call_id: String) -> Result<String, AiFault> {\n    todo()\n}\n";
    let flagged = decision_problems(source, &set(&["ai_undo"]));
    assert!(flagged.iter().any(|problem| problem.contains("ai_undo answers `Result<String, AiFault>`")), "{flagged:?}");
    let wider = "pub fn ai_undo(core: State<Arc<AppCore>>, conversation: String, call_id: String, text: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert!(!decision_problems(wider, &set(&["ai_undo"])).is_empty());
    let real = "pub fn ai_undo(core: State<Arc<AppCore>>, conversation: String, call_id: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert_eq!(decision_problems(real, &set(&["ai_undo"])), Vec::<String>::new());
}

// ---- the one command that releases a request of wsh ai ------------------------------------------------------------------

/// What is wrong with the way the commands in `names` could release a request of `wsh ai`, as sentences:
/// `ai_ask_answer` takes the request, the verdict and the seal, as strings and in that order, and answers nothing; no
/// other `ai_` command takes a verdict or a seal.
fn ask_problems(commands_rs: &str, names: &BTreeSet<String>) -> Vec<String> {
    const OWN: [&str; 2] = ["verdict", "seal"];
    let mut problems = Vec::new();
    for name in names.iter().filter(|name| name.starts_with("ai_")) {
        let Some(args) = arguments(commands_rs, name) else {
            problems.push(format!("{name} is in the manifest but is not a command function of commands.rs"));
            continue;
        };
        if name == "ai_ask_answer" {
            let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
            let expected = [("ask", "String"), ("verdict", "String"), ("seal", "String")];
            if shown != expected {
                problems.push(format!("ai_ask_answer takes {shown:?}, not the reviewed list {expected:?}"));
            }
            let answer = return_type(commands_rs, name).unwrap_or_default();
            if answer != "Result<(), AiFault>" {
                problems.push(format!("ai_ask_answer answers `{answer}`: it answers nothing"));
            }
            continue;
        }
        for (arg, _) in &args {
            if OWN.contains(&arg.as_str()) {
                problems.push(format!("{name} takes an argument named {arg}: only ai_ask_answer names a verdict or a seal"));
            }
        }
    }
    problems
}

#[test]
fn ai_ask_answer_takes_an_ask_a_verdict_and_a_seal_and_answers_nothing() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    assert!(manifest.contains("ai_ask_answer"), "ai_ask_answer is not a command");
    assert!(manifest.contains("ai_ask_pending"), "ai_ask_pending is not a command");
    let args = arguments(COMMANDS_RS, "ai_ask_answer").expect("a command function");
    let shown: Vec<(&str, &str)> = args.iter().map(|(arg, ty)| (arg.as_str(), ty.as_str())).collect();
    assert_eq!(shown, [("ask", "String"), ("verdict", "String"), ("seal", "String")], "three ids and nothing else");
    assert_eq!(return_type(COMMANDS_RS, "ai_ask_answer").as_deref(), Some("Result<(), AiFault>"));
    for forbidden in ["text", "path", "content", "command", "prompt", "input", "body"] {
        assert!(!args.iter().any(|(arg, _)| arg == forbidden), "ai_ask_answer takes {forbidden}");
    }
    // The list command takes nothing from the page: the calling window is filled in by Tauri.
    assert_eq!(arguments(COMMANDS_RS, "ai_ask_pending").expect("a command function").len(), 1);
    assert_eq!(
        arguments(COMMANDS_RS, "ai_ask_pending").expect("a command function")[0].1,
        "WebviewWindow",
        "the only argument of the list is the calling window"
    );
    assert_eq!(return_type(COMMANDS_RS, "ai_ask_pending").as_deref(), Some("Result<Vec<AskView>, AiFault>"));
    assert_eq!(ask_problems(COMMANDS_RS, &manifest), Vec::<String>::new());

    // The scanner flags an answer that returns data and one that takes more than the three ids, and accepts the real one.
    let returns = "pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String) -> Result<String, AiFault> {\n    todo()\n}\n";
    let flagged = ask_problems(returns, &set(&["ai_ask_answer"]));
    assert!(flagged.iter().any(|problem| problem.contains("ai_ask_answer answers `Result<String, AiFault>`")), "{flagged:?}");
    let wider = "pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String, text: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert!(!ask_problems(wider, &set(&["ai_ask_answer"])).is_empty());
    let real = "pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    assert_eq!(ask_problems(real, &set(&["ai_ask_answer"])), Vec::<String>::new());
}

#[test]
fn no_other_ai_command_has_a_verdict_or_a_seal() {
    let manifest = manifest_commands(BUILD_RS).expect("manifest list");
    for name in manifest.iter().filter(|name| name.starts_with("ai_") && *name != "ai_ask_answer") {
        let others = arguments(COMMANDS_RS, name).expect("a command function");
        assert!(others.iter().all(|(arg, _)| arg != "verdict" && arg != "seal"), "{name} names a verdict or a seal");
    }
    // A second command that takes a seal, or a verdict, is flagged.
    let source = "\
pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_release(core: State<Arc<AppCore>>, ask: String, seal: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_judge(core: State<Arc<AppCore>>, verdict: String) -> Result<(), AiFault> {\n    todo()\n}\n\
pub fn ai_note(core: State<Arc<AppCore>>, conversation: String) -> Result<(), AiFault> {\n    todo()\n}\n";
    let flagged = ask_problems(source, &set(&["ai_ask_answer", "ai_release", "ai_judge", "ai_note"]));
    let said = |needle: &str| flagged.iter().any(|problem| problem.contains(needle));
    assert!(said("ai_release takes an argument named seal"), "{flagged:?}");
    assert!(said("ai_judge takes an argument named verdict"), "{flagged:?}");
    assert!(!flagged.iter().any(|problem| problem.starts_with("ai_note") || problem.starts_with("ai_ask_answer")), "{flagged:?}");
}

/// The part of `source` that ships: up to the test module, without comment lines.
fn shipped_code(source: &str) -> String {
    let shipped = source.split("\n#[cfg(test)]\nmod ").next().unwrap_or(source);
    code_lines(shipped).join("\n")
}

/// How many times each file of `files` calls the approval table's decide method in the part that ships.
fn decide_calls(files: &[(String, String)]) -> Vec<(String, usize)> {
    files
        .iter()
        .map(|(name, source)| (name.clone(), shipped_code(source).matches(".decide(").count()))
        .filter(|(_, count)| *count > 0)
        .collect()
}

#[test]
fn the_approval_table_is_decided_from_one_place_only() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the source directory") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_name().expect("a name").to_string_lossy().into_owned();
            files.push((name, std::fs::read_to_string(&path).expect("a source file")));
        }
    }
    assert!(files.len() >= 8, "the scan found {} files", files.len());
    let calls = decide_calls(&files);
    assert_eq!(calls, [("ai_tools.rs".to_owned(), 1)], "the table is decided from one place and one call only");

    // A second call in another file, or a second one in the same file, is counted.
    let mut more = files.clone();
    more.push(("rogue.rs".to_owned(), "fn go(t: &T) {\n    t.approvals().decide(a, b, c, d);\n}\n".to_owned()));
    assert_eq!(decide_calls(&more).len(), 2);
    let twice = vec![("x.rs".to_owned(), "fn a() { t.decide(1) }\nfn b() { t.decide(2) }\n".to_owned())];
    assert_eq!(decide_calls(&twice), [("x.rs".to_owned(), 2)]);
    // A call in a comment or in a test module is not one.
    let quiet = vec![("y.rs".to_owned(), "// t.decide(1)\nfn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { x.decide(1) }\n}\n".to_owned())];
    assert_eq!(decide_calls(&quiet), Vec::<(String, usize)>::new());
}

/// How many times each file of `files` calls the desk's settle method in the part that ships.
fn settle_calls(files: &[(String, String)]) -> Vec<(String, usize)> {
    files
        .iter()
        .map(|(name, source)| (name.clone(), shipped_code(source).matches(".settle(").count()))
        .filter(|(_, count)| *count > 0)
        .collect()
}

#[test]
fn the_ask_table_is_settled_from_one_place_only() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("the source directory") {
        let path = entry.expect("an entry").path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            let name = path.file_name().expect("a name").to_string_lossy().into_owned();
            files.push((name, std::fs::read_to_string(&path).expect("a source file")));
        }
    }
    assert!(files.len() >= 8, "the scan found {} files", files.len());
    assert_eq!(settle_calls(&files), [("ai_ask.rs".to_owned(), 1)], "a request is settled from one place and one call only");

    // A second call in another file, or a second one in the same file, is counted.
    let mut more = files.clone();
    more.push(("rogue.rs".to_owned(), "fn go(a: &A) {\n    a.asks().settle(x, y, z);\n}\n".to_owned()));
    assert_eq!(settle_calls(&more).len(), 2);
    let twice = vec![("x.rs".to_owned(), "fn a() { t.settle(1) }\nfn b() { t.settle(2) }\n".to_owned())];
    assert_eq!(settle_calls(&twice), [("x.rs".to_owned(), 2)]);
    // A call in a comment or in a test module is not one.
    let quiet = vec![("y.rs".to_owned(), "// t.settle(1)\nfn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() { x.settle(1) }\n}\n".to_owned())];
    assert_eq!(settle_calls(&quiet), Vec::<(String, usize)>::new());
}

/// The variant names of the enum `Cmd` in `source`.
fn command_variants(source: &str) -> Vec<String> {
    let Some(start) = source.find("pub enum Cmd {") else { return Vec::new() };
    let mut names = Vec::new();
    for line in source[start..].lines().skip(1) {
        if line.starts_with('}') {
            break;
        }
        let Some(rest) = line.strip_prefix("    ") else { continue };
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if name.chars().next().is_some_and(char::is_uppercase) {
            names.push(name);
        }
    }
    names
}

/// The variants among `names` whose name says they decide, approve, consent to, grant, allow or deny something.
fn deciding_variants(names: &[String]) -> Vec<String> {
    const WORDS: [&str; 8] = ["decide", "decision", "approve", "approval", "consent", "grant", "allow", "deny"];
    names.iter().filter(|name| WORDS.iter().any(|word| name.to_lowercase().contains(word))).cloned().collect()
}

#[test]
fn the_local_socket_has_no_command_that_decides() {
    let variants = command_variants(IPC_RS);
    assert!(variants.len() >= 4, "the scan found {variants:?}");
    assert_eq!(deciding_variants(&variants), Vec::<String>::new(), "the socket of wsh must not decide, approve or grant");

    let synthetic = "pub enum Cmd {\n    /// Show a file.\n    View { path: String },\n    AiDecide { call: String },\n    Ping,\n}\n";
    let found = command_variants(synthetic);
    assert_eq!(found, ["View", "AiDecide", "Ping"]);
    assert_eq!(deciding_variants(&found), ["AiDecide"]);
    assert_eq!(deciding_variants(&["AllowAlways".to_owned(), "Deny".to_owned(), "Consent".to_owned()]).len(), 3);
}

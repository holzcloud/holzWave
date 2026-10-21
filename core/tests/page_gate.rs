//! The page and ACL gate: what the webview may say to the shell, and what the shell lets through, pinned from the outside.
//!
//! The page of the app is the one place where model, pane, file and server text is drawn, so the doors between it and the rest
//! of the app are checked here as text: the exact Content-Security-Policy the page runs under, the one capability and the
//! windows it grants, the three lists every command has to be in (invoke handler, build manifest, capability permission) and the
//! golden row of its arguments, the absence of any command that hands a key back, the vocabulary of the arguments of the AI
//! commands (ids and typed text only), the commands of the local socket (none can decide, consent, answer or undo), and a page file
//! that loads only its own scripts and styles.
//!
//! The files are read with `include_str!` and scanned as text, so this runs on any machine with no Zig and no shell build:
//! `cargo test --locked --offline -p holzwave-core --no-default-features --test page_gate`. The reviewed list of AI commands stays
//! in `app/src-tauri/tests/acl.rs`, which needs the shell build; this file never edits it and agrees with it by construction (a
//! command that one of them does not know fails there or here). Every scanner has a self-test that fails on a synthetic violation.

use std::collections::BTreeSet;

use serde_json::Value;

const CONF: &str = include_str!("../../app/src-tauri/tauri.conf.json");
const LIB_RS: &str = include_str!("../../app/src-tauri/src/lib.rs");
const BUILD_RS: &str = include_str!("../../app/src-tauri/build.rs");
const COMMANDS_RS: &str = include_str!("../../app/src-tauri/src/commands.rs");
const CAPABILITY: &str = include_str!("../../app/src-tauri/capabilities/default.json");
const IPC_RS: &str = include_str!("../../ipc/src/lib.rs");
const INDEX_HTML: &str = include_str!("../../app/src/index.html");
const PROTOCOL: &str = include_str!("../../app/tests/fixtures/protocol.json");

/// The policy the page has always run under, byte for byte. A change of the policy is a decision of the owner: it edits this
/// constant and the directive checks of `acl.rs` together, and says why in the commit.
const PINNED_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' holzfile: http://holzfile.localhost; media-src 'self' holzfile: http://holzfile.localhost; font-src 'self'; connect-src 'self' ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'";

// ---- scanners -------------------------------------------------------------------------------------------------------------

/// The text between `open` and the first `]` after it, with line comments removed.
fn list_after(source: &str, open: &str) -> Option<String> {
    let start = source.find(open)? + open.len();
    let end = start + source[start..].find(']')?;
    let body: Vec<&str> = source[start..end].lines().map(|line| line.split("//").next().unwrap_or("")).collect();
    Some(body.join("\n"))
}

/// The command names inside `generate_handler![ ... ]` (`commands::name, ...`).
fn handler_names(lib_rs: &str) -> Result<BTreeSet<String>, String> {
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
fn manifest_names(build_rs: &str) -> Result<BTreeSet<String>, String> {
    let body = list_after(build_rs, ".commands(&[").ok_or("build.rs has no .commands(&[ ... ]) manifest list")?;
    let mut names = BTreeSet::new();
    for (i, part) in body.split('"').enumerate() {
        if i % 2 == 1 && !names.insert(part.to_owned()) {
            return Err(format!("build.rs lists the command {part} twice"));
        }
    }
    Ok(names)
}

/// The `permissions` array of a capability file; every entry must be a plain identifier (an object would carry a scope).
fn permissions(capability: &str) -> Result<BTreeSet<String>, String> {
    let value: Value = serde_json::from_str(capability).map_err(|e| format!("the capability is not JSON: {e}"))?;
    let list = value.get("permissions").and_then(Value::as_array).ok_or("the capability has no permissions array")?;
    let mut out = BTreeSet::new();
    for entry in list {
        let name = entry.as_str().ok_or_else(|| format!("permission entry {entry} is not a plain identifier"))?;
        if !out.insert(name.to_owned()) {
            return Err(format!("the capability lists {name} twice"));
        }
    }
    Ok(out)
}

/// The permission a command needs: `allow-` plus the name with underscores turned into dashes.
fn permission_of(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// One command function of `commands.rs`.
#[derive(Debug)]
struct Sig {
    name: String,
    /// The parameters the page can fill: `(name, type)` with the injected ones (`State`, window and app handles) dropped.
    params: Vec<(String, String)>,
    /// The text of the return type; empty for a function that returns nothing.
    ret: String,
}

/// Split `text` at the commas that are not inside brackets of any kind.
fn split_top_level(text: &str) -> Vec<String> {
    let (mut depth, mut parts, mut current) = (0i32, Vec::new(), String::new());
    for c in text.chars() {
        match c {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts.into_iter().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect()
}

/// Whether a parameter type is one of the handles Tauri injects (the page never sends it).
fn injected(ty: &str) -> bool {
    ["State<", "tauri::State<", "WebviewWindow", "tauri::WebviewWindow", "Window", "AppHandle", "tauri::AppHandle"].iter().any(|p| ty.starts_with(p))
}

/// For every `#[tauri::command` attribute, the function that follows: its name, its parameters and its return type.
fn command_signatures(commands_rs: &str) -> Vec<Sig> {
    let mut sigs = Vec::new();
    let mut from = 0;
    while let Some(at) = commands_rs[from..].find("#[tauri::command") {
        let attr = from + at;
        from = attr + 1;
        let rest = &commands_rs[attr..];
        let Some(f) = rest.find("fn ") else { break };
        let after = &rest[f + 3..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let Some(open) = after.find('(') else { continue };
        let (mut depth, mut close) = (0i32, None);
        for (i, c) in after[open..].char_indices() {
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
        let Some(close) = close else { continue };
        let params = split_top_level(&after[open + 1..close])
            .into_iter()
            .filter_map(|p| p.split_once(':').map(|(n, t)| (n.trim().to_owned(), t.trim().to_owned())))
            .filter(|(_, ty)| !injected(ty))
            .collect();
        let tail = &after[close + 1..];
        let head = tail[..tail.find('{').unwrap_or(tail.len())].trim();
        let ret = head.strip_prefix("->").map_or(String::new(), |t| t.trim().to_owned());
        sigs.push(Sig { name, params, ret });
    }
    sigs
}

/// The variant names inside `pub enum Cmd { ... }` of the socket's library.
fn cmd_variants(ipc_rs: &str) -> Vec<String> {
    let Some(at) = ipc_rs.find("pub enum Cmd {") else { return Vec::new() };
    let body = &ipc_rs[at + "pub enum Cmd {".len()..];
    let (mut depth, mut end) = (1i32, body.len());
    for (i, c) in body.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    let mut names = Vec::new();
    let mut depth = 0i32;
    for line in body[..end].lines() {
        let code = line.split("//").next().unwrap_or("").trim();
        if depth == 0 && !code.is_empty() && !code.starts_with('#') {
            let name: String = code.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if name.chars().next().is_some_and(char::is_uppercase) {
                names.push(name);
            }
        }
        depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
    }
    names
}

// ---- the checks, each a function of text so that a self-test can feed it a violation -----------------------------------

/// What is wrong with the policy part of a configuration: the string and the members around it.
fn policy_problems(conf: &str, pinned: &str) -> Vec<String> {
    let mut found = Vec::new();
    let value: Value = match serde_json::from_str(conf) {
        Ok(v) => v,
        Err(e) => return vec![format!("tauri.conf.json is not JSON: {e}")],
    };
    let security = value.pointer("/app/security");
    match security.and_then(|s| s.get("csp")).and_then(Value::as_str) {
        Some(csp) if csp == pinned => {}
        Some(csp) => found.push(format!("the policy is not the pinned string:\n  found:  {csp}\n  pinned: {pinned}")),
        None => found.push("app.security.csp is missing".to_owned()),
    }
    if let Some(object) = security.and_then(Value::as_object) {
        let others: Vec<&String> = object.keys().filter(|k| k.as_str() != "csp").collect();
        if !others.is_empty() {
            found.push(format!("app.security has members besides csp: {others:?}"));
        }
    }
    if value.pointer("/build/devUrl").is_some() {
        found.push("build has a devUrl: the page would load from a server".to_owned());
    }
    found
}

/// The four lists every command of the AI chat and the Markdown renderer has to be in, and the row of the golden.
fn triple_problems(
    handler: &BTreeSet<String>, manifest: &BTreeSet<String>, permissions: &BTreeSet<String>, golden: &BTreeSet<String>,
) -> Vec<String> {
    let mut found = Vec::new();
    let relevant = |name: &String| name.starts_with("ai_") || name == "markdown_render";
    let handler: BTreeSet<&String> = handler.iter().filter(|n| relevant(n)).collect();
    let manifest: BTreeSet<&String> = manifest.iter().filter(|n| relevant(n)).collect();
    let golden: BTreeSet<&String> = golden.iter().filter(|n| relevant(n)).collect();
    let granted: BTreeSet<String> = permissions.iter().filter(|p| p.starts_with("allow-ai-") || *p == "allow-markdown-render").cloned().collect();
    let wanted: BTreeSet<String> = manifest.iter().map(|n| permission_of(n)).collect();
    let lists: [(&str, BTreeSet<&String>); 3] = [("the invoke handler", handler), ("the build manifest", manifest.clone()), ("the protocol golden", golden)];
    for (what, list) in &lists {
        if list.len() < 19 {
            found.push(format!("{what} lists {} AI commands, at least 19 are expected", list.len()));
        }
        for name in manifest.difference(list) {
            found.push(format!("{name} is in the build manifest but not in {what}"));
        }
        for name in list.difference(&manifest) {
            found.push(format!("{name} is in {what} but not in the build manifest"));
        }
        if !list.iter().any(|n| n.as_str() == "markdown_render") {
            found.push(format!("markdown_render is missing from {what}"));
        }
    }
    for permission in wanted.difference(&granted) {
        found.push(format!("{permission} is missing from capabilities/default.json"));
    }
    for permission in granted.difference(&wanted) {
        found.push(format!("{permission} in capabilities/default.json belongs to no command of the manifest"));
    }
    found
}

/// Commands whose names speak of a key and commands whose answers could carry one.
fn key_problems(sigs: &[Sig], manifest: &BTreeSet<String>) -> Vec<String> {
    let mut found = Vec::new();
    let words = ["key", "secret", "token", "password", "passphrase", "credential"];
    let named: BTreeSet<&str> = manifest.iter().map(String::as_str).filter(|n| words.iter().any(|w| n.contains(w))).collect();
    let allowed: BTreeSet<&str> =
        ["key", "ai_key_set", "ai_key_clear", "ai_has_key", "integration_secret_set", "integration_secret_clear"]
            .into_iter()
            .collect();
    for name in named.difference(&allowed) {
        found.push(format!(
            "{name} is named for a key or a secret: only key, ai_key_set, ai_key_clear, ai_has_key, integration_secret_set and integration_secret_clear may be"
        ));
    }
    for sig in sigs {
        // The two commands of the profiles' secret variables are write-only: a value goes in and nothing comes out.
        if sig.name.starts_with("integration_secret_") && sig.ret != "Result<(), IntegrationFault>" {
            found.push(format!("{} answers `{}`: a command named for a secret answers nothing", sig.name, sig.ret));
        }
        if sig.name.contains("key") && sig.name != "key" && sig.ret != "Result<(), AiFault>" && sig.ret != "Result<bool, AiFault>" {
            found.push(format!("{} answers `{}`: a command named for a key may answer nothing or a flag, never data", sig.name, sig.ret));
        }
        for word in ["ApiKey", "Secret", "Zeroizing", "Password", "Passphrase"] {
            if sig.ret.contains(word) {
                found.push(format!("{} answers `{}`, which mentions {word}", sig.name, sig.ret));
            }
        }
    }
    found
}

/// The parameters of the AI commands and of the Markdown renderer: no type that carries bytes or free-form data, and the argument
/// names equal to the row of the golden.
fn argument_problems(sigs: &[Sig], golden: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut found = Vec::new();
    for sig in sigs.iter().filter(|s| s.name.starts_with("ai_") || s.name == "markdown_render") {
        for (name, ty) in &sig.params {
            for bad in ["Value", "Vec<u8>", "Box<", "HashMap", "&[u8]", "Bytes"] {
                if ty.contains(bad) {
                    found.push(format!("{}: the argument {name} has the type `{ty}`, which carries {bad}", sig.name));
                }
            }
        }
        let names: Vec<&str> = sig.params.iter().map(|(n, _)| n.as_str()).collect();
        let row: Option<Vec<&str>> = golden.get(&sig.name).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect());
        match row {
            Some(row) if row == names => {}
            Some(row) => found.push(format!("{}: the arguments are {names:?}, the golden lists {row:?}", sig.name)),
            None => found.push(format!("{} has no row in the protocol golden", sig.name)),
        }
    }
    found
}

/// A variant of the socket's `Cmd` that could decide, consent, answer or undo.
fn socket_problems(ipc_rs: &str) -> Vec<String> {
    let variants = cmd_variants(ipc_rs);
    let mut found = Vec::new();
    if variants.is_empty() {
        found.push("the scan found no variant of Cmd".to_owned());
    }
    let words = ["decide", "approve", "consent", "grant", "allow", "undo", "answer", "verdict", "seal", "share", "confirm", "key"];
    for v in &variants {
        let lower = v.to_ascii_lowercase();
        if let Some(w) = words.iter().find(|w| lower.contains(**w)) {
            found.push(format!("the socket has a command {v} (it says `{w}`): a process in a pane must have no door to a decision"));
        }
    }
    found
}

/// What the page file loads or runs that is not its own.
fn page_problems(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find('<') {
        let start = at + i;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let tag = &lower[start + 1..end];
        let name: String = tag.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
        let attrs = &tag[name.len()..];
        if name == "script" && !attrs.contains("src=") {
            found.push("a script element with no src".to_owned());
        }
        for bad in ["base", "iframe", "frame", "object", "embed", "form"] {
            if name == bad {
                found.push(format!("a {bad} element"));
            }
        }
        if name == "meta" && attrs.contains("http-equiv") {
            found.push("a meta element with http-equiv".to_owned());
        }
        if attrs.split_whitespace().any(|a| a.starts_with("style=") || a.starts_with("on") && a.contains('=')) {
            found.push(format!("a style or event attribute on <{name}>"));
        }
        at = end.max(start + 1);
    }
    if lower.contains("http://") || lower.contains("https://") {
        found.push("the page file names an address".to_owned());
    }
    found
}

fn golden_commands() -> serde_json::Map<String, Value> {
    let golden: Value = serde_json::from_str(PROTOCOL).expect("protocol.json is JSON");
    golden.get("commands").and_then(Value::as_object).cloned().expect("protocol.json has commands")
}

// ---- the gate -------------------------------------------------------------------------------------------------------------

#[test]
fn the_policy_is_the_exact_string_the_page_has_always_run_under() {
    let found = policy_problems(CONF, PINNED_POLICY);
    assert!(found.is_empty(), "the policy of the page changed:\n  {}\nA change is a decision of the owner: edit PINNED_POLICY and acl.rs together.", found.join("\n  "));
    assert!(!PINNED_POLICY.contains("unsafe-inline") && !PINNED_POLICY.contains("unsafe-eval"), "the pinned policy allows inline or dynamic code");
}

#[test]
fn only_one_capability_exists_and_it_grants_the_app_windows_only() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/src-tauri/capabilities");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{} cannot be read: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["default.json"], "the capabilities directory holds one file, default.json: another capability is another door");
    let value: Value = serde_json::from_str(CAPABILITY).expect("the capability is JSON");
    let windows: Vec<&str> = value.get("windows").and_then(Value::as_array).expect("windows").iter().filter_map(Value::as_str).collect();
    assert_eq!(windows, ["main", "window-*"], "the capability grants the main window and the windows the app makes, nothing else");
    for member in ["remote", "webviews", "platforms"] {
        assert!(value.get(member).is_none(), "the capability has a `{member}` member: it would grant the commands to another place");
    }
}

#[test]
fn every_ai_command_and_the_markdown_command_has_the_triple_and_a_golden_row() {
    let handler = handler_names(LIB_RS).expect("handler list");
    let manifest = manifest_names(BUILD_RS).expect("manifest list");
    let permissions = permissions(CAPABILITY).expect("permissions");
    let golden: BTreeSet<String> = golden_commands().keys().cloned().collect();
    let found = triple_problems(&handler, &manifest, &permissions, &golden);
    assert!(
        found.is_empty(),
        "the lists of AI commands disagree:\n  {}\nA new ai_ command goes into the invoke handler, the build manifest, the capability, the golden and, on purpose, the reviewed list AI_COMMANDS of app/src-tauri/tests/acl.rs.",
        found.join("\n  ")
    );
    let sigs = command_signatures(COMMANDS_RS);
    let markdown = sigs.iter().find(|s| s.name == "markdown_render").expect("markdown_render is a command function");
    assert_eq!(markdown.params.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["text"]);
    assert_eq!(golden_commands()["markdown_render"], serde_json::json!(["text"]));
}

#[test]
fn no_command_hands_a_key_or_a_secret_back() {
    let manifest = manifest_names(BUILD_RS).expect("manifest list");
    let sigs = command_signatures(COMMANDS_RS);
    assert!(sigs.len() >= 60, "the scan found {} command functions", sigs.len());
    let found = key_problems(&sigs, &manifest);
    assert!(found.is_empty(), "a command could hand a key to the page:\n  {}", found.join("\n  "));
    for name in ["ai_key_set", "ai_key_clear", "ai_has_key"] {
        let sig = sigs.iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name} is not a command function"));
        assert!(sig.ret == "Result<(), AiFault>" || sig.ret == "Result<bool, AiFault>", "{name} answers {}", sig.ret);
    }
}

#[test]
fn ai_commands_take_ids_and_typed_text_only() {
    let sigs = command_signatures(COMMANDS_RS);
    let found = argument_problems(&sigs, &golden_commands());
    assert!(found.is_empty(), "an AI command takes something that is not an id or typed text:\n  {}", found.join("\n  "));
    assert!(sigs.iter().filter(|s| s.name.starts_with("ai_")).count() >= 19, "the scan found fewer than 19 AI commands");
}

#[test]
fn the_socket_has_no_door_to_a_decision() {
    let found = socket_problems(IPC_RS);
    assert!(found.is_empty(), "{}", found.join("\n"));
    let variants = cmd_variants(IPC_RS);
    for known in ["View", "Edit", "Setmeta", "Hist", "HistOpen", "Ai", "Ping"] {
        assert!(variants.iter().any(|v| v == known), "the scan did not find the variant {known}: {variants:?}");
    }
}

#[test]
fn the_page_file_loads_only_its_own_files() {
    let found = page_problems(INDEX_HTML);
    assert!(found.is_empty(), "the page file loads or runs something that is not its own:\n  {}", found.join("\n  "));
    assert!(INDEX_HTML.contains("<script type=\"module\" src=\"./main.js\">"), "the scan did not see the page's own script");
}

#[test]
fn the_scanners_can_fail() {
    // A command in one list only.
    let names = |list: &[&str]| -> BTreeSet<String> { list.iter().map(|n| (*n).to_owned()).collect() };
    let mut ai: Vec<String> = (0..19).map(|i| format!("ai_c{i:02}")).collect();
    ai.push("markdown_render".to_owned());
    let all: BTreeSet<String> = ai.iter().cloned().collect();
    let granted: BTreeSet<String> = all.iter().map(|n| permission_of(n)).collect();
    assert!(triple_problems(&all, &all, &granted, &all).is_empty(), "the synthetic lists agree");
    let mut short = all.clone();
    short.remove("ai_c07");
    let missing = triple_problems(&short, &all, &granted, &all);
    assert!(missing.iter().any(|l| l.contains("ai_c07") && l.contains("invoke handler")), "{missing:?}");
    let mut no_permission = granted.clone();
    no_permission.remove("allow-ai-c03");
    assert!(triple_problems(&all, &all, &no_permission, &all).iter().any(|l| l.contains("allow-ai-c03")));
    let mut extra = granted.clone();
    extra.insert("allow-ai-extra".to_owned());
    assert!(triple_problems(&all, &all, &extra, &all).iter().any(|l| l.contains("allow-ai-extra")));
    assert!(triple_problems(&names(&["ai_a"]), &names(&["ai_a"]), &names(&[]), &names(&["ai_a"])).iter().any(|l| l.contains("at least 19")));

    // A getter named `ai_key_get` that returns a String, and a key in a type.
    let source = "#[tauri::command(async)]\npub fn ai_key_get(core: State<Arc<AppCore>>, profile: String) -> Result<String, AiFault> {\n    todo()\n}\n\
#[tauri::command]\npub fn ai_key_set(core: State<Arc<AppCore>>, profile: String, api_key: String) -> Result<(), AiFault> {\n    todo()\n}\n\
#[tauri::command]\npub fn ai_peek(core: State<Arc<AppCore>>) -> Option<ApiKey> {\n    todo()\n}\n";
    let sigs = command_signatures(source);
    assert_eq!(sigs.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["ai_key_get", "ai_key_set", "ai_peek"]);
    assert_eq!(sigs[0].params, [("profile".to_owned(), "String".to_owned())], "the injected state is dropped");
    let manifest = names(&["ai_key_get", "ai_key_set", "ai_peek"]);
    let found = key_problems(&sigs, &manifest);
    assert!(found.iter().any(|l| l.contains("ai_key_get") && l.contains("only key")), "{found:?}");
    assert!(found.iter().any(|l| l.contains("ai_key_get") && l.contains("never data")), "{found:?}");
    assert!(found.iter().any(|l| l.contains("ai_peek") && l.contains("ApiKey")), "{found:?}");
    assert!(!found.iter().any(|l| l.starts_with("ai_key_set ")), "a setter passes: {found:?}");

    // An extra argument named `context`, a type that carries bytes, and a command with no row.
    let source = "#[tauri::command]\npub fn ai_send(core: State<Arc<AppCore>>, conversation: String, context: String, blob: Vec<u8>, map: HashMap<String, u8>) -> Result<(), AiFault> {\n    todo()\n}\n";
    let sigs = command_signatures(source);
    assert_eq!(sigs[0].params.len(), 4, "a comma inside a type does not split the parameter: {:?}", sigs[0].params);
    let mut golden = serde_json::Map::new();
    golden.insert("ai_send".to_owned(), serde_json::json!(["conversation"]));
    let found = argument_problems(&sigs, &golden);
    assert!(found.iter().any(|l| l.contains("golden lists")), "{found:?}");
    assert!(found.iter().any(|l| l.contains("Vec<u8>")) && found.iter().any(|l| l.contains("HashMap")), "{found:?}");
    assert!(argument_problems(&sigs, &serde_json::Map::new()).iter().any(|l| l.contains("no row")));

    // A socket command that could decide.
    let ipc = "pub enum Cmd {\n    View { path: String },\n    /// decide\n    AiDecide { call: String },\n    Ping,\n}\n";
    assert_eq!(cmd_variants(ipc), ["View", "AiDecide", "Ping"]);
    let found = socket_problems(ipc);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("AiDecide"));
    assert!(socket_problems("pub enum Cmd {\n    Share,\n    Confirm { x: u8 },\n}\n").len() == 2);
    assert!(!socket_problems("nothing here").is_empty(), "a scan that finds no variant is a failure");

    // A policy string with an extra source, a second member and a dev server.
    let conf = |csp: &str, extra: &str| format!("{{\"app\": {{\"security\": {{\"csp\": \"{csp}\"{extra}}}}}, \"build\": {{{}}}}}", if extra.contains("dev") { "\"devUrl\": \"http://localhost:1\"" } else { "" });
    assert!(policy_problems(&conf(PINNED_POLICY, ""), PINNED_POLICY).is_empty());
    let wider = format!("{PINNED_POLICY}; frame-src 'self'");
    assert!(policy_problems(&conf(&wider, ""), PINNED_POLICY).iter().any(|l| l.contains("not the pinned string")));
    assert!(policy_problems(&conf(PINNED_POLICY, ", \"freezePrototype\": true"), PINNED_POLICY).iter().any(|l| l.contains("besides csp")));
    assert!(policy_problems(&conf(PINNED_POLICY, ", \"dev\": 1"), PINNED_POLICY).iter().any(|l| l.contains("devUrl")));
    assert!(policy_problems("{}", PINNED_POLICY).iter().any(|l| l.contains("missing")));

    // A page file that loads or runs something of its own making.
    let page = "<html><head><meta http-equiv=\"refresh\" content=\"0\"><base href=\"x\"></head><body onload=\"x()\" style=\"a:b\"><script>1</script><iframe></iframe><script src=\"./main.js\"></script><a href=\"https://example.org/\">x</a></body></html>";
    let found = page_problems(page);
    for want in ["script element with no src", "base element", "iframe element", "meta element with http-equiv", "style or event attribute on <body>", "names an address"] {
        assert!(found.iter().any(|l| l.contains(want)), "the page scan missed {want}: {found:?}");
    }
    assert!(page_problems("<script type=\"module\" src=\"./main.js\"></script>").is_empty());
}

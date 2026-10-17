//! Fixtures of the command-line tools holzWave wraps, and the conformance tests of the real tools.
//!
//! Two kinds of test live here. The hygiene tests run everywhere and read only `core/tests/fixtures/integrations`: every
//! fixture is listed once in `MANIFEST.md` with its source (`recorded` from a real run, or `written` by hand), no fixture
//! holds a secret or an identity of a real account, and a drift variant sits next to the fixture it varies. The
//! conformance tests run the real tools (`kubectl`, `az`, GAM) against scratch configuration and only when
//! `HW_INTEGRATION_REAL=1` is set; they print one line and return when the variable or the tool is missing. No test
//! reads or writes a tool's own configuration: every run points the tool at a scratch directory.
//!
//! The tools are found by an absolute path (a directory of the `PATH`, or `HW_<TOOL>=/absolute/path`), never by a bare
//! name. The fixtures are recorded with `sh scripts/record-integration-fixtures.sh <tool|all>`.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The canary strings of the scratch kubeconfig: the one file that may hold them is the kubeconfig that carries them.
const CANARY_TOKEN: &str = "HW-CANARY-TOKEN-0001";
const CANARY_EXEC_ENV: &str = "HW-CANARY-EXEC-ENV";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn fixtures() -> PathBuf {
    root().join("core/tests/fixtures/integrations")
}

fn read_fixture(relative: &str) -> String {
    let path = fixtures().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// An executable regular file by an absolute path. The variable `HW_<NAME>` wins, then the directories of the `PATH` in
/// order. A bare name is never run.
fn find_tool(name: &str) -> Option<PathBuf> {
    let variable = format!("HW_{}", name.to_ascii_uppercase());
    if let Some(value) = std::env::var_os(&variable) {
        let path = PathBuf::from(value);
        if path.is_absolute() && is_executable(&path) {
            return Some(path);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|dir| dir.is_absolute()).map(|dir| dir.join(name)).find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Whether the real-tool tests are asked for. When they are not, one line says so.
fn gated() -> bool {
    if std::env::var("HW_INTEGRATION_REAL").as_deref() == Ok("1") {
        true
    } else {
        println!("skipped: the real-tool tests run only with HW_INTEGRATION_REAL=1");
        false
    }
}

/// The real tool when the gate is open and the tool exists; otherwise a printed reason and `None`.
fn real_tool(name: &str) -> Option<PathBuf> {
    if !gated() {
        return None;
    }
    let found = find_tool(name);
    if found.is_none() {
        println!("skipped: {name} is not installed here");
    }
    found
}

/// Every file below the fixture directory except the manifest, as paths relative to it with `/`.
fn fixture_files() -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir).expect("fixture directory").map(|e| e.expect("entry").path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                let relative = path.strip_prefix(base).expect("below base");
                let text: Vec<String> = relative.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                out.push(text.join("/"));
            }
        }
    }
    let mut files = Vec::new();
    walk(&fixtures(), &fixtures(), &mut files);
    files.retain(|f| f != "MANIFEST.md");
    files
}

/// One row of the manifest table.
#[derive(Debug, Clone)]
struct Row {
    file: String,
    source: String,
    tool: String,
}

/// The rows of `MANIFEST.md`: table lines whose first cell is a path in backticks and whose second cell is the source.
fn manifest_rows() -> Vec<Row> {
    let text = read_fixture("MANIFEST.md");
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 5 {
            continue;
        }
        let first = cells[0];
        if first.len() > 2 && first.starts_with('`') && first.ends_with('`') {
            rows.push(Row { file: first[1..first.len() - 1].to_owned(), source: cells[1].to_owned(), tool: cells[2].to_owned() });
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------------------------------
// Fixture hygiene (runs everywhere).
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn every_fixture_is_in_the_manifest_once() {
    let rows = manifest_rows();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for row in &rows {
        *seen.entry(row.file.as_str()).or_default() += 1;
    }
    let files = fixture_files();
    let mut problems = Vec::new();
    for file in &files {
        match seen.get(file.as_str()) {
            None => problems.push(format!("{file} is in the directory and not in the manifest")),
            Some(1) => {}
            Some(n) => problems.push(format!("{file} is listed {n} times in the manifest")),
        }
    }
    for row in &rows {
        if !files.contains(&row.file) {
            problems.push(format!("{} is listed in the manifest and does not exist", row.file));
        }
        if row.source != "recorded" && row.source != "written" {
            problems.push(format!("{} has the source {:?}: it must be recorded or written", row.file, row.source));
        }
    }
    assert!(problems.is_empty(), "manifest problems:\n{}", problems.join("\n"));
    assert!(!files.is_empty(), "the fixture directory holds no fixture");
}

/// Documentation addresses (RFC 5737) and the loopback are fine in a fixture; so are the public hosts of the tools' own
/// project pages that their version lines print.
const ALLOWED_HOSTS: &[&str] = &["example.com", "localhost", "127.0.0.1", "github.com", "kubernetes.io", "talos.dev"];

fn host_is_allowed(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    ALLOWED_HOSTS.iter().any(|allowed| host == *allowed || host.ends_with(&format!(".{allowed}")))
        || host.starts_with("192.0.2.")
        || host.starts_with("198.51.100.")
        || host.starts_with("203.0.113.")
}

fn is_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

/// The synthetic range of identifiers: `00000000-0000-4000-8000-0000000000NN`.
fn is_synthetic_guid(guid: &str) -> bool {
    guid.len() == 36 && guid.starts_with("00000000-0000-4000-8000-0000000000") && guid[34..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// GUID-shaped substrings (8-4-4-4-12 hex digits).
fn guids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let dashes = [8usize, 13, 18, 23];
    let mut i = 0;
    while i + 36 <= bytes.len() {
        let window = &bytes[i..i + 36];
        let shaped = window.iter().enumerate().all(|(k, b)| if dashes.contains(&k) { *b == b'-' } else { b.is_ascii_hexdigit() });
        let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after_ok = i + 36 == bytes.len() || !bytes[i + 36].is_ascii_alphanumeric();
        if shaped && before_ok && after_ok {
            out.push(String::from_utf8_lossy(window).into_owned());
            i += 36;
        } else {
            i += 1;
        }
    }
    out
}

/// Addresses and hosts that are not synthetic: an e-mail address outside `example.com` and a URL host that is not allowed.
fn foreign_addresses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '@' && i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            let host: String = chars[i + 1..].iter().take_while(|c| is_host_char(**c)).collect();
            if !host.is_empty() && !host_is_allowed(&host) {
                out.push(format!("address at {host}"));
            }
        }
    }
    for scheme in ["http://", "https://"] {
        let mut rest = text;
        while let Some(at) = rest.find(scheme) {
            let after = &rest[at + scheme.len()..];
            let host: String = after.chars().take_while(|c| is_host_char(*c)).collect();
            if !host.is_empty() && !host_is_allowed(&host) {
                out.push(format!("url host {host}"));
            }
            rest = after;
        }
    }
    out
}

/// Prefixes of well-known credential formats.
const TOKEN_PREFIXES: &[&str] = &["eyJ", "ghp_", "gho_", "ghs_", "AKIA", "ya29.", "xoxb-", "xoxp-", "sk-", "1//0", "-----BEGIN", "Bearer "];

/// Words of 40 or more characters of the base64 alphabet. Absolute paths and pure lowercase hex of 40 or 64 digits (a
/// commit or a digest) are not secrets.
fn long_secretish_words(text: &str) -> Vec<String> {
    let is_alphabet = |c: char| c.is_ascii_alphanumeric() || "+/_=-".contains(c);
    text.split(|c: char| !(is_alphabet(c) || c == '.'))
        .flat_map(|word| word.split('.'))
        .filter(|word| word.len() >= 40 && word.chars().all(is_alphabet))
        .filter(|word| !word.starts_with('/') && !word.contains("//"))
        .filter(|word| !(matches!(word.len(), 40 | 64) && word.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))))
        .map(str::to_owned)
        .collect()
}

/// Home directories of any user, and the scratch and cache directories that carry a user name.
fn path_leaks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["/Users/", "C:\\Users", "C:/Users", "/var/folders", "/private/var", "/private/tmp"] {
        if text.contains(marker) {
            out.push(format!("path {marker}"));
        }
    }
    let mut rest = text;
    while let Some(at) = rest.find("/home/") {
        let after = &rest[at + "/home/".len()..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        if name != "user" {
            out.push(format!("path /home/{name}"));
        }
        rest = after;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if home.len() > 4 && text.contains(&home) {
            out.push("the home directory of this machine".to_owned());
        }
    }
    out
}

/// The files that hold a long string on purpose: the hostile fixture exists to carry oversized values.
fn may_hold_long_strings(file: &str) -> bool {
    file.contains("hostile")
}

#[test]
fn no_fixture_holds_a_secret_or_an_identity() {
    let mut problems = Vec::new();
    for file in fixture_files() {
        let text = read_fixture(&file);
        let is_canary_source = file == "kubectl/config_view_canary.kubeconfig";
        for guid in guids(&text) {
            if !is_synthetic_guid(&guid) {
                problems.push(format!("{file}: GUID {guid} is outside the synthetic range"));
            }
        }
        for address in foreign_addresses(&text) {
            problems.push(format!("{file}: {address}"));
        }
        for prefix in TOKEN_PREFIXES {
            if text.contains(prefix) {
                problems.push(format!("{file}: a token prefix {prefix:?}"));
            }
        }
        if !may_hold_long_strings(&file) {
            for word in long_secretish_words(&text) {
                problems.push(format!("{file}: a long base64-looking string of {} characters", word.len()));
            }
        }
        for leak in path_leaks(&text) {
            problems.push(format!("{file}: {leak}"));
        }
        if !is_canary_source && (text.contains("HW-CANARY") || text.contains(CANARY_TOKEN) || text.contains(CANARY_EXEC_ENV)) {
            problems.push(format!("{file}: a canary string outside the canary kubeconfig"));
        }
    }
    assert!(problems.is_empty(), "fixture hygiene problems:\n{}", problems.join("\n"));
}

#[test]
fn the_hygiene_scanners_flag_what_they_are_for() {
    assert_eq!(guids("tenant 3f2c9d1e-aaaa-4bbb-8ccc-123456789abc end").len(), 1);
    assert!(!is_synthetic_guid("3f2c9d1e-aaaa-4bbb-8ccc-123456789abc"));
    assert!(is_synthetic_guid("00000000-0000-4000-8000-000000000002"));
    assert_eq!(foreign_addresses("mail me at someone@corp.test").len(), 1);
    assert!(foreign_addresses("user@example.com and https://api.example.com/x and https://192.0.2.7:6443").is_empty());
    assert_eq!(foreign_addresses("https://login.corp.test/path").len(), 1);
    assert_eq!(long_secretish_words(&format!("x {} y", "aB3/".repeat(12))).len(), 1);
    assert!(long_secretish_words(&format!("sha {}", "0123456789abcdef".repeat(2) + "01234567")).is_empty());
    assert!(long_secretish_words("/subscriptions/00000000-0000-4000-8000-000000000001/resourceGroups/rg-one-two").is_empty());
    assert_eq!(path_leaks("see /Users/someone/.kube").len(), 1);
    assert!(path_leaks("see /home/user/.kube").is_empty());
    assert_eq!(path_leaks("see /home/alice/.kube").len(), 1);
}

#[test]
fn config_view_redacts_the_canary_tokens() {
    let text = read_fixture("kubectl/config_view.json");
    assert!(!text.contains(CANARY_TOKEN), "the bearer token canary is in config_view.json");
    assert!(!text.contains(CANARY_EXEC_ENV), "the exec environment canary is in config_view.json");
    assert!(text.contains("REDACTED"), "config_view.json shows no REDACTED");
    let canary = read_fixture("kubectl/config_view_canary.kubeconfig");
    assert!(canary.contains(CANARY_TOKEN) && canary.contains(CANARY_EXEC_ENV), "the canary kubeconfig lost a canary");
}

// ---------------------------------------------------------------------------------------------------------------------
// The real kubectl (gated).
// ---------------------------------------------------------------------------------------------------------------------

/// Run a tool by its absolute path with a closed environment: only the variables in `env` (and a minimal `PATH`).
fn run_tool(tool: &Path, args: &[&str], env: &[(&str, &Path)]) -> std::process::Output {
    let mut command = std::process::Command::new(tool);
    command.args(args).env_clear().env("PATH", "/usr/bin:/bin").stdin(std::process::Stdio::null());
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap_or_else(|e| panic!("cannot run {}: {e}", tool.display()))
}

/// What the exec plugin environment holds is not redacted by the tool; the recorder replaces those values so that the
/// fixture holds no canary. The same replacement makes the real output comparable with the fixture.
fn with_exec_env_values_redacted(mut config: Value) -> Value {
    if let Some(users) = config.get_mut("users").and_then(Value::as_array_mut) {
        for user in users {
            if let Some(env) = user.pointer_mut("/user/exec/env").and_then(Value::as_array_mut) {
                for entry in env {
                    if let Some(object) = entry.as_object_mut() {
                        object.insert("value".to_owned(), Value::String("REDACTED".to_owned()));
                    }
                }
            }
        }
    }
    config
}

#[test]
fn config_view_of_the_canary_kubeconfig_matches_the_recorded_fixture() {
    let Some(kubectl) = real_tool("kubectl") else { return };
    let scratch = common::scratch_dir("kcview");
    let kubeconfig = scratch.join("config");
    fs::write(&kubeconfig, read_fixture("kubectl/config_view_canary.kubeconfig")).expect("write scratch kubeconfig");
    let out = run_tool(&kubectl, &["--request-timeout=5s", "config", "view", "-o", "json"], &[("KUBECONFIG", &kubeconfig), ("HOME", &scratch)]);
    assert!(out.status.success(), "config view failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(!text.contains(CANARY_TOKEN), "the tool printed the bearer token");
    assert!(text.contains("REDACTED"), "the tool printed no REDACTED");
    // Pinned as observed: the tool redacts a bearer token and does not redact the values of an exec plugin's environment,
    // so nothing may show the output of `config view` to a user or a log unfiltered.
    assert!(text.contains(CANARY_EXEC_ENV), "the tool now redacts exec environment values: the recorder may stop replacing them");
    let real: Value = serde_json::from_str(&text).expect("config view prints JSON");
    let recorded: Value = serde_json::from_str(&read_fixture("kubectl/config_view.json")).expect("fixture is JSON");
    assert_eq!(with_exec_env_values_redacted(real), recorded, "the real output differs from the recorded fixture");
    fs::remove_dir_all(&scratch).ok();
}

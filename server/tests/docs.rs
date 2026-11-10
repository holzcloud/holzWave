//! The documents of the server agree with the server: the examples of `docs/config.md` are run through the validator that
//! `serve` and `check-config` use, the keys the loader reads are the keys the document names, the bounds in the tables are
//! the bounds in the code, and the sentences in the table of `docs/server.md` are the sentences the loader prints.
//!
//! The file system is a stand-in (a probe that says the certificate and secret files exist), so no certificate is needed.
//! Every check is a plain function of the document text, and each has a negative control that feeds it a changed text and
//! requires a complaint: a check that cannot fail proves nothing.

#[path = "support/scratch.rs"]
mod scratch;

use std::collections::BTreeSet;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use holzwave_server::config::{self, Env, Probe, RealProbe, Refusal, TlsSetup, prepare};
use holzwave_server::keyfile::ServerKey;
use holzwave_server::limits;
use scratch::Scratch;
use serde_json::{Value, json};

const CONFIG_PATH: &str = "/etc/test/server.json";
const KEY_PATH: &str = "/etc/holzwave/key/server.key";

/// A document of the repository, by its path under `docs/`.
fn doc(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("docs").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The JSON of the fenced block that follows the marker `<!-- example: NAME -->` in the text of `docs/config.md`.
fn documented_example(text: &str, name: &str) -> String {
    let marker = format!("<!-- example: {name} -->");
    let after = text.split_once(&marker).unwrap_or_else(|| panic!("docs/config.md has no example {name}")).1;
    let fence = after.trim_start().strip_prefix("```json\n").unwrap_or_else(|| panic!("no json fence after {name}"));
    fence.split_once("\n```").unwrap_or_else(|| panic!("the fence of {name} is not closed")).0.to_owned()
}

// ---------------------------------------------------------------------------------------------------------------------
// The stand-in file system
// ---------------------------------------------------------------------------------------------------------------------

/// Serves one configuration text, says that the files named in the examples are readable, runs as the user the image runs
/// as, and hands out a fixed key (or reads the real file, for the rows about the key file).
struct FakeProbe {
    config: Option<String>,
    readable: Vec<PathBuf>,
    uid: u32,
    real_key: bool,
}

impl Probe for FakeProbe {
    fn read_config(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        match (&self.config, path == Path::new(CONFIG_PATH)) {
            (Some(text), true) => Ok(text.clone().into_bytes()),
            _ => Err(std::io::ErrorKind::NotFound.into()),
        }
    }

    fn readable(&self, path: &Path) -> bool {
        self.readable.iter().any(|p| p == path)
    }

    fn own_uid(&self) -> u32 {
        self.uid
    }

    fn load_key(&self, path: &Path) -> Result<ServerKey, Refusal> {
        if self.real_key {
            RealProbe.load_key(path)
        } else {
            Ok(ServerKey::from_bytes([4; 32]).expect("a key"))
        }
    }
}

fn probe(text: &str) -> FakeProbe {
    FakeProbe {
        config: Some(text.to_owned()),
        readable: [
            "/etc/holzwave/tls/tls.crt",
            "/etc/holzwave/tls/tls.key",
            "/etc/holzwave/oidc/corp/secret",
            "/certs/cert.pem",
            "/certs/key.pem",
            "/secrets/oidc",
        ]
        .map(PathBuf::from)
        .to_vec(),
        uid: 10_001,
        real_key: false,
    }
}

fn env() -> Env {
    let mut env = Env::new();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), KEY_PATH.to_owned());
    env.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), CONFIG_PATH.to_owned());
    env
}

/// The configuration text through the validator.
fn judge(text: &str) -> Result<config::Prepared, Refusal> {
    prepare(&env(), &probe(text))
}

fn set(value: &mut Value, path: &str, new: Value) {
    let mut at = value;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            at[part] = new;
            return;
        }
        if at.get(part).is_none() {
            at[part] = json!({});
        }
        at = &mut at[part];
    }
}

fn remove(value: &mut Value, path: &str) {
    let (parent, last) = match path.rsplit_once('.') {
        Some((parent, last)) => (parent, last),
        None => ("", path),
    };
    let mut at = value;
    for part in parent.split('.').filter(|p| !p.is_empty()) {
        at = &mut at[part];
    }
    at.as_object_mut().expect("an object").remove(last);
}

// ---------------------------------------------------------------------------------------------------------------------
// The examples
// ---------------------------------------------------------------------------------------------------------------------

/// What is wrong with the four examples of the document, one sentence each. The files, proxy and provider examples must be
/// accepted; the ACME example must be refused for the missing agreement and accepted once the agreement is given.
fn example_problems(config_md: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let files = documented_example(config_md, "server/files.json");
    let acme = documented_example(config_md, "server/acme.json");
    let proxy = documented_example(config_md, "server/proxy.json");
    let oidc = documented_example(config_md, "server/oidc.json");

    match judge(&files) {
        Ok(p) => {
            if !matches!(p.tls, TlsSetup::Files { .. }) {
                problems.push("server/files.json is not the files mode".to_owned());
            }
            if !(p.auth.passkey && p.auth.password_totp) {
                problems.push("server/files.json does not switch both sign-in methods on".to_owned());
            }
        }
        Err(e) => problems.push(format!("server/files.json is refused: {}", e.sentence())),
    }
    match judge(&documented_example(config_md, "server/ceiling.json")) {
        Ok(p) => {
            let ceiling = p.device_access.ceiling();
            if (ceiling.sync, ceiling.remote) != (false, false) {
                problems.push("server/ceiling.json does not close both device features".to_owned());
            }
            if p.devices.idle != std::time::Duration::from_secs(30 * 86_400) {
                problems.push("server/ceiling.json does not set the idle time of a device to 30 days".to_owned());
            }
        }
        Err(e) => problems.push(format!("server/ceiling.json is refused: {}", e.sentence())),
    }
    match judge(&proxy) {
        Ok(p) => {
            if !matches!(&p.tls, TlsSetup::Proxy { trusted_proxies } if trusted_proxies.len() == 2) {
                problems.push("server/proxy.json is not the proxy mode with two networks".to_owned());
            }
        }
        Err(e) => problems.push(format!("server/proxy.json is refused: {}", e.sentence())),
    }

    // The provider entry is merged into the auth section of the files example.
    match serde_json::from_str::<Value>(&files).and_then(|mut base| {
        let entry: Value = serde_json::from_str(&oidc)?;
        set(&mut base, "auth.oidc", json!([entry]));
        Ok(base)
    }) {
        Ok(merged) => match judge(&merged.to_string()) {
            Ok(p) => {
                if p.auth.oidc.len() != 1 || p.auth.oidc[0].id != "corp" {
                    problems.push("server/oidc.json did not become the provider `corp`".to_owned());
                }
            }
            Err(e) => problems.push(format!("server/oidc.json is refused: {}", e.sentence())),
        },
        Err(e) => problems.push(format!("server/oidc.json or server/files.json is not JSON: {e}")),
    }

    // The agreement is the operator's: the shipped example must not carry it.
    match judge(&acme) {
        Ok(_) => problems.push("server/acme.json is accepted without the agreement to the terms".to_owned()),
        Err(e) => {
            let sentence = e.sentence();
            if !(sentence.contains("tls.terms_agreed") && sentence.contains("the server never agrees for you")) {
                problems.push(format!("server/acme.json is refused for another reason: {sentence}"));
            }
        }
    }
    match serde_json::from_str::<Value>(&acme) {
        Ok(mut agreed) => {
            set(&mut agreed, "tls.terms_agreed", json!(true));
            match judge(&agreed.to_string()) {
                Ok(p) => {
                    if !matches!(p.tls, TlsSetup::Acme { .. }) {
                        problems.push("server/acme.json with the agreement is not the acme mode".to_owned());
                    }
                }
                Err(e) => problems.push(format!("server/acme.json with the agreement is refused: {}", e.sentence())),
            }
        }
        Err(e) => problems.push(format!("server/acme.json is not JSON: {e}")),
    }
    problems
}

#[test]
fn every_documented_example_is_judged_by_the_real_validator() {
    let problems = example_problems(&doc("config.md"));
    assert!(problems.is_empty(), "docs/config.md: {problems:?}");
}

#[test]
fn the_example_check_rejects_a_mutated_example() {
    let text = doc("config.md");
    let files = documented_example(&text, "server/files.json");
    let oidc = documented_example(&text, "server/oidc.json");
    let acme = documented_example(&text, "server/acme.json");
    let proxy = documented_example(&text, "server/proxy.json");

    let mutate = |from: &str, to: &str| {
        assert!(text.contains(from), "the mutation target {from:?} is in the document");
        text.replacen(from, to, 1)
    };
    // A plain-HTTP address, a mode that does not exist, a provider on http, an empty proxy list, and an agreement already given.
    let first = format!("<!-- example: server/files.json -->\n```json\n{files}\n```");
    let broken = files.replacen("\"public_url\": \"https://holzwave.example.net\"", "\"public_url\": \"http://holzwave.example.net\"", 1);
    assert_ne!(broken, files);
    let cases = [
        text.replacen(&first, &format!("<!-- example: server/files.json -->\n```json\n{broken}\n```"), 1),
        mutate("\"mode\": \"files\"", "\"mode\": \"none\""),
        mutate("\"idle\": \"30d\"", "\"idle\": \"400d\""),
        mutate(&oidc.lines().nth(3).expect("a line").to_owned(), "  \"issuer\": \"http://login.example.net\","),
        mutate(&proxy, &proxy.replacen("[\"10.0.0.0/8\", \"fd00::/8\"]", "[]", 1)),
        mutate(&acme, &acme.replacen("\"terms_agreed\": false", "\"terms_agreed\": true", 1)),
    ];
    for (n, mutated) in cases.iter().enumerate() {
        assert!(!example_problems(mutated).is_empty(), "mutation {n} was not noticed");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The keys
// ---------------------------------------------------------------------------------------------------------------------

/// The cells of a Markdown table row, split at every `|` that is not escaped, with the outer bars dropped.
fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?;
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut previous = ' ';
    for c in inner.chars() {
        if c == '|' && previous != '\\' {
            out.push(cell.trim().to_owned());
            cell.clear();
        } else {
            cell.push(c);
        }
        previous = c;
    }
    out.push(cell.trim().to_owned());
    Some(out)
}

/// The text of a cell that is one code span, without its delimiters. A span of two backticks may hold single ones.
fn code_span(cell: &str) -> Option<String> {
    if let Some(inner) = cell.strip_prefix("`` ").and_then(|c| c.strip_suffix(" ``")) {
        return Some(inner.to_owned());
    }
    if let Some(inner) = cell.strip_prefix("``").and_then(|c| c.strip_suffix("``")) {
        return Some(inner.to_owned());
    }
    let inner = cell.strip_prefix('`')?.strip_suffix('`')?;
    if inner.contains('`') { None } else { Some(inner.to_owned()) }
}

/// The part of the document from the heading that starts with `heading` to the end of the file.
fn section_from(text: &str, heading: &str) -> String {
    let start = text.find(heading).unwrap_or_else(|| panic!("the document has no {heading:?}"));
    text[start..].to_owned()
}

/// The keys that a document's tables name: the first cell of a row that is one code span.
fn documented_keys(config_md: &str) -> BTreeSet<String> {
    section_from(config_md, "## The server configuration")
        .lines()
        .filter_map(cells)
        .filter_map(|row| row.first().and_then(|c| code_span(c)))
        .collect()
}

/// A configuration whose only fault is an unknown member `zz_not_a_key` at `path` (a dotted path; `auth.oidc[0]` is the
/// first provider), and the keys the loader says it would accept there.
fn accepted_keys_at(base: &Value, path: &str) -> Vec<String> {
    let mut file = base.clone();
    if path == "auth.oidc[0]" {
        set(
            &mut file,
            "auth.oidc",
            json!([{
                "id": "corp", "name": "Corp", "issuer": "https://login.example.net", "client_id": "x",
                "client_secret_file": "/secrets/oidc", "zz_not_a_key": 1
            }]),
        );
    } else if path.is_empty() {
        file["zz_not_a_key"] = json!(1);
    } else {
        set(&mut file, &format!("{path}.zz_not_a_key"), json!(1));
    }
    let sentence = judge(&file.to_string()).expect_err("an unknown member is refused").sentence().to_owned();
    let (_, tail) = sentence.split_once("unknown field `zz_not_a_key`, expected ").unwrap_or_else(|| panic!("no key list in {sentence:?}"));
    let list = tail.split(" at line ").next().expect("a list");
    list.split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

/// Every key the loader reads, named as the document names it.
fn loader_keys() -> BTreeSet<String> {
    let base = json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    });
    let sections = ["tls", "auth", "sessions", "limits", "workers", "upload", "transport", "device_access", "devices"];
    let mut keys = BTreeSet::new();
    for key in accepted_keys_at(&base, "") {
        if !sections.contains(&key.as_str()) {
            keys.insert(key);
        }
    }
    // The tag of the tls section is not in the list of any mode.
    keys.insert("tls.mode".to_owned());
    for mode in ["files", "acme", "proxy"] {
        let mut file = base.clone();
        set(&mut file, "tls", json!({"mode": mode, "zz_not_a_key": 1}));
        let sentence = judge(&file.to_string()).expect_err("refused").sentence().to_owned();
        let (_, tail) = sentence
            .split_once("unknown field `zz_not_a_key`, expected ")
            .unwrap_or_else(|| panic!("no key list in {sentence:?}"));
        for name in tail.split(" at line ").next().expect("a list").split('`').skip(1).step_by(2) {
            keys.insert(format!("tls.{name}"));
        }
    }
    for section in ["auth", "sessions", "limits", "workers", "upload", "transport", "device_access", "devices"] {
        for key in accepted_keys_at(&base, section) {
            // `workers.mode` exists only in a build with the in-process workers; it is not a key of the image.
            if section == "workers" && key == "mode" {
                continue;
            }
            keys.insert(format!("{section}.{key}"));
        }
    }
    for key in accepted_keys_at(&base, "auth.oidc[0]") {
        keys.insert(format!("auth.oidc[].{key}"));
    }
    keys
}

/// What is wrong with the key tables of the document against the keys of the loader.
fn key_problems(config_md: &str, loader: &BTreeSet<String>) -> Vec<String> {
    let documented = documented_keys(config_md);
    let mut problems = Vec::new();
    for key in loader.difference(&documented) {
        problems.push(format!("the loader reads `{key}` and the document does not name it"));
    }
    for key in documented.difference(loader) {
        problems.push(format!("the document names `{key}` and the loader does not read it"));
    }
    problems
}

#[test]
fn the_keys_of_the_loader_are_the_keys_of_the_document() {
    let loader = loader_keys();
    assert!(loader.len() > 70, "the key list looks too short: {loader:?}");
    for must in ["public_url", "tls.terms_agreed", "limits.upload_stall", "workers.helper_path", "auth.oidc[].groups_claim"] {
        assert!(loader.contains(must), "the loader's list lacks {must}");
    }
    let problems = key_problems(&doc("config.md"), &loader);
    assert!(problems.is_empty(), "docs/config.md: {problems:#?}");
}

#[test]
fn the_key_check_notices_a_removed_row_and_a_made_up_one() {
    let loader = loader_keys();
    let text = doc("config.md");
    let removed: String = text
        .lines()
        .filter(|line| !line.starts_with("| `limits.gate_wait` |"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(removed, text, "the row exists to be removed");
    assert_eq!(key_problems(&removed, &loader), ["the loader reads `limits.gate_wait` and the document does not name it"]);

    let added = format!("{text}\n| `limits.no_such_key` | 1 | 1 to 2 | Nothing. |\n");
    assert_eq!(key_problems(&added, &loader), ["the document names `limits.no_such_key` and the loader does not read it"]);
}

/// Every documented key that has a default is set to that default in a valid file: the name and the value must be
/// accepted by the loader. Returns the keys that were refused.
fn default_problems(config_md: &str) -> Vec<String> {
    let mut file = json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    });
    let mut set_keys = Vec::new();
    for row in section_from(config_md, "## The server configuration").lines().filter_map(cells) {
        let (Some(key), Some(default)) = (row.first().and_then(|c| code_span(c)), row.get(1)) else { continue };
        let scoped = ["listen", "data_dir", "runtime_dir"].contains(&key.as_str())
            || ["sessions.", "limits.", "workers.", "upload.", "transport.", "devices."].iter().any(|p| key.starts_with(p));
        let default = default.trim_matches('`');
        if !scoped || default.starts_with("none") || default.is_empty() {
            continue;
        }
        let value = default.parse::<u64>().map_or_else(|_| json!(default), |n| json!(n));
        set(&mut file, &key, value);
        set_keys.push(key);
    }
    let mut problems = Vec::new();
    if set_keys.len() < 40 {
        problems.push(format!("only {} documented defaults were found", set_keys.len()));
    }
    if let Err(e) = judge(&file.to_string()) {
        problems.push(format!("the documented defaults together are refused: {}", e.sentence()));
    }
    problems
}

#[test]
fn every_documented_default_is_accepted_for_its_key() {
    let problems = default_problems(&doc("config.md"));
    assert!(problems.is_empty(), "docs/config.md: {problems:?}");
}

#[test]
fn the_default_check_notices_a_default_outside_its_bound() {
    let text = doc("config.md");
    let broken = text.replacen("| `workers.max_panes` | 64 |", "| `workers.max_panes` | 100000 |", 1);
    assert_ne!(broken, text);
    let problems = default_problems(&broken);
    assert!(problems.iter().any(|p| p.contains("workers.max_panes")), "{problems:?}");
}

// ---------------------------------------------------------------------------------------------------------------------
// The bounds
// ---------------------------------------------------------------------------------------------------------------------

/// The rows of the loader's table (`key`, default, `min to max`) that the document does not carry in exactly that form.
fn bound_problems(config_md: &str) -> Vec<String> {
    let section = section_from(config_md, "## The server configuration");
    let rows: Vec<Vec<String>> = section.lines().filter_map(cells).collect();
    let mut problems = Vec::new();
    for row in limits::all_rows() {
        let found = rows.iter().find(|cells| cells.first().is_some_and(|c| code_span(c).as_deref() == Some(row.key)));
        match found {
            None => problems.push(format!("the document has no row for {}", row.key)),
            Some(cells) => {
                let want_bound = format!("{} to {}", row.min, row.max);
                if cells.get(1).map(|c| c.trim_matches('`')) != Some(row.default.as_str()) {
                    problems.push(format!("{}: the document says the default is {:?}, the code {}", row.key, cells.get(1), row.default));
                }
                if cells.get(2).map(String::as_str) != Some(want_bound.as_str()) {
                    problems.push(format!("{}: the document says the bound is {:?}, the code {want_bound}", row.key, cells.get(2)));
                }
            }
        }
    }
    problems
}

#[test]
fn the_defaults_and_bounds_of_the_document_are_those_of_the_code() {
    assert!(limits::all_rows().len() >= 40, "the loader's table of bounds is not empty");
    let problems = bound_problems(&doc("config.md"));
    assert!(problems.is_empty(), "docs/config.md: {problems:#?}");
}

#[test]
fn the_bound_check_notices_a_changed_default_and_a_changed_bound() {
    let text = doc("config.md");
    let changed_default = text.replacen("| `limits.max_connections` | 1024 |", "| `limits.max_connections` | 2048 |", 1);
    assert_ne!(changed_default, text);
    assert_eq!(bound_problems(&changed_default).len(), 1, "{:?}", bound_problems(&changed_default));
    let changed_bound = text.replacen("| `sessions.idle` | 30m | 1m to 30d |", "| `sessions.idle` | 30m | 1m to 60d |", 1);
    assert_ne!(changed_bound, text);
    assert_eq!(bound_problems(&changed_bound).len(), 1, "{:?}", bound_problems(&changed_bound));
}

// ---------------------------------------------------------------------------------------------------------------------
// The sentences of the guide
// ---------------------------------------------------------------------------------------------------------------------

/// Where the key file of a case comes from.
#[derive(Clone, Copy)]
enum KeyCase {
    /// The fixed key of the stand-in.
    Fixed,
    /// `HOLZWAVE_SERVER_KEY_FILE` is not set.
    NotSet,
    /// A path with no file.
    Missing,
    /// A good file with mode 0644.
    Loose,
    /// A link to a good file.
    Link,
    /// A file that is no key.
    Malformed,
}

/// One refusal of the guide: the sentence as the guide prints it, the change that provokes it, and the pieces of the real
/// sentence that stand for a value of the file (they are replaced by the placeholder the guide uses).
struct Case {
    sentence: &'static str,
    edit: fn(&mut Value),
    key: KeyCase,
    config_path: Option<&'static str>,
    replace: &'static [(&'static str, &'static str)],
}

fn nothing(_: &mut Value) {}

fn acme_tls(value: &mut Value) {
    set(value, "tls", json!({"mode": "acme", "contact": "mailto:admin@example.org", "domains": ["hw.example.test"], "terms_agreed": true}));
}

fn proxy_tls(value: &mut Value, entries: Value) {
    set(value, "tls", json!({"mode": "proxy", "trusted_proxies": entries}));
}

fn oidc_entry(value: &mut Value, patch: &[(&str, Value)]) {
    let mut entry = json!({
        "id": "corp", "name": "Corp", "issuer": "https://login.example.net", "client_id": "x", "client_secret_file": "/secrets/oidc"
    });
    for (key, v) in patch {
        entry[*key] = v.clone();
    }
    set(value, "auth", json!({"oidc": [entry]}));
}

const NO_REPLACE: &[(&str, &str)] = &[];

#[rustfmt::skip]
fn cases() -> Vec<Case> {
    let case = |sentence, edit: fn(&mut Value), replace| Case { sentence, edit, key: KeyCase::Fixed, config_path: None, replace };
    let key_case = |sentence, key| Case { sentence, edit: nothing, key, config_path: None, replace: NO_REPLACE };
    vec![
        Case { sentence: "HOLZWAVE_SERVER_KEY_FILE is not set; create a key file with `holzwave-server keygen --out FILE` and name it in that variable", edit: nothing, key: KeyCase::NotSet, config_path: None, replace: NO_REPLACE },
        Case { sentence: "the configuration file <path> cannot be read (entity not found); name the file in HOLZWAVE_SERVER_CONFIG", edit: nothing, key: KeyCase::Fixed, config_path: Some("/nonexistent/server.json"), replace: &[("/nonexistent/server.json", "<path>")] },
        case("public_url is not set; name the address people type in the browser, such as https://holzwave.example.org", |v| remove(v, "public_url"), NO_REPLACE),
        case("public_url \"<address>\" is not usable: it must start with https:// because passkeys and sign-in cookies need TLS", |v| set(v, "public_url", json!("http://hw.example.test")), &[("http://hw.example.test", "<address>")]),
        case("public_url \"<address>\" has an IP address as its host, but passkeys need a name; use a host name or turn auth.passkey off", |v| set(v, "public_url", json!("https://192.0.2.7")), &[("https://192.0.2.7", "<address>")]),
        case("listen \"<value>\" is not an address and port such as 0.0.0.0:8443", |v| set(v, "listen", json!("everywhere")), &[("\"everywhere\"", "\"<value>\"")]),
        case("the configuration has no tls section; choose a mode: \"files\", \"acme\" or \"proxy\" (the server has no plain-HTTP mode)", |v| remove(v, "tls"), NO_REPLACE),
        case("tls mode \"files\" needs cert_file and key_file", |v| remove(v, "tls.cert_file"), NO_REPLACE),
        case("the certificate file <path> cannot be read; check tls.cert_file", |v| set(v, "tls.cert_file", json!("/certs/missing.pem")), &[("/certs/missing.pem", "<path>")]),
        case("the certificate key file <path> cannot be read; check tls.key_file", |v| set(v, "tls.key_file", json!("/certs/missing.key")), &[("/certs/missing.key", "<path>")]),
        case("tls mode \"acme\" needs tls.terms_agreed set to true: by setting it you, the operator, agree to the terms of service of the certificate authority at https://acme-v02.api.letsencrypt.org/directory; the server never agrees for you", |v| { acme_tls(v); set(v, "tls.terms_agreed", json!(false)); }, NO_REPLACE),
        case("tls mode \"acme\" needs at least one name in tls.domains", |v| { acme_tls(v); set(v, "tls.domains", json!([])); }, NO_REPLACE),
        case("tls mode \"acme\" needs tls.contact, an address such as mailto:admin@example.org", |v| { acme_tls(v); remove(v, "tls.contact"); }, NO_REPLACE),
        case("the host \"<host>\" of public_url is not in tls.domains; the certificate must cover the address people type", |v| { acme_tls(v); set(v, "tls.domains", json!(["www.example.test"])); }, &[("\"hw.example.test\"", "\"<host>\"")]),
        case("tls mode \"proxy\" needs tls.trusted_proxies, the addresses of the proxies allowed to connect; the list is empty", |v| proxy_tls(v, json!([])), NO_REPLACE),
        case("tls.trusted_proxies entry \"<entry>\" is not an address or a network such as 10.0.0.1 or 10.0.0.0/8", |v| proxy_tls(v, json!(["proxy.example.test"])), &[("\"proxy.example.test\"", "\"<entry>\"")]),
        case("tls.trusted_proxies entry \"<entry>\" would trust every address; name the networks of the proxies", |v| proxy_tls(v, json!(["0.0.0.0/0"])), &[("\"0.0.0.0/0\"", "\"<entry>\"")]),
        case("no sign-in method is on; turn on auth.passkey or auth.password_totp, or add a provider to auth.oidc (the server never runs without sign-in)", |v| set(v, "auth", json!({"passkey": false})), NO_REPLACE),
        case("auth.oidc \"<id>\": the client secret file <path> cannot be read; check client_secret_file", |v| oidc_entry(v, &[("client_secret_file", json!("/secrets/none"))]), &[("\"corp\"", "\"<id>\""), ("/secrets/none", "<path>")]),
        case("auth.oidc \"<id>\": issuer must be an https address", |v| oidc_entry(v, &[("issuer", json!("http://login.example.net"))]), &[("\"corp\"", "\"<id>\"")]),
        case("workers.uid_min 0 is below 1000, which could name a system account; use a range of 1000 or more", |v| set(v, "workers.uid_min", json!(0)), NO_REPLACE),
        case("the worker range 10000..10002 contains the user id the server itself runs as (10001); choose a range without it", |v| { set(v, "workers.uid_min", json!(10_000)); set(v, "workers.uid_max", json!(10_002)); }, NO_REPLACE),
        case("limits.max_connections is 0 but must be between 1 and 65536", |v| set(v, "limits.max_connections", json!(0)), NO_REPLACE),
        case("sessions.idle 2d is longer than sessions.absolute 1d", |v| { set(v, "sessions.idle", json!("2d")); set(v, "sessions.absolute", json!("1d")); }, NO_REPLACE),
        case("upload.dir ../elsewhere must be a relative path inside the home, without \"..\" or a leading slash", |v| set(v, "upload.dir", json!("../elsewhere")), NO_REPLACE),
        case("transport.compression is deflate, but compression is not available in this build; it must be off", |v| set(v, "transport.compression", json!("deflate")), NO_REPLACE),
        key_case("the server key file <path> does not exist; check the path or create the file", KeyCase::Missing),
        key_case("the server key file <path> has mode 0644, which is too open; run chmod 0600 on it (a group may read it, nobody else may write or read it)", KeyCase::Loose),
        key_case("the server key file <path> is a link and links are refused; name the file itself", KeyCase::Link),
        key_case("the server key file <path> is not a key made by `holzwave-server keygen`; make a new key file with that command", KeyCase::Malformed),
    ]
}

/// A valid file for the cases: the files mode with passkeys.
fn case_base() -> Value {
    json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    })
}

/// A good key line, as `keygen` writes it.
fn key_line() -> String {
    use base64::Engine;
    format!("hws1:{}\n", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([9u8; 32]))
}

/// The sentence the loader prints for the case, with the values of the file replaced by the guide's placeholders.
fn produced(case: &Case) -> String {
    let dir = Scratch::new("docs-refusals");
    let mut file = case_base();
    (case.edit)(&mut file);
    let mut environment = env();
    let mut replace: Vec<(String, String)> = case.replace.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect();
    let mut real_key = false;
    match case.key {
        KeyCase::Fixed => {}
        KeyCase::NotSet => {
            environment.remove("HOLZWAVE_SERVER_KEY_FILE");
        }
        other => {
            real_key = true;
            let path = match other {
                KeyCase::Missing => dir.join("missing.key"),
                KeyCase::Loose => dir.write_file("loose.key", key_line().as_bytes(), 0o644),
                KeyCase::Link => {
                    let target = dir.write_file("target.key", key_line().as_bytes(), 0o600);
                    let link = dir.join("link.key");
                    symlink(&target, &link).expect("make a link");
                    link
                }
                KeyCase::Malformed => dir.write_file("malformed.key", b"not a key\n", 0o600),
                KeyCase::Fixed | KeyCase::NotSet => unreachable!(),
            };
            let shown = path.to_str().expect("utf-8 path").to_owned();
            environment.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), shown.clone());
            replace.push((shown, "<path>".to_owned()));
        }
    }
    if let Some(path) = case.config_path {
        environment.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), path.to_owned());
    }
    let mut probe = probe(&file.to_string());
    probe.real_key = real_key;
    let sentence = prepare(&environment, &probe)
        .err()
        .unwrap_or_else(|| panic!("the case {:?} must be refused", case.sentence))
        .sentence()
        .to_owned();
    replace.iter().fold(sentence, |s, (from, to)| s.replace(from.as_str(), to))
}

/// The sentences of the table in the guide, in order.
fn guide_sentences(guide: &str) -> Vec<String> {
    section_from(guide, "## When the server refuses to start")
        .lines()
        .filter_map(cells)
        .filter(|row| row.len() == 2)
        .filter_map(|row| code_span(&row[0]))
        .collect()
}

/// What is wrong with the table of refusals in the guide against what the loader prints.
fn refusal_problems(guide: &str) -> Vec<String> {
    let table = guide_sentences(guide);
    let mut problems = Vec::new();
    for case in cases() {
        if !table.iter().any(|s| s == case.sentence) {
            problems.push(format!("the guide does not hold the sentence {:?}", case.sentence));
        }
        let real = produced(&case);
        if real != case.sentence {
            problems.push(format!("the loader prints {real:?} where the guide says {:?}", case.sentence));
        }
    }
    let known: BTreeSet<&str> = cases().iter().map(|c| c.sentence).collect();
    for sentence in &table {
        if !known.contains(sentence.as_str()) {
            problems.push(format!("the guide holds the sentence {sentence:?}, which no case provokes"));
        }
    }
    problems
}

#[test]
fn the_refusal_table_of_the_guide_holds_exactly_the_sentences_of_the_loader() {
    let guide = doc("server.md");
    assert!(guide_sentences(&guide).len() >= 30, "the table of the guide was not read");
    let problems = refusal_problems(&guide);
    assert!(problems.is_empty(), "docs/server.md: {problems:#?}");
}

#[test]
fn the_refusal_check_notices_a_changed_sentence_a_removed_row_and_a_made_up_one() {
    let guide = doc("server.md");
    let changed = guide.replacen("the server never agrees for you", "the server agrees for you", 1);
    assert_ne!(changed, guide);
    assert!(!refusal_problems(&changed).is_empty(), "a changed sentence");

    let removed: String = guide.lines().filter(|l| !l.starts_with("| `limits.max_connections is 0")).collect::<Vec<_>>().join("\n");
    assert_ne!(removed, guide);
    assert!(!refusal_problems(&removed).is_empty(), "a removed row");

    let extra = guide.replacen(
        "| `sessions.idle 2d is longer",
        "| `the server is angry` | Calm it. |\n| `sessions.idle 2d is longer",
        1,
    );
    assert_ne!(extra, guide);
    assert!(!refusal_problems(&extra).is_empty(), "a made-up row");
}

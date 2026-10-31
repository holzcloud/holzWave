//! The canary suite: secrets exist only as ciphertext inside the person's store file, a wrong key or a damaged file fails
//! closed, two writers keep both values, and locked background work reports the fixed sentence.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/unlock_env.rs"]
mod unlock_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::path::{Path, PathBuf};
use std::time::Duration;

use holzwave_server::http::app::StoreState;
use holzwave_server::unlock::{Kek, kek_from_recovery, parse_recovery_code};
use holzwave_server::worker::secretstore::{EncryptedFileStore, SlotKind};
use holzwave_core::secrets::SecretStore;
use serde_json::{Value, json};
use unlock_env::{PASSWORD, Region, UnlockEnv, User, ai_has_key, ai_key_set, ai_profile, integration_profile, integration_secret_set};
use ws_env::focus;

const LOCKED: &str = "secrets are locked";

/// A fixed key whose bytes must never appear in any `Debug` text.
const KNOWN_KEY: [u8; 32] = [0xC7; 32];

/// A random text for a canary: it exists only in this run.
fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("random bytes");
    unlock_env::hex(&buf)
}

/// Whether a path below one of the roots is a file the server is allowed to write. A file nobody listed here is a file
/// something wrote outside the known places.
fn expected_file(relative: &str) -> bool {
    let parts: Vec<&str> = relative.split('/').collect();
    match parts.as_slice() {
        ["server", "audit", name] => name.starts_with("audit-") && name.ends_with(".jsonl"),
        ["server", name] => ["server.sqlite3", "server.sqlite3-shm", "server.sqlite3-wal"].contains(name),
        // The key and the certificate of the test listener, made by the test environment.
        ["cert.pem"] | ["key.pem"] => true,
        ["homes", uid, ".holzwave", name] => {
            uid.chars().all(|c| c.is_ascii_digit())
                && [
                    "ai.json",
                    "history.db",
                    "history.db-shm",
                    "history.db-wal",
                    "secrets.store",
                    "secrets.store.lock",
                    "session.json",
                    "settings.json",
                    "integrations.json",
                ]
                .contains(name)
        }
        _ => false,
    }
}

/// The files below the roots of the volume that are not on the list, relative to their root.
fn unexpected_files(env: &UnlockEnv) -> Vec<String> {
    let mut odd = Vec::new();
    for root in env.volume() {
        for file in unlock_env::files_below(&root) {
            let relative = file.strip_prefix(&root).expect("below").to_string_lossy().into_owned();
            if !expected_file(&relative) {
                odd.push(relative);
            }
        }
    }
    odd
}

/// The key that opens the recovery slot of the person, derived the way the gate does.
fn recovery_kek(env: &UnlockEnv, user: &User) -> holzwave_server::unlock::Kek {
    let rows = env.ws.test.store.read(|tx| tx.unlock_list(user.id)).expect("unlock rows");
    let row = rows.iter().find(|r| r.kind == holzwave_server::store::UnlockKind::Recovery).expect("a recovery row");
    kek_from_recovery(&parse_recovery_code(&user.recovery).expect("the code parses"), &row.salt).expect("the key")
}

/// Opens the store file of the person from a new handle with the recovery key: the file as the next process would find it.
fn fresh_open(env: &UnlockEnv, user: &User) -> EncryptedFileStore {
    let store = EncryptedFileStore::open(&env.store_file(user)).expect("the store file is whole");
    store.unlock(SlotKind::Recovery, &recovery_kek(env, user)).expect("the recovery key opens it");
    store
}

fn tree(root: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = unlock_env::files_below(root)
        .iter()
        .map(|p| (p.strip_prefix(root).expect("below").to_string_lossy().into_owned(), std::fs::metadata(p).map_or(0, |m| m.len())))
        .collect();
    out.sort();
    out
}

/// One struct that derives `Debug` and has a field that may hold a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub name: String,
}

const WORDS: [&str; 8] = ["key", "secret", "token", "password", "kek", "dek", "sealed", "zeroizing"];

fn code_of(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
}

/// Every struct in `source` that derives `Debug` and has a field whose name or type contains one of the words. Simple line and
/// brace tracking, not a parser: it over-reports (a word inside a longer name counts) and does not under-report.
pub fn derive_debug_findings(file: &str, source: &str) -> Vec<Finding> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut derives_debug = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.starts_with("#[") {
            if line.contains("derive(") && line.contains("Debug") {
                derives_debug = true;
            }
            i += 1;
            continue;
        }
        if line.starts_with("///") || line.starts_with("//") || line.is_empty() {
            i += 1;
            continue;
        }
        let words: Vec<&str> = code_of(line).split_whitespace().collect();
        let at = words.iter().position(|w| *w == "struct");
        if let (Some(at), true) = (at, derives_debug) {
            let name: String =
                words.get(at + 1).map_or(String::new(), |n| n.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect());
            // The body: to the closing brace of a braced struct, or to the semicolon of a tuple struct.
            let mut depth: i32 = 0;
            let mut opened = false;
            let mut body = String::new();
            let start = i;
            let mut j = i;
            'body: while j < lines.len() {
                let code = code_of(lines[j]);
                body.push_str(code);
                body.push('\n');
                for c in code.chars() {
                    match c {
                        '{' | '(' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' | ')' => depth -= 1,
                        _ => {}
                    }
                }
                if (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';')) {
                    break 'body;
                }
                j += 1;
            }
            // Only the fields: what follows the name.
            let after_name = body.split_once(&name).map_or(body.as_str(), |(_, rest)| rest).to_ascii_lowercase();
            if WORDS.iter().any(|w| after_name.contains(w)) {
                found.push(Finding { file: file.to_owned(), line: start + 1, name });
            }
            derives_debug = false;
            i = j + 1;
            continue;
        }
        derives_debug = false;
        i += 1;
    }
    found
}

/// Structs that derive `Debug` and have a field that matches the words, with the reason each is safe. An entry that matches no
/// finding fails the test, so the list cannot go stale.
const ALLOWED: [(&str, &str, &str); 15] = [
    ("accounts.rs", "Issued", "the field is a `Token`, whose Debug prints `Token(..)`"),
    ("audit.rs", "Event", "the word is `DetailKey`, the closed set of names of detail members; no value is a free string"),
    ("auth/password.rs", "Verified", "the field is a `Kek`, whose Debug prints a marker"),
    ("auth/session.rs", "Created", "the field is a `SessionToken`, whose Debug prints `SessionToken(..)`"),
    ("config.rs", "Auth", "the word is the flag `passkey`: a boolean that turns the method on"),
    ("config.rs", "Oidc", "`client_secret_file` is the path of a file, never its content"),
    ("config.rs", "Prepared", "the field is a `ServerKey`, whose Debug prints a marker"),
    ("http/ws.rs", "Bucket", "`tokens` counts the commands a socket may still send"),
    ("limits.rs", "Spec", "the word is the key of a setting in the file"),
    ("limits.rs", "Row", "the word is the key of a setting in the file"),
    ("store.rs", "Diagnostics", "the words are the SQLite settings `foreign_keys` and `secure_delete`"),
    ("store.rs", "Passkey", "`passkey_json` and the credential id are the public part of a credential; no private key is kept"),
    ("supervisor/launcher.rs", "LaunchRequest", "the word is `AccountKey`, the pair of an account number and a user id"),
    ("wire.rs", "SlotKey", "the field is a wire `Key`, whose Debug prints `Key(..)`"),
    ("wire.rs", "OpeningKey", "the field is a wire `Key`, whose Debug prints `Key(..)`"),
];

/// A directory below the system temporary directory that is removed when it goes out of scope.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("hw-canary-{tag}-{}-{}", std::process::id(), random_hex(6)));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write_file(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("a scratch file");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn all_sources() -> Vec<(String, String)> {
    let root = src_dir();
    let mut out = Vec::new();
    for path in unlock_env::files_below(&root) {
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&path).expect("a source file");
            out.push((path.strip_prefix(&root).expect("below").to_string_lossy().into_owned(), text));
        }
    }
    out
}

// ---- the scanners prove themselves --------------------------------------------------------------------------------------

#[test]
fn scanner_finds_a_planted_sample_in_every_encoding_and_nothing_in_a_clean_tree() {
    let dir = Scratch::new("scan-self");
    let needle = "canary-Z9x/+=value 42";
    let forms = unlock_env::encodings(needle);
    assert_eq!(forms.len(), 5, "plain, base64, base64url, hex and percent");
    for (i, (how, bytes)) in forms.iter().enumerate() {
        let mut content = b"some bytes before ".to_vec();
        content.extend_from_slice(bytes);
        content.extend_from_slice(b" and after");
        dir.write_file(&format!("planted-{i}-{how}"), &content);
    }
    dir.write_file("clean", b"nothing to see in this file");
    let (hits, read) = unlock_env::scan_volume_for(&[dir.path().to_path_buf()], &[("sample", needle)]);
    assert_eq!(read, 6);
    // Every planted file is found, once for the encoding it holds (the plain form is also a part of no other).
    for (i, (how, _)) in forms.iter().enumerate() {
        let name = format!("planted-{i}-{how}");
        assert!(hits.iter().any(|h| h.path.ends_with(&name) && h.needle.contains(how)), "{name} is found: {hits:?}");
    }
    assert!(hits.iter().all(|h| !h.path.ends_with("clean")));
    assert!(hits.iter().all(|h| !h.needle.contains(needle)), "a hit never shows the needle");
    let clean = Scratch::new("scan-clean");
    clean.write_file("a", b"nothing");
    assert!(unlock_env::scan_volume_for(&[clean.path().to_path_buf()], &[("sample", needle)]).0.is_empty());
    assert_eq!(unlock_env::scan_text_for(&format!("log line {needle} end"), &[("sample", needle)]), ["sample (plain)"]);
    assert!(unlock_env::scan_text_for("a clean line", &[("sample", needle)]).is_empty());
}

#[test]
fn source_scan_fails_a_sample_that_derives_debug_over_a_secret_and_passes_a_manual_impl() {
    let bad = "/// A thing.\n#[derive(Debug, Clone)]\npub struct Holder {\n    /// The key.\n    pub api_key: String,\n    pub n: u32,\n}\n";
    assert_eq!(derive_debug_findings("sample.rs", bad).len(), 1, "a derived Debug over a key is found");
    let tuple = "#[derive(Debug)]\npub struct Wrapper(Zeroizing<String>);\n";
    assert_eq!(derive_debug_findings("sample.rs", tuple).len(), 1, "a tuple struct is found too");
    let manual = "pub struct Holder {\n    pub api_key: String,\n}\nimpl fmt::Debug for Holder {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(\"Holder(..)\") }\n}\n";
    assert!(derive_debug_findings("sample.rs", manual).is_empty(), "a hand-written Debug is the goal");
    let harmless = "#[derive(Debug)]\npub struct Plain {\n    pub name: String,\n}\n";
    assert!(derive_debug_findings("sample.rs", harmless).is_empty());
    let in_a_comment = "#[derive(Debug)]\npub struct Plain {\n    // the key is elsewhere\n    pub name: String,\n}\n";
    assert!(derive_debug_findings("sample.rs", in_a_comment).is_empty());
}

#[test]
fn source_scan_every_struct_that_holds_a_secret_prints_a_marker_or_has_a_reason() {
    let mut found = Vec::new();
    for (file, text) in all_sources() {
        found.extend(derive_debug_findings(&file, &text));
    }
    let unlisted: Vec<String> = found
        .iter()
        .filter(|f| !ALLOWED.iter().any(|(file, name, _)| *file == f.file && *name == f.name))
        .map(|f| format!("{}:{} {}", f.file, f.line, f.name))
        .collect();
    assert!(unlisted.is_empty(), "derive(Debug) over a key, secret, token or password: {unlisted:?}");
    let stale: Vec<&str> = ALLOWED
        .iter()
        .filter(|(file, name, _)| !found.iter().any(|f| f.file == *file && f.name == *name))
        .map(|(_, name, _)| *name)
        .collect();
    assert!(stale.is_empty(), "entries of the list that match nothing: {stale:?}");
    assert!(ALLOWED.iter().all(|(_, _, reason)| reason.len() > 20), "every entry has its reason");
    println!("source scan: {} structs derive Debug over a word, all {} listed with a reason", found.len(), ALLOWED.len());
}


// ---- the canary run -----------------------------------------------------------------------------------------------------

/// The canary of one run: a text that is built from the seed and a label, so that a child process and its parent make the
/// same ones, and that no other run and no source file contains.
fn canary(seed: &str, label: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, format!("{seed}/{label}").as_bytes());
    format!("cn-{label}-{}", &unlock_env::hex(digest.as_ref())[..24])
}

/// Everything a run leaves behind that a test looks at.
struct Run {
    env: UnlockEnv,
    alice: User,
    bob: User,
    /// Every secret of the run: name and text.
    needles: Vec<(String, String)>,
    /// The cookies of every session the run made (the last ones are still live).
    cookies: Vec<String>,
}

impl Run {
    fn needle_refs(&self) -> Vec<(&str, &str)> {
        self.needles.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect()
    }

    /// Ends every session of the run and waits for both stores to lock.
    async fn sign_everyone_out(&self) {
        for cookie in &self.cookies {
            // A session that ended on its own (a password change ends the others) needs no sign-out.
            if self.env.ws.test.sessions.lookup(cookie, self.env.now()).is_ok() {
                let _ = self.env.sign_out(&self.env.signed_from(cookie)).await;
            }
        }
        self.env.wait_state(&self.alice, StoreState::Locked, "the last sign-out of alice").await;
        self.env.wait_state(&self.bob, StoreState::Locked, "the last sign-out of bob").await;
    }
}

async fn set_passphrase(env: &UnlockEnv, who: &ws_env_signed::Signed, text: &str) {
    let reply = env.api(who, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": text}))).await;
    assert_eq!(reply.status, 204, "{}", reply.text());
}

async fn put_secrets(env: &UnlockEnv, who: &ws_env_signed::Signed, assistant: &str, integration: &str) -> String {
    let mut socket = env.socket(who).await;
    let profile = ai_profile(&mut socket).await;
    ai_key_set(&mut socket, &profile, assistant).await.expect("the assistant key is saved");
    integration_profile(&mut socket, "prod", &["TOKEN".to_owned()]).await;
    integration_secret_set(&mut socket, "prod", "TOKEN", integration).await.expect("the integration secret is saved");
    profile
}

mod ws_env_signed {
    pub use crate::edge::Signed;
}

/// The whole life of two people's secrets: enrolment with a canary password, a canary passphrase and the generated recovery
/// code; a canary assistant key and a canary integration secret over a real socket; sign-out and sign-in; unlock by
/// passphrase and by recovery code; a new password; a new passphrase; the administrator's reset; the next sign-in.
async fn canary_run(seed: &str) -> Run {
    let c = |label: &str| canary(seed, label);
    let env = UnlockEnv::start().await;
    let mut needles: Vec<(String, String)> = Vec::new();
    let mut cookies: Vec<String> = Vec::new();
    let add = |name: &str, text: String, needles: &mut Vec<(String, String)>| needles.push((name.to_owned(), text));

    let (alice, a1, _) = env.bootstrap("alice", &c("alice-password")).await;
    let (bob, b1, _) = env.invite(&alice, "bob", &c("bob-password")).await;
    for (name, user) in [("alice", &alice), ("bob", &bob)] {
        add(&format!("{name} password"), user.password.clone(), &mut needles);
        add(&format!("{name} recovery code"), user.recovery.clone(), &mut needles);
        add(&format!("{name} recovery code (compact)"), user.recovery.replace(['-', ' '], ""), &mut needles);
        add(&format!("{name} recovery code (lower case)"), user.recovery.to_lowercase(), &mut needles);
    }
    for (label, signed) in [("a1", &a1), ("b1", &b1)] {
        add(&format!("session cookie {label}"), signed.cookie.clone(), &mut needles);
        add(&format!("anti-forgery token {label}"), signed.csrf.clone(), &mut needles);
        cookies.push(signed.cookie.clone());
    }

    for name in ["alice", "bob"] {
        let who = if name == "alice" { &a1 } else { &b1 };
        set_passphrase(&env, who, &c(&format!("{name}-passphrase"))).await;
        put_secrets(&env, who, &c(&format!("{name}-assistant-key")), &c(&format!("{name}-integration"))).await;
        for label in ["passphrase", "assistant-key", "integration"] {
            add(&format!("{name} {label}"), c(&format!("{name}-{label}")), &mut needles);
        }
    }

    // Sign out and in again: the key leaves the memory and comes back from the file.
    let a2 = env.sign_in(&alice).await;
    cookies.push(a2.cookie.clone());
    env.sign_out(&a1).await;
    env.sign_out(&b1).await;
    env.wait_state(&bob, StoreState::Locked, "the sign-out of bob").await;
    let b2 = env.sign_in(&bob).await;
    cookies.push(b2.cookie.clone());
    assert_eq!(env.state(&bob), StoreState::Open);
    assert_eq!(env.state(&alice), StoreState::Open);

    // Unlock by passphrase and by recovery code.
    env.lock_at_the_worker(&alice).await;
    assert_eq!(env.unlock_passphrase(&a2, &c("alice-passphrase")).await.status, 200);
    env.lock_at_the_worker(&alice).await;
    assert_eq!(env.unlock_recovery(&a2, &alice.recovery).await.status, 200);
    assert_eq!(env.state(&alice), StoreState::Open);

    // A new password and a new passphrase.
    let new_password = c("alice-password-new");
    let body = json!({"current": c("alice-password"), "new": new_password});
    assert_eq!(env.api(&a2, "POST", "/api/me/password", Some(&body)).await.status, 204);
    add("alice new password", new_password.clone(), &mut needles);
    let mut alice_new = alice.clone();
    alice_new.password = new_password;
    let a3 = env.sign_in(&alice_new).await;
    cookies.push(a3.cookie.clone());
    set_passphrase(&env, &a3, &c("alice-passphrase-new")).await;
    add("alice new passphrase", c("alice-passphrase-new"), &mut needles);
    assert_eq!(env.state(&alice), StoreState::Open);

    // The administrator's reset of bob, and bob's next sign-in, which makes a new store from his password.
    assert_eq!(env.reset_secrets(&a3, &bob).await.status, 204);
    let b3 = env.sign_in(&bob).await;
    cookies.push(b3.cookie.clone());
    for (label, signed) in [("a2", &a2), ("b2", &b2), ("a3", &a3), ("b3", &b3)] {
        add(&format!("session cookie {label}"), signed.cookie.clone(), &mut needles);
        add(&format!("anti-forgery token {label}"), signed.csrf.clone(), &mut needles);
    }
    Run { env, alice, bob, needles, cookies }
}

fn assert_no_hits(what: &str, hits: &[unlock_env::Hit]) {
    let shown: Vec<String> = hits.iter().map(|h| format!("{} at {} ({})", h.path.display(), h.offset, h.needle)).collect();
    assert!(hits.is_empty(), "{what}: {shown:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn canary_run_no_file_of_the_volume_holds_a_secret_in_any_of_the_five_encodings() {
    let run = canary_run(&random_hex(8)).await;
    assert!(run.needles.len() >= 24, "{} needles", run.needles.len());
    // While the people are signed in, the keys are in memory and the files hold ciphertext.
    let (hits, read) = unlock_env::scan_volume_for(&run.env.volume(), &run.needle_refs());
    assert!(read >= 8, "only {read} files were read");
    assert_no_hits("signed in", &hits);
    // After everybody left and the workers ended.
    run.sign_everyone_out().await;
    run.env.restart_worker(&run.alice).await;
    run.env.restart_worker(&run.bob).await;
    let (hits, read_after) = unlock_env::scan_volume_for(&run.env.volume(), &run.needle_refs());
    assert_no_hits("after the sign-out", &hits);
    // The scan is not blind: the same secret planted in the home of alice is found, and only there.
    let planted = run.env.home(&run.alice).join(".holzwave").join("planted.txt");
    std::fs::write(&planted, format!("x {} y", run.needles[0].1)).expect("a planted file");
    let (hits, _) = unlock_env::scan_volume_for(&run.env.volume(), &run.needle_refs());
    std::fs::remove_file(&planted).expect("remove the planted file");
    // (A text of letters and digits is the same in the plain and the percent form, so it is found in both.)
    assert!(!hits.is_empty() && hits.len() <= 2, "{hits:?}");
    assert!(hits.iter().all(|h| h.path.ends_with("planted.txt") && h.needle.starts_with(&run.needles[0].0)), "{hits:?}");
    // The files that matter were scanned: the store of each person is among them and is made of ciphertext.
    for user in [&run.alice, &run.bob] {
        let bytes = run.env.store_bytes(user).expect("the store file");
        assert!(bytes.starts_with(b"HWSS"), "the magic");
        assert!(bytes.len() > 100, "{} bytes", bytes.len());
    }
    println!("canary run: {} secrets in 5 encodings, {read} files read while signed in, {read_after} after the sign-out, 0 hits", run.needles.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn canary_run_the_server_writes_only_the_files_it_knows_and_keeps_the_store_private() {
    let run = canary_run(&random_hex(8)).await;
    assert_eq!(unexpected_files(&run.env), Vec::<String>::new());
    run.sign_everyone_out().await;
    assert_eq!(unexpected_files(&run.env), Vec::<String>::new());
    // The check is not blind either.
    let stray = run.env.home(&run.alice).join("stray.txt");
    std::fs::write(&stray, b"x").expect("a stray file");
    assert_eq!(unexpected_files(&run.env), vec![format!("homes/{}/stray.txt", run.alice.key.uid)]);
    std::fs::remove_file(&stray).expect("remove");
    // The store and its lock are private to the person.
    use std::os::unix::fs::PermissionsExt;
    for user in [&run.alice, &run.bob] {
        let store = run.env.store_file(user);
        for path in [store.clone(), store.with_extension("store.lock")] {
            if let Ok(meta) = std::fs::metadata(&path) {
                assert_eq!(meta.permissions().mode() & 0o077, 0, "{} is private", path.display());
            }
        }
    }
}

/// The child half of the log capture: it runs the canary run and writes the secrets to a file the parent named. Without the
/// variables it does nothing.
#[tokio::test(flavor = "multi_thread")]
async fn child_canary_run() {
    let (Some(seed), Some(sink)) = (std::env::var_os("HW_CANARY_SEED"), std::env::var_os("HW_CANARY_SINK")) else {
        return;
    };
    let run = canary_run(&seed.to_string_lossy()).await;
    run.sign_everyone_out().await;
    let lines: Vec<String> = run.needles.iter().map(|(n, t)| format!("{n}\t{t}")).collect();
    std::fs::write(sink, lines.join("\n")).expect("write the secrets for the parent");
    println!("child canary run finished");
}

#[test]
fn log_capture_standard_output_and_error_of_a_whole_canary_run_hold_no_secret() {
    let seed = random_hex(8);
    let dir = Scratch::new("child");
    let sink = dir.path().join("secrets.txt");
    let exe = std::env::current_exe().expect("the test binary");
    let output = std::process::Command::new(exe)
        .args(["--exact", "child_canary_run", "--nocapture", "--test-threads=1"])
        .env("HW_CANARY_SEED", &seed)
        .env("HW_CANARY_SINK", &sink)
        .env("RUST_BACKTRACE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the child process");
    let captured = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(output.status.success(), "the child failed: {}", captured.chars().take(2000).collect::<String>());
    assert!(captured.contains("child canary run finished"), "the child ran the whole run");
    let text = std::fs::read_to_string(&sink).expect("the secrets of the child");
    let owned: Vec<(String, String)> = text.lines().filter_map(|l| l.split_once('\t')).map(|(n, t)| (n.to_owned(), t.to_owned())).collect();
    assert!(owned.len() >= 24, "{} secrets", owned.len());
    let refs: Vec<(&str, &str)> = owned.iter().map(|(n, t)| (n.as_str(), t.as_str())).collect();
    let hits = unlock_env::scan_text_for(&captured, &refs);
    assert!(hits.is_empty(), "the log holds: {hits:?}");
    // The capture sees what the server writes to standard error, and the scan sees a secret in it.
    let planted = format!("{captured}\nwarn x password={}", owned[0].1);
    assert!(!unlock_env::scan_text_for(&planted, &refs).is_empty());
    println!("log capture: {} bytes of standard output and error, {} secrets, 0 hits", captured.len(), owned.len());
}

#[test]
fn log_events_of_the_server_never_name_a_secret() {
    let mut calls = Vec::new();
    for (file, text) in all_sources() {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("log::event(") {
                continue;
            }
            let mut call = String::new();
            for next in &lines[i..lines.len().min(i + 6)] {
                call.push_str(next);
                if next.contains(");") {
                    break;
                }
            }
            calls.push((file.clone(), i + 1, call));
        }
    }
    assert!(calls.len() >= 30, "{} call sites were found", calls.len());
    let words = ["password", "passphrase", "recovery", "kek", "secret", "token", "cookie", "csrf", "prf"];
    let mut bad = Vec::new();
    for (file, line, call) in &calls {
        let lower = call.to_ascii_lowercase();
        // `slot.key` is the pair of an account number and a user id.
        let lower = lower.replace("slot.key.id", "slot.account");
        if words.iter().any(|w| lower.contains(w)) || lower.contains(".key") || lower.contains("(\"key\"") {
            bad.push(format!("{file}:{line}"));
        }
    }
    assert!(bad.is_empty(), "log events that may carry a secret: {bad:?}");
}

#[test]
fn debug_table_every_type_that_holds_key_bytes_prints_a_marker_in_both_forms() {
    use holzwave_server::auth::password::Verified;
    use holzwave_server::keyfile::ServerKey;
    use holzwave_server::store::{AccountId, PasswordCredential, TotpSecret};
    use holzwave_server::unlock::generate_recovery_code;
    use holzwave_server::wire::{Ctl, Key, OpeningKey, SlotKey};

    fn leaks(text: &str) -> bool {
        let raw = KNOWN_KEY;
        let b64 = base64_of(&raw);
        let lower = unlock_env::hex(&raw);
        text.contains(&lower)
            || text.contains(&lower.to_uppercase())
            || text.contains(&b64.0)
            || text.contains(&b64.1)
            || text.contains("199, 199")
            || text.contains("0xc7")
            || text.contains("0xC7")
            || text.contains("199,\n")
    }
    fn base64_of(raw: &[u8]) -> (String, String) {
        let forms = unlock_env::encodings(&String::from_utf8_lossy(raw));
        let _ = forms;
        use base64::Engine as _;
        (base64::engine::general_purpose::STANDARD.encode(raw), base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
    }
    // The check itself: a type that derives Debug over the bytes is caught in both forms.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct Leaky([u8; 32]);
    assert!(leaks(&format!("{:?}", Leaky(KNOWN_KEY))));
    assert!(leaks(&format!("{:#?}", Leaky(KNOWN_KEY))));
    assert!(leaks(&format!("{}", unlock_env::hex(&KNOWN_KEY))));
    assert!(!leaks("Key(..)"));

    let key = Key::from_bytes(&KNOWN_KEY);
    let slot = SlotKey { kind: SlotKind::Password, id: [0x11; 16], key: key.clone() };
    let opening = OpeningKey { kind: SlotKind::Recovery, key: key.clone() };
    let recovery = generate_recovery_code().expect("a code");
    let recovery_text = recovery.display();
    let parsed = parse_recovery_code(&recovery_text).expect("it parses");
    let table: Vec<(&str, String, String)> = vec![
        ("wire Key", format!("{key:?}"), format!("{key:#?}")),
        ("Kek", format!("{:?}", Kek::from_bytes(&KNOWN_KEY)), format!("{:#?}", Kek::from_bytes(&KNOWN_KEY))),
        ("SlotKey", format!("{slot:?}"), format!("{slot:#?}")),
        ("OpeningKey", format!("{opening:?}"), format!("{opening:#?}")),
        ("Ctl::Unlock", format!("{:?}", Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key: key.clone() }), format!("{:#?}", Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key: key.clone() })),
        ("Ctl::Provision", format!("{:?}", Ctl::Provision { ctl: 2, slots: vec![slot.clone()] }), format!("{:#?}", Ctl::Provision { ctl: 2, slots: vec![slot.clone()] })),
        (
            "Ctl::SetSlot",
            format!("{:?}", Ctl::SetSlot { ctl: 3, slot: slot.clone(), opening: Some(opening.clone()) }),
            format!("{:#?}", Ctl::SetSlot { ctl: 3, slot: slot.clone(), opening: Some(opening.clone()) }),
        ),
        ("ServerKey", format!("{:?}", ServerKey::from_bytes(KNOWN_KEY).expect("a key")), format!("{:#?}", ServerKey::from_bytes(KNOWN_KEY).expect("a key"))),
        ("RecoveryCode", format!("{parsed:?}"), format!("{parsed:#?}")),
        (
            "Verified",
            format!("{:?}", Verified { account: AccountId::from_raw(1), kek: Kek::from_bytes(&KNOWN_KEY) }),
            format!("{:#?}", Verified { account: AccountId::from_raw(1), kek: Kek::from_bytes(&KNOWN_KEY) }),
        ),
        (
            "PasswordCredential",
            format!("{:?}", PasswordCredential { verifier: KNOWN_KEY.to_vec(), salt: KNOWN_KEY.to_vec(), params: String::new(), changed_at: 0 }),
            format!("{:#?}", PasswordCredential { verifier: KNOWN_KEY.to_vec(), salt: KNOWN_KEY.to_vec(), params: String::new(), changed_at: 0 }),
        ),
        (
            "TotpSecret",
            format!("{:?}", TotpSecret { sealed_secret: KNOWN_KEY.to_vec(), last_step: 7 }),
            format!("{:#?}", TotpSecret { sealed_secret: KNOWN_KEY.to_vec(), last_step: 7 }),
        ),
    ];
    for (name, compact, pretty) in &table {
        for text in [compact, pretty] {
            assert!(!leaks(text), "{name} prints key bytes: {text}");
            assert!(!text.contains(&*recovery_text), "{name} prints the recovery code");
            assert!(text.len() < 400, "{name} prints {} bytes", text.len());
        }
    }
    println!("debug table: {} types, both forms, no key byte in any of them", table.len());
}

// ---- fail closed --------------------------------------------------------------------------------------------------------

/// What every secret command answers while the store is locked, and the proof that it wrote nothing.
async fn assert_commands_fail_closed(env: &UnlockEnv, who: &crate::edge::Signed, user: &User, what: &str) {
    let store_before = env.store_sha256(user);
    let names_before: Vec<String> = tree(&env.home(user)).into_iter().map(|(n, _)| n).collect();
    let mut socket = env.socket(who).await;
    let profile = ai_profile(&mut socket).await;
    assert_eq!(ai_key_set(&mut socket, &profile, "sk-must-not-land").await, Err(LOCKED.to_owned()), "{what}: the assistant key");
    integration_profile(&mut socket, "prod", &["TOKEN".to_owned()]).await;
    let refused = integration_secret_set(&mut socket, "prod", "TOKEN", "must-not-land").await;
    assert_eq!(refused, Err(LOCKED.to_owned()), "{what}: the integration secret");
    drop(socket);
    assert_eq!(env.store_sha256(user), store_before, "{what}: the store file is as it was");
    let names_after: Vec<String> = tree(&env.home(user)).into_iter().map(|(n, _)| n).collect();
    // The profile of the assistant and of the integration are settings, not secrets: they may be written, and no file of the
    // secrets is: no new store, no temporary copy of one.
    let created: Vec<&String> = names_after
        .iter()
        .filter(|n| !names_before.contains(n))
        .filter(|n| !n.ends_with("/ai.json") && !n.ends_with("/integrations.json"))
        .collect();
    assert!(created.is_empty(), "{what}: the commands made files: {created:?}");
    let (hits, _) = unlock_env::scan_volume_for(&env.volume(), &[("assistant key", "sk-must-not-land"), ("integration secret", "must-not-land")]);
    assert_no_hits(what, &hits);
}

/// A person with a passphrase, a canary assistant key and a canary integration secret, signed out, the worker ended.
async fn person_with_secrets(env: &UnlockEnv) -> (User, String, String, String) {
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    set_passphrase(env, &first, unlock_env_passphrase()).await;
    let (assistant, integration) = (canary("fail", "assistant"), canary("fail", "integration"));
    let profile = put_secrets(env, &first, &assistant, &integration).await;
    env.sign_out(&first).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    env.restart_worker(&alice).await;
    (alice, assistant, integration, profile)
}

fn unlock_env_passphrase() -> &'static str {
    "a passphrase of several words"
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_closed_a_changed_byte_in_the_header_a_slot_or_the_body_locks_every_secret_command() {
    for region in [Region::Header, Region::Slot, Region::Body] {
        let env = UnlockEnv::start().await;
        let (alice, assistant, integration, _profile) = person_with_secrets(&env).await;
        env.flip_byte(&alice, region);
        let damaged = env.store_sha256(&alice);
        let session = env.sign_in(&alice).await;
        assert_eq!(env.state(&alice), StoreState::Locked, "{region:?}");
        assert_commands_fail_closed(&env, &session, &alice, &format!("{region:?}")).await;
        // No other way in: neither the passphrase nor the recovery code opens a file that is damaged where they need it.
        let by_passphrase = env.unlock_passphrase(&session, unlock_env_passphrase()).await.status;
        let by_recovery = env.unlock_recovery(&session, &alice.recovery).await.status;
        if region == Region::Slot {
            // Only the first slot (the password's) is damaged: the others still open the store.
            assert_eq!((by_passphrase, by_recovery), (200, 200), "{region:?}: the damage is local to one slot");
        } else {
            assert_eq!((by_passphrase, by_recovery), (401, 401), "{region:?}");
            assert_eq!(env.store_sha256(&alice), damaged, "{region:?}: refused attempts write nothing");
        }
        let _ = (assistant, integration);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_closed_the_damage_is_undone_by_the_same_change_and_nothing_was_lost() {
    let env = UnlockEnv::start().await;
    let (alice, assistant, integration, _profile) = person_with_secrets(&env).await;
    let original = env.store_sha256(&alice);
    env.flip_byte(&alice, Region::Body);
    let session = env.sign_in(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Locked);
    env.sign_out(&session).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    env.restart_worker(&alice).await;
    env.flip_byte(&alice, Region::Body);
    assert_eq!(env.store_sha256(&alice), original, "the same change twice is no change");
    let _again = env.sign_in(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Open, "the intact file opens at the next sign-in");
    env.restart_worker(&alice).await;
    let store = fresh_open(&env, &alice);
    assert_eq!(store.get("int:prod:TOKEN").expect("read").as_deref(), Some(integration.as_str()));
    assert!(assistant.starts_with("cn-"));
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_closed_a_store_made_with_another_key_is_never_opened_replaced_or_extended() {
    let env = UnlockEnv::start().await;
    let (_alice, admin, bob, first) = env.people().await;
    env.sign_out(&first).await;
    env.wait_state(&bob, StoreState::Locked, "the sign-out").await;
    assert_eq!(env.reset_secrets(&admin, &bob).await.status, 204);
    env.restart_worker(&bob).await;
    // The store of somebody else's key sits where the store of bob belongs.
    let other = Kek::from_bytes(&[0x5a; 32]);
    EncryptedFileStore::create(&env.store_file(&bob), SlotKind::Password, [0u8; 16], &other).expect("a foreign store");
    let foreign = env.store_sha256(&bob);
    let session = env.sign_in(&bob).await;
    assert_eq!(env.state(&bob), StoreState::Locked);
    assert_commands_fail_closed(&env, &session, &bob, "a store of another key").await;
    // Setting a passphrase needs the key of the store, which the person does not have: nothing is added.
    let reply = env.api(&session, "PUT", "/api/me/passphrase", Some(&json!({"passphrase": unlock_env_passphrase()}))).await;
    assert!(reply.status == 204 || reply.status >= 400, "status {}", reply.status);
    assert_eq!(env.store_sha256(&bob), foreign, "no slot was added to a store that is not his");
    assert_eq!(env.unlock_recovery(&session, &bob.recovery).await.status, 401);
    assert_eq!(env.unlock_passphrase(&session, unlock_env_passphrase()).await.status, 401);
    assert_eq!(env.store_sha256(&bob), foreign);
    assert_eq!(env.state(&bob), StoreState::Locked);
}

#[tokio::test(flavor = "multi_thread")]
async fn fail_closed_a_changed_second_slot_costs_only_the_method_it_belongs_to() {
    let env = UnlockEnv::start().await;
    let (alice, _assistant, integration, saved_profile) = person_with_secrets(&env).await;
    env.flip_byte(&alice, Region::SecondSlot);
    let session = env.sign_in(&alice).await;
    // The password's slot is intact: the store opens at the sign-in, and its content is whole.
    assert_eq!(env.state(&alice), StoreState::Open);
    env.lock_at_the_worker(&alice).await;
    let kinds: Vec<SlotKind> = env.slots_of(&alice).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds.len(), 3, "the slots are still three");
    // The method of the second slot no longer opens; the other one still does.
    let (broken, working) = if kinds[1] == SlotKind::Recovery { ("recovery code", "passphrase") } else { ("passphrase", "recovery code") };
    let attempt = |which: &str| {
        let (env, session, alice) = (&env, &session, &alice);
        let which = which.to_owned();
        async move {
            if which == "passphrase" {
                env.unlock_passphrase(session, unlock_env_passphrase()).await.status
            } else {
                env.unlock_recovery(session, &alice.recovery).await.status
            }
        }
    };
    assert_eq!(attempt(broken).await, 401, "the {broken} belongs to the changed slot");
    assert_eq!(env.state(&alice), StoreState::Locked);
    assert_eq!(attempt(working).await, 200, "the {working} is untouched");
    assert_eq!(env.state(&alice), StoreState::Open);
    env.sign_out(&session).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-out").await;
    env.restart_worker(&alice).await;
    let again = env.sign_in(&alice).await;
    assert_eq!(env.state(&alice), StoreState::Open, "the password opens the store as before");
    let mut socket = env.socket(&again).await;
    assert_eq!(ai_has_key(&mut socket, &saved_profile).await, Ok(true), "the content is whole");
    assert!(integration.starts_with("cn-"));
}

// ---- two writers --------------------------------------------------------------------------------------------------------

/// Calls an operation that answers `Result<_, String>` until the store is not locked, counting the refusals.
macro_rules! until_saved {
    ($call:expr) => {{
        let mut tries = 0u32;
        loop {
            match $call.await {
                Ok(_) => break tries,
                Err(text) if text == LOCKED && tries < 600 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(15)).await;
                }
                Err(text) => panic!("the save failed: {text}"),
            }
        }
    }};
}

/// Two sessions of one person write twenty secrets each at the same time: one an integration secret each time, the other the
/// key of the assistant each time. With `racing`, the store is locked and unlocked again while they write.
async fn two_writers(racing: bool) {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    set_passphrase(&env, &first, unlock_env_passphrase()).await;
    let second = env.sign_in(&alice).await;
    let names: Vec<String> = (0..8).map(|i| format!("TOKEN_{i}")).collect();
    let mut one = env.socket(&first).await;
    let mut two = env.socket(&second).await;
    let profile = ai_profile(&mut one).await;
    integration_profile(&mut one, "prod", &names).await;
    let value = |i: usize| canary("writers", &format!("value-{i}"));
    let (retries_a, retries_b, locks) = {
        let writer_a = async {
            let mut retries = 0;
            // Twenty saves over eight names: the later ones replace the earlier ones of the same name.
            for i in 0..20 {
                retries += until_saved!(integration_secret_set(&mut one, "prod", &names[i % 8], &value(i)));
            }
            retries
        };
        let writer_b = async {
            let mut retries = 0;
            for i in 0..20 {
                retries += until_saved!(ai_key_set(&mut two, &profile, &format!("sk-writer-b-{i}")));
            }
            retries
        };
        let locker = async {
            let mut locks = 0;
            if racing {
                for _ in 0..8 {
                    env.lock_at_the_worker(&alice).await;
                    locks += 1;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    assert_eq!(env.unlock_passphrase(&first, unlock_env_passphrase()).await.status, 200);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
            }
            locks
        };
        tokio::join!(writer_a, writer_b, locker)
    };
    drop((one, two));
    env.sign_out(&first).await;
    env.sign_out(&second).await;
    env.wait_state(&alice, StoreState::Locked, "the sign-outs").await;
    env.restart_worker(&alice).await;
    // The file as the next process finds it: every value of both writers, the last one of the assistant key, three slots.
    let store = fresh_open(&env, &alice);
    for (k, name) in names.iter().enumerate() {
        let last = (0..20).filter(|i| i % 8 == k).max().expect("every name is written");
        assert_eq!(store.get(&format!("int:prod:{name}")).expect("read").as_deref(), Some(value(last).as_str()), "{name}");
    }
    let envelope: Value = serde_json::from_str(&store.get(&format!("ai:{profile}")).expect("read").expect("a value")).expect("JSON");
    assert_eq!(envelope["key"], "sk-writer-b-19", "the last write of the second writer");
    assert_eq!(env.slots_of(&alice).len(), 3);
    let (hits, _) = unlock_env::scan_volume_for(&env.volume(), &[("a value", &value(19)), ("assistant key", "sk-writer-b-19")]);
    assert_no_hits("the volume", &hits);
    println!("two writers (racing: {racing}): 40 saves, {locks} locks, refused and repeated {} times, every last value present", retries_a + retries_b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_writers_two_sessions_with_twenty_saves_each_lose_nothing() {
    two_writers(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_writers_racing_a_lock_and_an_unlock_lose_nothing_that_was_acknowledged() {
    two_writers(true).await;
}

// ---- background work ----------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn background_work_on_a_locked_store_ends_once_with_a_fixed_sentence_and_resumes_after_the_unlock() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    set_passphrase(&env, &first, unlock_env_passphrase()).await;
    let key = canary("background", "assistant");
    let mut socket = env.socket(&first).await;
    let profile = ai_profile(&mut socket).await;
    ai_key_set(&mut socket, &profile, &key).await.expect("the key is saved");
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    let conversation = socket.call("ai_conversation_new", "{}").await["ok"].as_str().expect("a conversation").to_owned();
    env.lock_at_the_worker(&alice).await;
    let before = env.store_sha256(&alice);
    let args = json!({"conversation": conversation, "text": "hello", "pane": pane, "profile": profile}).to_string();
    let started = socket.call("ai_send", &args).await;
    assert!(started.get("ok").is_some(), "{started}");
    // The turn ends once, as failed, with the class and the fixed sentence of an unavailable key store.
    let ended = socket
        .event_where("ai", Duration::from_secs(10), |e| e["kind"] == "turn_ended")
        .await
        .expect("the turn ends");
    assert_eq!(ended["payload"]["end"], "failed");
    assert_eq!(ended["payload"]["error"]["class"], "unavailable", "{ended}");
    let text = ended["payload"]["error"]["text"].as_str().expect("a sentence").to_owned();
    assert!(!text.contains(&key));
    // No retry loop: nothing else about the turn comes, and nothing is written.
    let mut more = 0;
    while let Some(extra) = socket.event_where("ai", Duration::from_secs(3), |e| e["kind"] == "turn_ended" || e["kind"] == "turn_started").await {
        more += 1;
        assert!(more < 2, "a retry: {extra}");
    }
    assert_eq!(more, 0, "no second turn and no second ending");
    assert_eq!(env.store_sha256(&alice), before);
    // After the unlock the key is there again for the background work.
    assert_eq!(env.unlock_passphrase(&first, unlock_env_passphrase()).await.status, 200);
    assert_eq!(ai_has_key(&mut socket, &profile).await, Ok(true));
    let (hits, _) = unlock_env::scan_volume_for(&env.volume(), &[("assistant key", &key)]);
    assert_no_hits("the volume", &hits);
    println!("background: the turn ended once as failed ({text:?}), no retry, the key is back after the unlock");
}

#[tokio::test(flavor = "multi_thread")]
async fn background_work_the_sync_setup_on_a_locked_store_fails_closed_and_never_dials() {
    let env = UnlockEnv::start().await;
    let (alice, first, _) = env.bootstrap("alice", PASSWORD).await;
    let mut socket = env.socket(&first).await;
    env.lock_at_the_worker(&alice).await;
    let before = env.store_sha256(&alice);
    let password = canary("background", "sync-password");
    let args = json!({"endpoint": "https://sync.invalid/dav", "user": "me", "password": password, "passphrase": canary("background", "sync-passphrase")});
    let answer = socket.call("sync_save", &args.to_string()).await;
    let fault = UnlockEnv::fault_text(&answer).expect("a refusal");
    println!("sync_save on a locked store answers: {fault}");
    assert!(!fault.contains(&password));
    assert_eq!(env.store_sha256(&alice), before);
    let (hits, _) = unlock_env::scan_volume_for(&env.volume(), &[("sync password", &password), ("sync passphrase", &canary("background", "sync-passphrase"))]);
    assert_no_hits("the volume", &hits);
    let status = socket.call("sync_status", "{}").await;
    assert_eq!(status["ok"]["configured"], false, "nothing was configured: {status}");
}

// ---- the shell of a pane ------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn shell_environment_and_arguments_of_a_pane_and_of_every_process_hold_no_secret() {
    let seed = random_hex(8);
    let run = canary_run(&seed).await;
    // The store of alice is open (she signed in last); a shell of hers runs while the keys are in the worker.
    let who = run.env.signed_from(&run.cookies[4]);
    assert_eq!(run.env.state(&run.alice), StoreState::Open);
    let mut socket = run.env.socket(&who).await;
    let view = socket.ok("get_layout", "{}").await;
    let pane = focus(&view);
    socket.ok("start_pane", &json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16}).to_string()).await;
    let dir = Scratch::new("shell");
    let d = dir.path().display();
    // The environment of the shell, the arguments of every process on the machine and of this very shell, written to files.
    let line = format!("env > {d}/env.tmp; ps -ww -ax -o args= > {d}/ps.tmp; mv {d}/env.tmp {d}/env.txt; mv {d}/ps.tmp {d}/ps.txt\n");
    let mut attempts = 0;
    while !dir.path().join("ps.txt").exists() {
        attempts += 1;
        assert!(attempts < 60, "the shell did not run the command");
        // The shell may still be starting: the line is typed again until it ran.
        if attempts % 10 == 1 {
            socket.call("paste", &json!({"pane": pane, "text": line, "allow_unsafe": true}).to_string()).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let environment = std::fs::read_to_string(dir.path().join("env.txt")).expect("the environment");
    let processes = std::fs::read_to_string(dir.path().join("ps.txt")).expect("the processes");
    assert!(environment.contains("PATH="), "the environment was captured");
    assert!(processes.contains("canary"), "the process list holds at least this test");
    let refs = run.needle_refs();
    let in_environment = unlock_env::scan_text_for(&environment, &refs);
    let in_processes = unlock_env::scan_text_for(&processes, &refs);
    assert!(in_environment.is_empty(), "the environment of the shell holds: {in_environment:?}");
    assert!(in_processes.is_empty(), "the arguments of a process hold: {in_processes:?}");
    // The scan is not blind.
    let planted = format!("{environment}\nLEAK={}", run.needles[0].1);
    assert!(!unlock_env::scan_text_for(&planted, &refs).is_empty());
    println!("shell: {} environment lines and {} process lines, {} secrets, 0 hits", environment.lines().count(), processes.lines().count(), refs.len());
}

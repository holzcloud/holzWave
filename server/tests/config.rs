//! The server key file and the configuration: creation, the strict reader, the purpose keys and, below them, the
//! refusals that stop the server before any socket is opened.

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

use holzwave_server::keyfile::{self, Purpose, ServerKey};
use scratch::Scratch;

/// A marker that is no key. Files that hold it are refused; it must never reach a sentence or an output.
const CANARY: &str = "CANARY-do-not-print-0123456789";

/// A server key line for the bytes, as `keygen` writes it.
fn key_line(bytes: [u8; 32]) -> String {
    use base64::Engine;
    format!("hws1:{}\n", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Runs the server binary with exactly the given environment.
fn server(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("run the server binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
}

// ---------------------------------------------------------------------------------------------------------------------
// The key file: creation
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn key_keygen_writes_a_private_file_and_prints_only_the_path() {
    let dir = Scratch::new("keygen");
    let path = dir.join("server.key");
    let out = server(&["keygen", "--out", path.to_str().expect("utf-8 path")], &[]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("wrote {}\n", path.display()));
    assert!(out.stderr.is_empty(), "stderr: {}", text(&out.stderr));

    let content = std::fs::read_to_string(&path).expect("read the key file");
    assert!(content.starts_with("hws1:") && content.ends_with('\n'), "shape");
    assert_eq!(content.len(), 5 + 43 + 1);
    assert!(content[5..48].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "base64url characters");
    assert_eq!(mode_of(&path), 0o600);
    assert!(std::fs::metadata(&path).expect("metadata").is_file());
    println!("Key file: mode {:04o}, {} bytes ({} characters after the prefix)", mode_of(&path), content.len(), 43);

    // Nothing the command printed holds the key.
    let key_part = &content[5..48];
    assert!(!text(&out.stdout).contains(key_part) && !text(&out.stderr).contains(key_part));
    // The file it made loads.
    ServerKey::load(&path).expect("a generated file loads");
}

#[test]
fn key_keygen_takes_its_target_from_the_environment_and_from_an_equals_form() {
    let dir = Scratch::new("keygen-env");
    let by_env = dir.join("by-env.key");
    let out = server(&["keygen"], &[("HOLZWAVE_SERVER_KEY_FILE", by_env.to_str().expect("utf-8 path"))]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(mode_of(&by_env), 0o600);

    let by_flag = dir.join("by-flag.key");
    let arg = format!("--out={}", by_flag.display());
    // The flag wins over the variable.
    let out = server(&["keygen", &arg], &[("HOLZWAVE_SERVER_KEY_FILE", by_env.to_str().expect("utf-8 path"))]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert!(by_flag.exists());

    let out = server(&["keygen"], &[]);
    assert_eq!(out.status.code(), Some(2), "no target is a usage error");
    assert!(text(&out.stderr).contains("HOLZWAVE_SERVER_KEY_FILE"));
}

#[test]
fn key_keygen_gives_a_different_key_every_time() {
    let dir = Scratch::new("keygen-twice");
    let (a, b) = (dir.join("a.key"), dir.join("b.key"));
    keyfile::generate(&a).expect("first");
    keyfile::generate(&b).expect("second");
    assert_ne!(std::fs::read(&a).expect("read a"), std::fs::read(&b).expect("read b"));
}

#[test]
fn key_keygen_never_overwrites_and_never_follows_a_link() {
    let dir = Scratch::new("keygen-refuse");
    let path = dir.join("server.key");
    let p = path.to_str().expect("utf-8 path");
    assert_eq!(server(&["keygen", "--out", p], &[]).status.code(), Some(0));
    let before = std::fs::read(&path).expect("read the key file");

    let out = server(&["keygen", "--out", p], &[]);
    assert_eq!(out.status.code(), Some(1), "an existing file is refused");
    assert!(out.stdout.is_empty(), "nothing is reported as written");
    let err = text(&out.stderr);
    assert!(err.contains(p) && err.contains("already exists"), "{err}");
    assert_eq!(std::fs::read(&path).expect("read again"), before, "the file is untouched");

    // A link to an existing file: refused, and the target is untouched.
    let link = dir.join("link.key");
    symlink(&path, &link).expect("make a link");
    let out = server(&["keygen", "--out", link.to_str().expect("utf-8 path")], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read(&path).expect("read the target"), before);

    // A link to nothing: refused, and nothing appears behind it.
    let target = dir.join("not-there.key");
    let dangling = dir.join("dangling.key");
    symlink(&target, &dangling).expect("make a dangling link");
    let out = server(&["keygen", "--out", dangling.to_str().expect("utf-8 path")], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!target.exists(), "the link was not followed to create its target");

    // A directory that is missing: a sentence, no panic.
    let out = server(&["keygen", "--out", dir.join("no-dir/x.key").to_str().expect("utf-8 path")], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("cannot be created"));
}

#[test]
fn key_keygen_sets_mode_0600_whatever_the_umask_allows() {
    // The mode is given at creation and set again on the descriptor, so a file made here is 0600 on any umask the
    // test runner has; the check is on the result.
    let dir = Scratch::new("keygen-mode");
    let path = dir.join("m.key");
    keyfile::generate(&path).expect("generate");
    let meta = std::fs::metadata(&path).expect("metadata");
    assert_eq!(meta.mode() & 0o7777, 0o600);
    assert_eq!(meta.nlink(), 1);
}

// ---------------------------------------------------------------------------------------------------------------------
// The key file: the strict reader
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn key_load_accepts_the_modes_a_private_file_may_have() {
    let dir = Scratch::new("load-modes");
    let line = key_line([9; 32]);
    // 0440 is how a mounted secret appears with a file group; the group may read, nobody may write.
    for (i, mode) in [0o600, 0o400, 0o440, 0o640].into_iter().enumerate() {
        let path = dir.write_file(&format!("k{i}"), line.as_bytes(), mode);
        assert!(ServerKey::load(&path).is_ok(), "mode {mode:04o} is accepted");
    }
}

#[test]
fn key_load_refuses_a_loose_mode_with_a_sentence_that_names_the_file_and_the_fix() {
    let dir = Scratch::new("load-loose");
    let line = format!("{CANARY}\n");
    let mut sentences = Vec::new();
    for mode in [0o620, 0o610, 0o604, 0o602, 0o601, 0o666, 0o777, 0o660, 0o644] {
        let path = dir.write_file(&format!("m{mode:o}"), line.as_bytes(), mode);
        let error = ServerKey::load(&path).expect_err("a loose mode is refused");
        let sentence = error.to_string();
        assert!(sentence.contains(path.to_str().expect("utf-8 path")), "{sentence}");
        assert!(sentence.contains("chmod 0600") && sentence.contains(&format!("{mode:04o}")), "{sentence}");
        assert!(!sentence.contains(CANARY), "{sentence}");
        assert!(!sentence.contains('\n'));
        sentences.push(sentence);
    }
    println!("Refusals: loose mode: {}", sentences[0].replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));
}

#[test]
fn key_load_refuses_a_missing_file_a_link_and_a_file_that_is_not_regular() {
    let dir = Scratch::new("load-kind");
    let missing = dir.join("missing.key");
    let sentence = ServerKey::load(&missing).expect_err("missing").to_string();
    assert!(sentence.contains(missing.to_str().expect("utf-8 path")) && sentence.contains("does not exist"), "{sentence}");
    println!("Refusals: missing: {}", sentence.replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));

    let real = dir.write_file("real.key", key_line([5; 32]).as_bytes(), 0o600);
    let link = dir.join("link.key");
    symlink(&real, &link).expect("make a link");
    let sentence = ServerKey::load(&link).expect_err("a link").to_string();
    assert!(sentence.contains(link.to_str().expect("utf-8 path")) && sentence.contains("link"), "{sentence}");
    println!("Refusals: link: {}", sentence.replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));

    let sentence = ServerKey::load(dir.path()).expect_err("a directory").to_string();
    assert!(sentence.contains("not a regular file"), "{sentence}");
    println!("Refusals: directory: {}", sentence.replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));
    let sentence = ServerKey::load(Path::new("/dev/null")).expect_err("a device").to_string();
    assert!(sentence.contains("not a regular file") || sentence.contains("too open"), "{sentence}");
}

#[test]
fn key_load_refuses_every_wrong_shape_without_printing_the_content() {
    let dir = Scratch::new("load-shape");
    let good = key_line([3; 32]);
    let digits43 = &good[5..48];
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("only a newline", b"\n".to_vec()),
        ("canary text", format!("{CANARY}\n").into_bytes()),
        ("no prefix", format!("{digits43}\n").into_bytes()),
        ("other prefix", format!("hws2:{digits43}\n").into_bytes()),
        ("upper prefix", format!("HWS1:{digits43}\n").into_bytes()),
        ("one character short", format!("hws1:{}\n", &digits43[1..]).into_bytes()),
        ("one character long", format!("hws1:{digits43}A\n").into_bytes()),
        ("standard alphabet", format!("hws1:{}+/\n", &digits43[2..]).into_bytes()),
        ("padding", format!("hws1:{}=\n", &digits43[1..]).into_bytes()),
        ("space inside", format!("hws1:{} {}\n", &digits43[..20], &digits43[21..]).into_bytes()),
        ("leading space", format!(" hws1:{digits43}\n").into_bytes()),
        ("carriage return", format!("hws1:{digits43}\r\n").into_bytes()),
        ("two lines", format!("hws1:{digits43}\nhws1:{digits43}\n").into_bytes()),
        ("two newlines", format!("hws1:{digits43}\n\n").into_bytes()),
        ("trailing text", format!("hws1:{digits43}\n{CANARY}\n").into_bytes()),
        ("not utf-8", vec![0xff; 48]),
        ("a unit bit set past the key", format!("hws1:{}B\n", "A".repeat(42)).into_bytes()),
        ("too big", vec![b'A'; 4096]),
        ("just over the cap", vec![b'A'; 257]),
    ];
    let mut shown_once = false;
    for (name, content) in cases {
        let path = dir.write_file(&name.replace(' ', "-"), &content, 0o600);
        let error = ServerKey::load(&path).expect_err(name);
        let sentence = error.to_string();
        assert!(sentence.contains(path.to_str().expect("utf-8 path")), "{name}: {sentence}");
        assert!(sentence.contains("holzwave-server keygen"), "{name}: names the fix: {sentence}");
        assert!(!sentence.contains(CANARY) && !sentence.contains(digits43), "{name}: {sentence}");
        assert!(!sentence.contains('\n'), "{name}");
        if !shown_once {
            shown_once = true;
            println!("Refusals: wrong shape: {}", sentence.replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));
        }
    }
}

#[test]
fn key_load_refuses_the_all_zero_key() {
    let dir = Scratch::new("load-zero");
    let path = dir.write_file("zero.key", key_line([0; 32]).as_bytes(), 0o600);
    let sentence = ServerKey::load(&path).expect_err("all zero").to_string();
    assert!(sentence.contains(path.to_str().expect("utf-8 path")) && sentence.contains("all-zero"), "{sentence}");
    println!("Refusals: all zero: {}", sentence.replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));
    assert!(ServerKey::from_bytes([0; 32]).is_none());
}

#[test]
fn key_open_private_names_the_kind_of_file_it_was_asked_for() {
    let dir = Scratch::new("open-private");
    let path = dir.write_file("tls.key", b"x", 0o666);
    let error = keyfile::open_private(&path, "TLS key file").expect_err("too open");
    assert!(error.sentence().starts_with("the TLS key file "), "{error}");
    let ok = dir.write_file("fine.key", b"x", 0o600);
    keyfile::open_private(&ok, "TLS key file").expect("a private file opens");
}

// ---------------------------------------------------------------------------------------------------------------------
// The key: no print, no copy, one key per purpose
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn key_debug_text_holds_no_key_byte() {
    // A pattern whose bytes would show up as "171", "ab" or "AB" in any rendering of the array.
    let key = ServerKey::from_bytes([0xAB; 32]).expect("a key");
    let plain = format!("{key:?}");
    let pretty = format!("{key:#?}");
    assert_eq!(plain, "ServerKey(..)");
    assert_eq!(pretty, "ServerKey(..)");
    for needle in ["171", "ab", "AB", "0xab", "[", "0x"] {
        assert!(!plain.contains(needle), "{needle}");
    }
    // The derived keys are not printed either: `Zeroizing` prints its content, which is why callers hold them and never log them.
    let boxed = Some(key);
    assert_eq!(format!("{boxed:?}"), "Some(ServerKey(..))");
}

#[test]
fn key_derive_gives_one_key_per_purpose_and_the_same_key_every_time() {
    let key = ServerKey::from_bytes([1; 32]).expect("a key");
    let again = ServerKey::from_bytes([1; 32]).expect("a key");
    let other = ServerKey::from_bytes([2; 32]).expect("a key");
    let all = [Purpose::Sealed, Purpose::Csrf, Purpose::Acme];
    for purpose in all {
        assert_eq!(*key.derive(purpose), *key.derive(purpose), "{purpose:?} is stable");
        assert_eq!(*key.derive(purpose), *again.derive(purpose), "{purpose:?} depends on the key only");
        assert_ne!(*key.derive(purpose), *other.derive(purpose), "{purpose:?} depends on the key");
        assert_ne!(*key.derive(purpose), [1; 32], "{purpose:?} is not the key itself");
    }
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(*key.derive(*a), *key.derive(*b), "{a:?} and {b:?} differ");
        }
    }
}

#[test]
fn key_derive_follows_hkdf_sha256_with_the_documented_info_string() {
    // Computed with HMAC-SHA-256 outside this code: PRK = HMAC(zero salt, key), OKM = HMAC(PRK, info || 0x01).
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    let key = ServerKey::from_bytes([1; 32]).expect("a key");
    assert_eq!(hex(&*key.derive(Purpose::Sealed)), "11f6e01f5e54c27cf4a42f0d6b05aed6d8c541d31b4d81598eddf6f5748f6859");
    assert_eq!(hex(&*key.derive(Purpose::Csrf)), "f806dc89562887466df8685983e7829d934a43ff5fabcb2219e3f8d75b8e9d40");
    assert_eq!(hex(&*key.derive(Purpose::Acme)), "81ebdd8a6386ee8e31557ad9a1f9affbac81e76bf2b4221988bcbbb3ce96877c");
}

#[test]
fn key_derive_works_on_a_key_read_from_a_file() {
    let dir = Scratch::new("derive-file");
    let path = dir.write_file("k", key_line([1; 32]).as_bytes(), 0o600);
    let loaded = ServerKey::load(&path).expect("load");
    let direct = ServerKey::from_bytes([1; 32]).expect("key");
    assert_eq!(*loaded.derive(Purpose::Sealed), *direct.derive(Purpose::Sealed));
}

// ---------------------------------------------------------------------------------------------------------------------
// The configuration: the typed file, the defaults and the strictness
// ---------------------------------------------------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use holzwave_server::config::{
    self, Compression, Env, Probe, Refusal, RealProbe, TlsSetup, prepare,
};
use holzwave_server::limits;
use serde_json::{Value, json};

/// The path the fake probe serves the configuration text under.
const CONFIG_PATH: &str = "/etc/test/server.json";

/// A probe that serves one configuration text, a list of readable files and a fixed user id. The key is read from the real
/// file system, because the strict reader is what the key tests above already cover.
struct FakeProbe {
    config: Option<String>,
    readable: Vec<PathBuf>,
    uid: u32,
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
        RealProbe.load_key(path)
    }
}

/// A scratch directory with a good key file, and the environment that points at it.
struct Setup {
    _dir: Scratch,
    key_path: PathBuf,
    env: Env,
}

fn setup() -> Setup {
    let dir = Scratch::new("config");
    let key_path = dir.write_file("server.key", key_line([4; 32]).as_bytes(), 0o600);
    let mut env = Env::new();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), key_path.to_str().expect("utf-8 path").to_owned());
    env.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), CONFIG_PATH.to_owned());
    Setup { _dir: dir, key_path, env }
}

fn probe_for(json: &Value) -> FakeProbe {
    FakeProbe {
        config: Some(json.to_string()),
        readable: ["/certs/cert.pem", "/certs/key.pem", "/certs/ca.pem", "/secrets/oidc"].map(PathBuf::from).to_vec(),
        uid: 10_001,
    }
}

/// A valid file for the TLS mode `files`: the smallest that is accepted.
fn base() -> Value {
    json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    })
}

fn prepare_json(setup: &Setup, json: &Value) -> Result<config::Prepared, Refusal> {
    prepare(&setup.env, &probe_for(json))
}

/// Sets the value at a dotted path, making the objects on the way.
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

#[test]
fn config_a_valid_file_for_each_tls_mode_is_prepared() {
    let s = setup();

    let files = prepare_json(&s, &base()).expect("files mode");
    assert_eq!(files.origin.as_str(), "https://hw.example.test");
    assert!(matches!(files.tls, TlsSetup::Files { .. }));
    assert_eq!(files.config_path, PathBuf::from(CONFIG_PATH));
    assert!(files.auth.passkey && !files.auth.password_totp && files.auth.oidc.is_empty());

    let mut acme = base();
    set(&mut acme, "tls", json!({"mode": "acme", "contact": "mailto:admin@example.test",
        "domains": ["hw.example.test", "www.example.test"], "ca_file": "/certs/ca.pem",
        "directory_url": "https://ca.example.test/directory"}));
    let acme = prepare_json(&s, &acme).expect("acme mode");
    match acme.tls {
        TlsSetup::Acme { directory_url, contact, domains, ca_file } => {
            assert_eq!(directory_url, "https://ca.example.test/directory");
            assert_eq!(contact, "mailto:admin@example.test");
            assert_eq!(domains, ["hw.example.test", "www.example.test"]);
            assert_eq!(ca_file, Some(PathBuf::from("/certs/ca.pem")));
        }
        other => panic!("{other:?}"),
    }
    let mut acme_default = base();
    set(&mut acme_default, "tls", json!({"mode": "acme", "contact": "admin@example.test", "domains": ["hw.example.test"]}));
    match prepare_json(&s, &acme_default).expect("acme mode with defaults").tls {
        TlsSetup::Acme { directory_url, .. } => assert_eq!(directory_url, config::DEFAULT_ACME_DIRECTORY),
        other => panic!("{other:?}"),
    }

    let mut proxy = base();
    set(&mut proxy, "tls", json!({"mode": "proxy", "trusted_proxies": ["10.0.0.0/8", "fd00::1"]}));
    set(&mut proxy, "listen", json!("127.0.0.1:8080"));
    let proxy = prepare_json(&s, &proxy).expect("proxy mode");
    assert_eq!(proxy.listen, "127.0.0.1:8080".parse().expect("address"));
    match proxy.tls {
        TlsSetup::Proxy { trusted_proxies } => assert_eq!(trusted_proxies.len(), 2),
        other => panic!("{other:?}"),
    }
}

#[test]
fn config_the_defaults_are_those_of_the_contract_and_sit_inside_their_bounds() {
    let s = setup();
    let p = prepare_json(&s, &base()).expect("minimal file");
    assert_eq!(p.listen, "0.0.0.0:8443".parse().expect("address"));
    assert_eq!((p.data_dir.as_path(), p.runtime_dir.as_path()), (Path::new("/data"), Path::new("/run/holzwave")));
    assert_eq!(p.sessions.idle, Duration::from_secs(30 * 60));
    assert_eq!(p.sessions.absolute, Duration::from_secs(24 * 3600));
    assert_eq!(p.sessions.step_up, Duration::from_secs(5 * 60));
    assert_eq!((p.workers.uid_min, p.workers.uid_max), (20_000, 29_999));
    assert_eq!(p.workers.idle_timeout, Duration::from_secs(24 * 3600));
    assert_eq!((p.workers.max_panes, p.workers.max_scrollback_lines), (64, 10_000));
    assert_eq!(p.upload.max_bytes, 256 * 1024 * 1024);
    assert_eq!(p.upload.dir, PathBuf::from("uploads"));
    assert_eq!(p.transport.compression, Compression::Off);
    assert_eq!(p.limits, limits::Limits::DEFAULT);
    assert!(!p.auth.password_totp && p.auth.oidc.is_empty(), "no method turns itself on");

    println!("Defaults:");
    println!("  listen = {}, data_dir = {}, runtime_dir = {}", p.listen, p.data_dir.display(), p.runtime_dir.display());
    println!("  workers.uid_min..uid_max = {}..{}, helper_path = {}", p.workers.uid_min, p.workers.uid_max, p.workers.helper_path.display());
    println!("  workers.worker_path = {}, upload.dir = {}, transport.compression = off", p.workers.worker_path.display(), p.upload.dir.display());
    println!("  {:<40} {:>10} {:>10} {:>10}", "key", "default", "min", "max");
    for row in limits::all_rows() {
        println!("  {:<40} {:>10} {:>10} {:>10}", row.key, row.default, row.min, row.max);
    }
}

#[test]
fn config_every_key_of_the_file_is_accepted() {
    let s = setup();
    let mut full = json!({
        "public_url": "https://hw.example.test:8443/",
        "listen": "[::]:9443",
        "data_dir": "/srv/data",
        "runtime_dir": "/run/hw",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {
            "passkey": true,
            "password_totp": true,
            "oidc": [{
                "id": "corp", "name": "Corp", "issuer": "https://login.example.test", "client_id": "abc",
                "client_secret_file": "/secrets/oidc", "allowed_domains": ["example.test"],
                "allowed_groups": ["staff"], "groups_claim": "roles"
            }]
        },
        "sessions": {"idle": "20m", "absolute": "12h", "step_up": "2m"},
        "limits": {
            "handshake_timeout": "5s", "header_timeout": "5s", "max_connections": 500, "max_connections_per_address": 30,
            "api_body_bytes": 32768, "ws_message_bytes": 4194304, "ws_command_bytes": 32768, "ws_small_command_bytes": 2048,
            "ws_queue_bytes": 2097152, "sockets_per_person": 4, "commands_per_second": 200, "command_burst": 400,
            "ping_interval": "20s", "session_recheck": "15s", "argon2_concurrency": 1,
            "password_attempts_per_account": 4, "password_attempts_per_address": 10, "attempt_window": "10m",
            "lock_duration": "20m", "pending_signin": "3m"
        },
        "workers": {
            "uid_min": 30000, "uid_max": 30999, "idle_timeout": "12h", "max_panes": 32, "max_scrollback_lines": 5000,
            "nproc": 512, "nofile": 2048, "helper_path": "/opt/hw/helper", "worker_path": "/opt/hw/server"
        },
        "upload": {"dir": "incoming/files", "max_bytes": 1048576},
        "transport": {"compression": "deflate"}
    });
    let p = prepare_json(&s, &full).expect("a file with every key");
    assert_eq!(p.origin.as_str(), "https://hw.example.test:8443");
    assert_eq!(p.listen, "[::]:9443".parse().expect("address"));
    assert_eq!(p.sessions.idle, Duration::from_secs(1200));
    assert_eq!(p.limits.max_connections, 500);
    assert_eq!(p.limits.ping_interval, Duration::from_secs(20));
    assert_eq!(p.workers.uid_min, 30_000);
    assert_eq!(p.upload.dir, PathBuf::from("incoming/files"));
    assert_eq!(p.transport.compression, Compression::Deflate);
    assert_eq!(p.auth.oidc[0].groups_claim.as_deref(), Some("roles"));

    // A variable replaces data_dir.
    let mut env = s.env.clone();
    env.insert("HOLZWAVE_DATA_DIR".to_owned(), "/mnt/hw".to_owned());
    set(&mut full, "data_dir", json!("/srv/data"));
    let p = prepare(&env, &probe_for(&full)).expect("with the data directory variable");
    assert_eq!(p.data_dir, PathBuf::from("/mnt/hw"));
}

#[test]
fn config_an_unknown_key_is_refused_at_every_level() {
    let s = setup();
    let mut full = base();
    set(&mut full, "auth.oidc", json!([{
        "id": "corp", "name": "Corp", "issuer": "https://login.example.test", "client_id": "abc",
        "client_secret_file": "/secrets/oidc"
    }]));
    for level in ["", "tls", "auth", "auth.oidc.0", "sessions", "limits", "workers", "upload", "transport"] {
        let mut edited = full.clone();
        if level.is_empty() {
            edited["zzz"] = json!(1);
        } else if let Some(index) = level.strip_suffix(".0") {
            let mut at = &mut edited;
            for part in index.split('.') {
                at = &mut at[part];
            }
            at[0]["zzz"] = json!(1);
        } else {
            set(&mut edited, &format!("{level}.zzz"), json!(1));
        }
        let error = prepare_json(&s, &edited).expect_err(level);
        assert!(error.sentence().contains("unknown field `zzz`"), "{level:?}: {error}");
        assert!(error.sentence().contains("is not valid"), "{level:?}: {error}");
    }
    // The base without edits is fine.
    prepare_json(&s, &full).expect("the unedited file");
}

#[cfg(not(feature = "in-process-workers"))]
#[test]
fn config_the_in_process_workers_key_does_not_exist_in_a_release_build() {
    let s = setup();
    let mut edited = base();
    set(&mut edited, "workers.mode", json!("in_process"));
    let error = prepare_json(&s, &edited).expect_err("no such key");
    assert!(error.sentence().contains("unknown field `mode`"), "{error}");
}

#[cfg(feature = "in-process-workers")]
#[test]
fn config_the_in_process_workers_key_exists_with_the_feature() {
    let s = setup();
    let mut edited = base();
    set(&mut edited, "workers.mode", json!("in_process"));
    let p = prepare_json(&s, &edited).expect("the feature adds the key");
    assert_eq!(p.workers.mode, config::WorkersMode::InProcess);
    set(&mut edited, "workers.mode", json!("elsewhere"));
    assert!(prepare_json(&s, &edited).is_err());
}

#[test]
fn config_a_wrong_type_or_a_bad_duration_does_not_parse() {
    let s = setup();
    for (path, value) in [
        ("sessions.idle", json!(1800)),
        ("sessions.idle", json!("30")),
        ("sessions.idle", json!("-1m")),
        ("sessions.idle", json!("999999999999d")),
        ("limits.max_connections", json!("many")),
        ("auth.passkey", json!("yes")),
        ("listen", json!(8443)),
        ("transport.compression", json!("gzip")),
        ("workers.uid_min", json!(-5)),
    ] {
        let mut edited = base();
        set(&mut edited, path, value.clone());
        let error = prepare_json(&s, &edited).expect_err(path);
        assert!(error.sentence().contains("is not valid"), "{path} = {value}: {error}");
    }
    // Not JSON at all, not UTF-8, too large.
    let probe = FakeProbe { config: Some("[1, 2".to_owned()), readable: vec![], uid: 1 };
    assert!(prepare(&s.env, &probe).expect_err("broken JSON").sentence().contains("is not valid"));
    let probe = FakeProbe { config: Some("x".repeat(config::MAX_CONFIG_BYTES + 1)), readable: vec![], uid: 1 };
    assert!(prepare(&s.env, &probe).expect_err("too large").sentence().contains("larger than"));
}

#[test]
fn config_the_environment_and_the_file_must_be_there() {
    let s = setup();
    let probe = probe_for(&base());

    let mut env = s.env.clone();
    env.remove("HOLZWAVE_SERVER_KEY_FILE");
    let error = prepare(&env, &probe).expect_err("no key variable");
    assert!(error.sentence().contains("HOLZWAVE_SERVER_KEY_FILE is not set"), "{error}");
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), String::new());
    assert!(prepare(&env, &probe).is_err(), "an empty variable is not a file name");

    // A configuration file that cannot be read names the variable that chooses it.
    let mut env = s.env.clone();
    env.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), "/etc/test/other.json".to_owned());
    let error = prepare(&env, &probe).expect_err("no such file");
    assert!(error.sentence().contains("/etc/test/other.json") && error.sentence().contains("HOLZWAVE_SERVER_CONFIG"), "{error}");

    // Without the variable the default path is used.
    let mut env = BTreeMap::new();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), s.key_path.to_str().expect("utf-8 path").to_owned());
    let error = prepare(&env, &probe).expect_err("default path is not served");
    assert!(error.sentence().contains(config::DEFAULT_CONFIG_PATH), "{error}");
}

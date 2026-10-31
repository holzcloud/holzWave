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
        "directory_url": "https://ca.example.test/directory", "terms_agreed": true}));
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
    set(
        &mut acme_default,
        "tls",
        json!({"mode": "acme", "contact": "admin@example.test", "domains": ["hw.example.test"], "terms_agreed": true}),
    );
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
        "transport": {"compression": "off"}
    });
    let p = prepare_json(&s, &full).expect("a file with every key");
    assert_eq!(p.origin.as_str(), "https://hw.example.test:8443");
    assert_eq!(p.listen, "[::]:9443".parse().expect("address"));
    assert_eq!(p.sessions.idle, Duration::from_secs(1200));
    assert_eq!(p.limits.max_connections, 500);
    assert_eq!(p.limits.ping_interval, Duration::from_secs(20));
    assert_eq!(p.workers.uid_min, 30_000);
    assert_eq!(p.upload.dir, PathBuf::from("incoming/files"));
    assert_eq!(p.transport.compression, Compression::Off);
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

// ---------------------------------------------------------------------------------------------------------------------
// The refusal matrix: a valid file changed in one place, one distinctive word of the sentence
// ---------------------------------------------------------------------------------------------------------------------

/// Removes the key at a dotted path.
fn remove(value: &mut Value, path: &str) {
    let (parent, key) = path.rsplit_once('.').map_or((None, path), |(p, k)| (Some(p), k));
    let mut at = value;
    if let Some(parent) = parent {
        for part in parent.split('.') {
            at = &mut at[part];
        }
    }
    at.as_object_mut().expect("an object").remove(key);
}

/// One change to a valid file.
enum Edit {
    Set(&'static str, Value),
    Remove(&'static str),
}

fn apply(value: &mut Value, edit: &Edit) {
    match edit {
        Edit::Set(path, new) => set(value, path, new.clone()),
        Edit::Remove(path) => remove(value, path),
    }
}

/// The valid file for the mode `acme`.
fn acme_base() -> Value {
    let mut v = base();
    set(
        &mut v,
        "tls",
        json!({"mode": "acme", "contact": "mailto:admin@example.test", "domains": ["hw.example.test"], "terms_agreed": true}),
    );
    v
}

/// The valid file for the mode `proxy`.
fn proxy_base() -> Value {
    let mut v = base();
    set(&mut v, "tls", json!({"mode": "proxy", "trusted_proxies": ["10.0.0.0/8"]}));
    v
}

/// Runs every row against `start` and checks that each is refused with a sentence that holds its word and fits on one line.
/// Prints one `Matrix:` line per row for the record.
fn matrix(group: &str, start: &Value, rows: Vec<(&str, Vec<Edit>, &str)>) {
    let s = setup();
    prepare_json(&s, start).unwrap_or_else(|e| panic!("{group}: the unedited file must be valid: {e}"));
    for (name, edits, word) in rows {
        let mut file = start.clone();
        for edit in &edits {
            apply(&mut file, edit);
        }
        let error = prepare_json(&s, &file).err().unwrap_or_else(|| panic!("{group}/{name}: must be refused"));
        let sentence = error.sentence();
        assert!(sentence.contains(word), "{group}/{name}: {sentence:?} lacks {word:?}");
        assert!(!sentence.contains('\n') && !sentence.is_empty(), "{group}/{name}");
        println!("Matrix: {group}/{name}: {sentence}");
    }
}

#[test]
fn matrix_the_public_url() {
    matrix(
        "public_url",
        &base(),
        vec![
            ("missing", vec![Edit::Remove("public_url")], "public_url is not set"),
            ("empty", vec![Edit::Set("public_url", json!(""))], "public_url is not set"),
            ("http", vec![Edit::Set("public_url", json!("http://hw.example.test"))], "must start with https://"),
            ("no scheme", vec![Edit::Set("public_url", json!("hw.example.test"))], "must start with https://"),
            ("path", vec![Edit::Set("public_url", json!("https://hw.example.test/app"))], "must not have a path"),
            ("query", vec![Edit::Set("public_url", json!("https://hw.example.test/?a=1"))], "must not have a query"),
            ("fragment", vec![Edit::Set("public_url", json!("https://hw.example.test/#top"))], "must not have a fragment"),
            ("user information", vec![Edit::Set("public_url", json!("https://me@hw.example.test"))], "user information"),
            ("bad port", vec![Edit::Set("public_url", json!("https://hw.example.test:99999"))], "port"),
            ("bad host", vec![Edit::Set("public_url", json!("https://hw_example.test"))], "not a valid name"),
            ("ip address with passkeys", vec![Edit::Set("public_url", json!("https://192.0.2.7"))], "IP address"),
            ("ipv6 address with passkeys", vec![Edit::Set("public_url", json!("https://[2001:db8::1]"))], "IP address"),
        ],
    );
}

#[test]
fn matrix_an_ip_address_is_allowed_when_passkeys_are_off() {
    let s = setup();
    let mut file = base();
    set(&mut file, "public_url", json!("https://192.0.2.7:8443"));
    set(&mut file, "auth", json!({"passkey": false, "password_totp": true}));
    let p = prepare_json(&s, &file).expect("an IP host without passkeys");
    assert!(p.origin.host_is_ip());
}

#[test]
fn matrix_the_tls_section() {
    matrix(
        "tls files",
        &base(),
        vec![
            ("no tls section", vec![Edit::Remove("tls")], "no tls section"),
            ("no mode", vec![Edit::Remove("tls.mode")], "is not valid"),
            ("unknown mode", vec![Edit::Set("tls.mode", json!("self-signed"))], "unknown variant `self-signed`"),
            ("no certificate key in the file", vec![Edit::Remove("tls.cert_file")], "needs cert_file and key_file"),
            ("no key key in the file", vec![Edit::Remove("tls.key_file")], "needs cert_file and key_file"),
            ("unreadable certificate", vec![Edit::Set("tls.cert_file", json!("/certs/missing.pem"))], "certificate file /certs/missing.pem"),
            ("unreadable key", vec![Edit::Set("tls.key_file", json!("/certs/missing.key"))], "certificate key file /certs/missing.key"),
        ],
    );
    matrix(
        "tls acme",
        &acme_base(),
        vec![
            ("no domains", vec![Edit::Remove("tls.domains")], "tls.domains"),
            ("empty domains", vec![Edit::Set("tls.domains", json!([]))], "tls.domains"),
            ("a bad domain", vec![Edit::Set("tls.domains", json!(["Not A Name"]))], "not a host name"),
            ("an upper-case domain", vec![Edit::Set("tls.domains", json!(["HW.example.test"]))], "not a host name"),
            ("no contact", vec![Edit::Remove("tls.contact")], "tls.contact"),
            ("blank contact", vec![Edit::Set("tls.contact", json!("  "))], "tls.contact"),
            ("contact is no address", vec![Edit::Set("tls.contact", json!("mailto:admin"))], "email address"),
            ("directory over http", vec![Edit::Set("tls.directory_url", json!("http://ca.example.test/dir"))], "https address"),
            ("unreadable root", vec![Edit::Set("tls.ca_file", json!("/certs/nothing.pem"))], "ACME root file"),
            ("terms not given", vec![Edit::Remove("tls.terms_agreed")], "tls.terms_agreed"),
            ("terms false", vec![Edit::Set("tls.terms_agreed", json!(false))], "tls.terms_agreed"),
            ("terms null", vec![Edit::Set("tls.terms_agreed", json!(null))], "tls.terms_agreed"),
            ("eleven names", vec![Edit::Set("tls.domains", json!([
                "hw.example.test", "a1.example.test", "a2.example.test", "a3.example.test", "a4.example.test",
                "a5.example.test", "a6.example.test", "a7.example.test", "a8.example.test", "a9.example.test",
                "a10.example.test"]))], "more than 10 names"),
            ("a wildcard", vec![Edit::Set("tls.domains", json!(["hw.example.test", "*.example.test"]))], "wildcard"),
            ("an ip address", vec![Edit::Set("tls.domains", json!(["hw.example.test", "192.0.2.1"]))], "IP address"),
            ("an ipv6 address", vec![Edit::Set("tls.domains", json!(["hw.example.test", "2001:db8::1"]))], "IP address"),
            ("an underscore", vec![Edit::Set("tls.domains", json!(["hw.example.test", "ex_ample.test"]))], "not a host name"),
            ("an empty label", vec![Edit::Set("tls.domains", json!(["hw.example.test", "a..b"]))], "not a host name"),
            ("a duplicate", vec![Edit::Set("tls.domains", json!(["hw.example.test", "hw.example.test"]))], "twice"),
            ("a name of 254 characters", vec![Edit::Set("tls.domains", json!(["hw.example.test", format!("{}.test", ["abcdefghi"; 25].join("."))]))], "not a host name"),
            ("a label of 64 characters", vec![Edit::Set("tls.domains", json!(["hw.example.test", format!("{}.test", "a".repeat(64))]))], "not a host name"),
            ("the public host is not a domain", vec![Edit::Set("tls.domains", json!(["www.example.test"]))], "public_url"),
            ("two at signs in the contact", vec![Edit::Set("tls.contact", json!("a@b@c"))], "email address"),
            ("a space in the contact", vec![Edit::Set("tls.contact", json!("ops @example.test"))], "email address"),
            ("a newline in the contact", vec![Edit::Set("tls.contact", json!("ops@example.test\nBcc: x@example.test"))], "email address"),
            ("a query in the contact", vec![Edit::Set("tls.contact", json!("mailto:ops@example.test?subject=x"))], "email address"),
            ("a contact of 255 characters", vec![Edit::Set("tls.contact", json!(format!("{}@example.test", "a".repeat(240))))], "email address"),
            ("a host of the contact that is no name", vec![Edit::Set("tls.contact", json!("ops@exa_mple.test"))], "email address"),
            ("directory without a host", vec![Edit::Set("tls.directory_url", json!("https://"))], "https address"),
            ("directory with a space", vec![Edit::Set("tls.directory_url", json!("https://ca.example.test/a b"))], "https address"),
        ],
    );
}

#[test]
fn matrix_acme_accepts_ten_names_and_an_agreement_given_as_true() {
    let s = setup();
    let names: Vec<String> = std::iter::once("hw.example.test".to_owned())
        .chain((1..10).map(|n| format!("a{n}.example.test")))
        .collect();
    assert_eq!(names.len(), 10);
    let mut file = acme_base();
    set(&mut file, "tls.domains", json!(names));
    prepare_json(&s, &file).expect("ten names");

    // The public host may be any one of the names.
    let mut other = acme_base();
    set(&mut other, "tls.domains", json!(["www.example.test", "hw.example.test"]));
    prepare_json(&s, &other).expect("the public host is the second name");

    for contact in ["ops@example.test", "mailto:ops.team+acme@example.test", "  ops@Example.Test  "] {
        let mut file = acme_base();
        set(&mut file, "tls.contact", json!(contact));
        prepare_json(&s, &file).unwrap_or_else(|e| panic!("{contact:?} must be accepted: {e}"));
    }
}

#[test]
fn matrix_the_proxy_mode_needs_proxies_it_can_trust() {
    matrix(
        "tls proxy",
        &proxy_base(),
        vec![
            ("empty list", vec![Edit::Set("tls.trusted_proxies", json!([]))], "trusted_proxies"),
            ("no list", vec![Edit::Remove("tls.trusted_proxies")], "trusted_proxies"),
            ("not an address", vec![Edit::Set("tls.trusted_proxies", json!(["proxy.example.test"]))], "not an address"),
            ("prefix over 32", vec![Edit::Set("tls.trusted_proxies", json!(["10.0.0.0/33"]))], "longer than 32"),
            ("prefix over 128", vec![Edit::Set("tls.trusted_proxies", json!(["fd00::/129"]))], "longer than 128"),
            ("everything", vec![Edit::Set("tls.trusted_proxies", json!(["0.0.0.0/0"]))], "every address"),
            ("one bad entry among good ones", vec![Edit::Set("tls.trusted_proxies", json!(["10.0.0.0/8", "nope"]))], "nope"),
        ],
    );
    let s = setup();
    for good in [json!(["10.0.0.0/8"]), json!(["192.0.2.1"]), json!(["::1", "fd00::/8", "172.16.0.0/12"])] {
        let mut file = proxy_base();
        set(&mut file, "tls.trusted_proxies", good.clone());
        prepare_json(&s, &file).unwrap_or_else(|e| panic!("{good} must be accepted: {e}"));
    }
}

#[test]
fn matrix_a_sign_in_method_is_required_and_a_provider_must_be_complete() {
    let oidc = json!([{"id": "corp", "name": "Corp", "issuer": "https://login.example.test", "client_id": "abc",
        "client_secret_file": "/secrets/oidc"}]);
    let mut with_oidc = base();
    set(&mut with_oidc, "auth", json!({"oidc": oidc}));
    matrix(
        "auth",
        &base(),
        vec![
            ("all off", vec![Edit::Set("auth", json!({"passkey": false, "password_totp": false, "oidc": []}))], "no sign-in method"),
            ("no auth section", vec![Edit::Remove("auth")], "no sign-in method"),
            ("empty auth section", vec![Edit::Set("auth", json!({}))], "no sign-in method"),
        ],
    );
    matrix(
        "oidc",
        &with_oidc,
        vec![
            ("unreadable secret file", vec![Edit::Set("auth.oidc", json!([{"id": "corp", "name": "Corp", "issuer": "https://login.example.test",
                "client_id": "abc", "client_secret_file": "/secrets/none"}]))], "client secret file"),
            ("issuer over http", vec![Edit::Set("auth.oidc", json!([{"id": "corp", "name": "Corp", "issuer": "http://login.example.test",
                "client_id": "abc", "client_secret_file": "/secrets/oidc"}]))], "issuer must be an https"),
            ("bad id", vec![Edit::Set("auth.oidc", json!([{"id": "Corp!", "name": "Corp", "issuer": "https://login.example.test",
                "client_id": "abc", "client_secret_file": "/secrets/oidc"}]))], "lower-case"),
            ("no client id", vec![Edit::Set("auth.oidc", json!([{"id": "corp", "name": "Corp", "issuer": "https://login.example.test",
                "client_id": " ", "client_secret_file": "/secrets/oidc"}]))], "client_id"),
            ("no secret in the file", vec![Edit::Set("auth.oidc", json!([{"id": "corp", "name": "Corp", "issuer": "https://login.example.test",
                "client_id": "abc"}]))], "client_secret_file"),
            ("twice", vec![Edit::Set("auth.oidc", json!([
                {"id": "corp", "name": "Corp", "issuer": "https://login.example.test", "client_id": "abc", "client_secret_file": "/secrets/oidc"},
                {"id": "corp", "name": "Other", "issuer": "https://other.example.test", "client_id": "def", "client_secret_file": "/secrets/oidc"}]))], "used twice"),
        ],
    );
}

#[test]
fn matrix_the_worker_user_range() {
    matrix(
        "workers",
        &base(),
        vec![
            ("below 1000", vec![Edit::Set("workers.uid_min", json!(999)), Edit::Set("workers.uid_max", json!(1500))], "below 1000"),
            ("root", vec![Edit::Set("workers.uid_min", json!(0))], "below 1000"),
            ("reversed", vec![Edit::Set("workers.uid_min", json!(30_000)), Edit::Set("workers.uid_max", json!(20_000))], "reversed"),
            ("contains the server's own id", vec![Edit::Set("workers.uid_min", json!(10_000)), Edit::Set("workers.uid_max", json!(10_002))], "itself"),
            ("ends at the largest id", vec![Edit::Set("workers.uid_max", json!(u32::MAX))], "reserves"),
            ("relative helper", vec![Edit::Set("workers.helper_path", json!("helper"))], "absolute"),
            ("relative worker", vec![Edit::Set("workers.worker_path", json!("bin/server"))], "absolute"),
        ],
    );
}

#[test]
fn matrix_a_duration_or_a_limit_outside_its_bounds() {
    matrix(
        "bounds",
        &base(),
        vec![
            ("idle zero", vec![Edit::Set("sessions.idle", json!("0s"))], "sessions.idle is 0s"),
            ("absolute too long", vec![Edit::Set("sessions.absolute", json!("200d"))], "sessions.absolute is 200d"),
            ("idle above absolute", vec![Edit::Set("sessions.idle", json!("2d"))], "longer than sessions.absolute"),
            ("step up too long", vec![Edit::Set("sessions.step_up", json!("5h"))], "sessions.step_up"),
            ("handshake too long", vec![Edit::Set("limits.handshake_timeout", json!("2h"))], "limits.handshake_timeout is 2h"),
            ("no connections", vec![Edit::Set("limits.max_connections", json!(0))], "limits.max_connections is 0"),
            ("too many connections", vec![Edit::Set("limits.max_connections", json!(10_000_000))], "limits.max_connections"),
            ("per address above total", vec![Edit::Set("limits.max_connections", json!(10)), Edit::Set("limits.max_connections_per_address", json!(20))], "must not be larger"),
            ("body too small", vec![Edit::Set("limits.api_body_bytes", json!(10))], "limits.api_body_bytes"),
            ("lock for a week", vec![Edit::Set("limits.lock_duration", json!("7d"))], "limits.lock_duration"),
            ("no panes", vec![Edit::Set("workers.max_panes", json!(0))], "workers.max_panes"),
            ("too many panes", vec![Edit::Set("workers.max_panes", json!(100_000))], "workers.max_panes"),
            ("scrollback", vec![Edit::Set("workers.max_scrollback_lines", json!(1))], "workers.max_scrollback_lines"),
            ("process limit", vec![Edit::Set("workers.nproc", json!(1))], "workers.nproc"),
            ("file limit", vec![Edit::Set("workers.nofile", json!(5))], "workers.nofile"),
            ("worker idle", vec![Edit::Set("workers.idle_timeout", json!("10s"))], "workers.idle_timeout"),
            ("upload zero", vec![Edit::Set("upload.max_bytes", json!(0))], "upload.max_bytes"),
            ("upload huge", vec![Edit::Set("upload.max_bytes", json!(u64::MAX))], "upload.max_bytes"),
            ("upload outside the home", vec![Edit::Set("upload.dir", json!("../elsewhere"))], "upload.dir"),
            ("upload absolute", vec![Edit::Set("upload.dir", json!("/tmp"))], "upload.dir"),
            ("upload empty", vec![Edit::Set("upload.dir", json!(""))], "upload.dir"),
            ("listen", vec![Edit::Set("listen", json!("everywhere"))], "listen"),
            ("relative data dir", vec![Edit::Set("data_dir", json!("data"))], "data_dir"),
            ("relative runtime dir", vec![Edit::Set("runtime_dir", json!("run"))], "runtime_dir"),
        ],
    );
}

#[test]
fn matrix_the_key_file() {
    let s = setup();
    let file = base();
    let probe = probe_for(&file);
    let dir = Scratch::new("matrix-key");
    let run = |key_path: &Path| {
        let mut env = s.env.clone();
        env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), key_path.to_str().expect("utf-8 path").to_owned());
        prepare(&env, &probe)
    };
    run(&s.key_path).expect("the good key file");

    let missing = dir.join("missing.key");
    let loose = dir.write_file("loose.key", key_line([4; 32]).as_bytes(), 0o644);
    let malformed = dir.write_file("malformed.key", CANARY.as_bytes(), 0o600);
    let zero = dir.write_file("zero.key", key_line([0; 32]).as_bytes(), 0o600);
    let group_read = dir.write_file("mounted.key", key_line([4; 32]).as_bytes(), 0o440);
    let link = dir.join("link.key");
    symlink(&s.key_path, &link).expect("make a link");
    for (name, path, word) in [
        ("missing", &missing, "does not exist"),
        ("loose", &loose, "too open"),
        ("malformed", &malformed, "not a key made by"),
        ("all zero", &zero, "all-zero"),
        ("link", &link, "is a link"),
    ] {
        let error = run(path).err().unwrap_or_else(|| panic!("{name}: must be refused"));
        assert!(error.sentence().contains(word), "{name}: {error}");
        assert!(error.sentence().contains(path.to_str().expect("utf-8 path")), "{name}: names the file: {error}");
        assert!(!error.sentence().contains(CANARY), "{name}: {error}");
        println!("Matrix: key/{name}: {}", error.sentence().replacen(dir.path().to_str().expect("utf-8 path"), "<dir>", 1));
    }
    run(&group_read).expect("a mounted secret with the group read bit is accepted");
    run(&dir.write_file("0400.key", key_line([4; 32]).as_bytes(), 0o400)).expect("a read-only key file");

    // The variable itself.
    let mut env = s.env.clone();
    env.remove("HOLZWAVE_SERVER_KEY_FILE");
    let error = prepare(&env, &probe).expect_err("no variable");
    println!("Matrix: key/no variable: {error}");
}

#[test]
fn matrix_a_file_that_is_valid_apart_from_the_key_still_loads_the_key_last() {
    // The cheap and informative refusals come first: a broken file is reported even when the key file is also bad.
    let s = setup();
    let mut env = s.env.clone();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), "/nonexistent/key".to_owned());
    let mut file = base();
    remove(&mut file, "public_url");
    let error = prepare(&env, &probe_for(&file)).expect_err("two problems");
    assert!(error.sentence().contains("public_url"), "{error}");
}

// ---------------------------------------------------------------------------------------------------------------------
// The prohibitions: no mode without sign-in, no default administrator, no plain public listener
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn prohibited_no_configuration_serves_without_sign_in() {
    let s = setup();
    let cases: Vec<(&str, Value, &str)> = {
        let mut off = base();
        set(&mut off, "auth", json!({"passkey": false, "password_totp": false, "oidc": []}));
        let mut disabled = base();
        set(&mut disabled, "auth.disabled", json!(true));
        let mut admin = base();
        set(&mut admin, "admin_password", json!("x"));
        let mut none = base();
        set(&mut none, "tls", json!({"mode": "none"}));
        let mut insecure = base();
        set(&mut insecure, "insecure", json!(true));
        vec![
            ("auth all off", off, "no sign-in method"),
            ("auth.disabled", disabled, "unknown field `disabled`"),
            ("admin_password", admin, "unknown field `admin_password`"),
            ("tls.mode none", none, "unknown variant `none`"),
            ("insecure", insecure, "unknown field `insecure`"),
        ]
    };
    for (name, file, word) in cases {
        let error = prepare_json(&s, &file).err().unwrap_or_else(|| panic!("{name}: must be refused"));
        assert!(error.sentence().contains(word), "{name}: {error}");
        println!("Prohibitions: {name}: {error}");
    }
}

#[test]
fn prohibited_other_spellings_of_the_same_things_are_unknown_too() {
    let s = setup();
    for (path, value) in [
        ("auth.enabled", json!(false)),
        ("auth.none", json!(true)),
        ("auth.anonymous", json!(true)),
        ("auth.required", json!(false)),
        ("admin", json!({"user": "admin", "password": "admin"})),
        ("admin_user", json!("admin")),
        ("password", json!("secret")),
        ("default_admin", json!(true)),
        ("bootstrap_password", json!("x")),
        ("no_auth", json!(true)),
        ("tls.insecure", json!(true)),
        ("tls.plain", json!(true)),
        ("tls.disabled", json!(true)),
        ("http", json!({"listen": "0.0.0.0:80"})),
        ("listen_plain", json!("0.0.0.0:80")),
    ] {
        let mut file = base();
        set(&mut file, path, value);
        let error = prepare_json(&s, &file).err().unwrap_or_else(|| panic!("{path}: must be refused"));
        assert!(error.sentence().contains("unknown field"), "{path}: {error}");
    }
    for mode in ["none", "http", "plain", "off", "disabled", ""] {
        let mut file = base();
        set(&mut file, "tls", json!({"mode": mode}));
        assert!(prepare_json(&s, &file).is_err(), "tls.mode {mode:?} must be refused");
    }
}

#[test]
fn prohibited_nothing_defaults_open() {
    let s = setup();
    // The auth section starts with every method off.
    let auth = config::Auth::default();
    assert!(!auth.passkey && !auth.password_totp && auth.oidc.is_empty());
    // An empty file, a file without TLS and a file without a method are refused, each for its own missing piece.
    let probe = |v: Value| FakeProbe { config: Some(v.to_string()), readable: vec![], uid: 1 };
    let word_of = |v: Value| prepare(&s.env, &probe(v)).expect_err("refused").sentence().to_owned();
    assert!(word_of(json!({})).contains("public_url is not set"));
    assert!(word_of(json!({"public_url": "https://hw.example.test"})).contains("no tls section"));
    assert!(
        word_of(json!({"public_url": "https://hw.example.test", "tls": {"mode": "proxy", "trusted_proxies": ["10.0.0.0/8"]}}))
            .contains("no sign-in method")
    );
    // A proxy-mode file with sign-in, a public URL and a proxy list is the only way to a plain listener.
    prepare(
        &s.env,
        &probe(json!({"public_url": "https://hw.example.test", "auth": {"password_totp": true},
            "tls": {"mode": "proxy", "trusted_proxies": ["10.0.0.0/8"]}})),
    )
    .expect("the one plain-text mode, with its list");
}

#[test]
fn prohibited_the_key_never_reaches_a_sentence_or_an_output() {
    let dir = Scratch::new("canary");
    let key = dir.write_file("canary.key", format!("{CANARY}\n").as_bytes(), 0o600);
    let good_key = dir.join("good.key");
    keyfile::generate(&good_key).expect("generate");
    let good_line = std::fs::read_to_string(&good_key).expect("read the key");
    let secret = good_line.trim_end().strip_prefix("hws1:").expect("prefix").to_owned();

    // 1. The refusal sentence of a malformed file holds none of its content.
    let sentence = ServerKey::load(&key).expect_err("malformed").to_string();
    assert!(!sentence.contains(CANARY));

    // 2. The commands print no key: check-config with the canary file, and with the good file.
    let cert = dir.write_file("cert.pem", b"cert", 0o644);
    let cert_key = dir.write_file("cert.key", b"key", 0o600);
    let config = dir.write_file(
        "server.json",
        json!({"public_url": "https://hw.example.test", "auth": {"passkey": true},
            "tls": {"mode": "files", "cert_file": cert, "key_file": cert_key}})
        .to_string()
        .as_bytes(),
        0o644,
    );
    for (key_path, expect_ok) in [(&key, false), (&good_key, true)] {
        let out = server(
            &["check-config"],
            &[
                ("HOLZWAVE_SERVER_CONFIG", config.to_str().expect("utf-8 path")),
                ("HOLZWAVE_SERVER_KEY_FILE", key_path.to_str().expect("utf-8 path")),
            ],
        );
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert_eq!(out.status.success(), expect_ok, "stdout: {stdout} stderr: {stderr}");
        for seen in [&stdout, &stderr] {
            assert!(!seen.contains(CANARY) && !seen.contains(&secret), "output holds key material: {seen}");
        }
    }

    // 3. A key that is derived is not the key, and a derived key is not in the key's Debug text.
    let loaded = ServerKey::load(&good_key).expect("load");
    assert!(!format!("{loaded:?}").contains(&secret));
}

// ---------------------------------------------------------------------------------------------------------------------
// The command line
// ---------------------------------------------------------------------------------------------------------------------

/// A scratch directory that holds a valid configuration (mode `files`) and a key file made by `keygen`.
struct Installed {
    dir: Scratch,
    config: PathBuf,
    key: PathBuf,
}

fn installed(config: &Value) -> Installed {
    let dir = Scratch::new("cli");
    let cert = dir.write_file("cert.pem", b"-----BEGIN CERTIFICATE-----", 0o644);
    let cert_key = dir.write_file("cert.key", b"-----BEGIN PRIVATE KEY-----", 0o600);
    let mut config = config.clone();
    set(&mut config, "tls", json!({"mode": "files", "cert_file": cert, "key_file": cert_key}));
    let config_path = dir.write_file("server.json", config.to_string().as_bytes(), 0o644);
    let key = dir.join("server.key");
    assert_eq!(server(&["keygen", "--out", key.to_str().expect("utf-8 path")], &[]).status.code(), Some(0));
    Installed { dir, config: config_path, key }
}

fn check_config(setup: &Installed) -> Output {
    server(
        &["check-config"],
        &[
            ("HOLZWAVE_SERVER_CONFIG", setup.config.to_str().expect("utf-8 path")),
            ("HOLZWAVE_SERVER_KEY_FILE", setup.key.to_str().expect("utf-8 path")),
        ],
    )
}

#[test]
fn cli_check_config_says_ok_for_a_valid_setup() {
    let setup = installed(&base());
    let out = check_config(&setup);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "configuration ok\n");
    assert!(out.stderr.is_empty());
    let _ = &setup.dir;
}

#[test]
fn cli_check_config_refuses_an_invalid_setup_with_one_sentence_on_stderr() {
    let mut file = base();
    set(&mut file, "auth", json!({"passkey": false}));
    let setup = installed(&file);
    let out = check_config(&setup);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "nothing on stdout: {}", text(&out.stdout));
    let err = text(&out.stderr);
    assert!(err.contains("no sign-in method"), "{err}");
    assert_eq!(err.lines().count(), 1, "one sentence: {err}");

    // A loose key file, with the sentence naming it.
    let setup = installed(&base());
    std::fs::set_permissions(&setup.key, std::fs::Permissions::from_mode(0o666)).expect("loosen the key file");
    let out = check_config(&setup);
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains(setup.key.to_str().expect("utf-8 path")) && err.contains("too open"), "{err}");

    // An unknown key in the real file.
    let setup = installed(&json!({"public_url": "https://hw.example.test", "auth": {"passkey": true}, "insecure": true}));
    let out = check_config(&setup);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("unknown field `insecure`"), "{}", text(&out.stderr));
}

#[test]
fn cli_check_config_with_no_environment_names_the_missing_variable() {
    let out = server(&["check-config"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("HOLZWAVE_SERVER_KEY_FILE"), "{}", text(&out.stderr));

    // With the key variable and no file at the default place, the file is what is missing, and the sentence says how to name it.
    let dir = Scratch::new("cli-env");
    let key = dir.join("k");
    server(&["keygen", "--out", key.to_str().expect("utf-8 path")], &[]);
    let out = server(&["check-config"], &[("HOLZWAVE_SERVER_KEY_FILE", key.to_str().expect("utf-8 path"))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("HOLZWAVE_SERVER_CONFIG"), "{}", text(&out.stderr));
}

#[test]
fn cli_the_usage_lists_the_commands_and_a_bad_call_is_status_two() {
    let out = server(&[], &[]);
    assert_eq!(out.status.code(), Some(2));
    let usage = text(&out.stderr);
    assert!(usage.contains("keygen") && usage.contains("check-config") && usage.contains("serve"), "{usage}");
    assert_eq!(server(&["check-config", "extra"], &[]).status.code(), Some(2));
    assert_eq!(server(&["serve", "extra"], &[]).status.code(), Some(2), "serve takes no arguments");
    // With no environment `serve` is refused by the same check as `check-config`: status 1, one sentence, nothing started.
    let out = server(&["serve"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(text(&out.stderr).contains("HOLZWAVE_SERVER_KEY_FILE"), "{}", text(&out.stderr));
}

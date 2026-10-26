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

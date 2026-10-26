//! The encrypted secret store: the format, the locked state, the slots, tampering, two writers and redaction.

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;

use holzwave_core::secrets::SecretStore;
use holzwave_server::unlock::Kek;
use holzwave_server::worker::secretstore::{EncryptedFileStore, LOCKED, SlotKind, StoreError, is_locked};

use scratch::Scratch;

fn kek(n: u8) -> Kek {
    Kek::from_bytes(&[n; 32])
}

fn id(n: u8) -> [u8; 16] {
    [n; 16]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn new_store(dir: &Scratch) -> (EncryptedFileStore, std::path::PathBuf) {
    let path = dir.join("secrets.store");
    let store = EncryptedFileStore::create(&path, SlotKind::Password, id(1), &kek(1)).expect("create the store");
    (store, path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn roundtrip_a_secret_survives_a_lock_and_an_unlock() {
    let dir = Scratch::new("store-roundtrip");
    let (store, _) = new_store(&dir);
    store.set("ssh:a", "hunter2").unwrap();
    assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("hunter2"));
    store.lock();
    let error = store.get("ssh:a").unwrap_err();
    assert_eq!(error.to_string(), "secrets are locked");
    assert!(is_locked(&error));
    store.unlock(SlotKind::Password, &kek(1)).unwrap();
    assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("hunter2"));
}

#[test]
fn roundtrip_the_file_shows_neither_the_secret_nor_its_name() {
    let dir = Scratch::new("store-canary");
    let (store, path) = new_store(&dir);
    store.set("ssh:a", "hunter2").unwrap();
    store.set("token:canary-name", "canary-value-7c1e4b").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    for needle in ["hunter2", "ssh:a", "token:canary-name", "canary-value-7c1e4b", "canary"] {
        assert!(!contains(&bytes, needle.as_bytes()), "{needle} is in the file");
    }
    assert_eq!(&bytes[..4], b"HWSS");
}

#[test]
fn roundtrip_the_file_mode_is_0600() {
    let dir = Scratch::new("store-mode");
    let (store, path) = new_store(&dir);
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    store.set("a", "b").unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn roundtrip_a_second_object_reads_the_value_with_the_same_key() {
    let dir = Scratch::new("store-second");
    let (store, path) = new_store(&dir);
    store.set("ssh:a", "hunter2").unwrap();
    let second = EncryptedFileStore::open(&path).unwrap();
    assert!(!second.is_unlocked());
    assert!(is_locked(&second.get("ssh:a").unwrap_err()));
    second.unlock(SlotKind::Password, &kek(1)).unwrap();
    assert_eq!(second.get("ssh:a").unwrap().as_deref(), Some("hunter2"));
}

#[test]
fn roundtrip_create_twice_is_refused_and_keeps_the_first_store() {
    let dir = Scratch::new("store-twice");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = EncryptedFileStore::create(&path, SlotKind::Password, id(9), &kek(9)).unwrap_err();
    assert!(matches!(error, StoreError::Exists), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn roundtrip_get_set_delete_behave_like_the_memory_store() {
    let dir = Scratch::new("store-memory-like");
    let (store, _) = new_store(&dir);
    assert_eq!(store.get("ssh:a").unwrap(), None);
    store.set("ssh:a", "one").unwrap();
    store.set("ssh:a", "two").unwrap();
    assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("two"));
    store.delete("ssh:a").unwrap();
    store.delete("ssh:a").unwrap();
    assert_eq!(store.get("ssh:a").unwrap(), None);
    assert!(store.set("", "x").is_err());
    assert!(store.set("a\nb", "x").is_err());
    assert!(store.set("ssh:a", &"x".repeat(4097)).is_err());
    assert!(store.set("ssh:a", &"x".repeat(4096)).is_ok());
    assert!(store.get(&"a".repeat(4097)).is_err());
}

#[test]
fn roundtrip_a_locked_store_fails_every_operation_with_the_one_text() {
    let dir = Scratch::new("store-locked");
    let (store, path) = new_store(&dir);
    store.set("a", "b").unwrap();
    let opened = EncryptedFileStore::open(&path).unwrap();
    for error in [opened.get("a").unwrap_err(), opened.set("a", "c").unwrap_err(), opened.delete("a").unwrap_err()] {
        assert_eq!(error.to_string(), LOCKED);
    }
    assert_eq!(LOCKED, "secrets are locked");
}

#[test]
fn roundtrip_format_of_an_empty_store() {
    let dir = Scratch::new("store-format");
    let (_store, path) = new_store(&dir);
    let bytes = std::fs::read(&path).unwrap();
    eprintln!("Format: {}", hex(&bytes[..64]));
    assert_eq!(&bytes[..4], b"HWSS");
    assert_eq!(&bytes[4..6], &[0, 1]);
    assert_eq!(bytes[6], 1);
    assert_eq!(bytes[7], 1);
    // magic, version, count, one slot, body nonce, `{}` and its tag.
    assert_eq!(bytes.len(), 7 + 89 + 24 + 2 + 16);
}

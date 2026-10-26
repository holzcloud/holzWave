//! The encrypted secret store: the format, the locked state, the slots, tampering, two writers and redaction.

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use holzwave_core::secrets::SecretStore;
use holzwave_server::unlock::{
    Kdf, Kek, UnlockError, argon2id_into, generate_recovery_code, kek_from_passphrase, kek_from_prf, kek_from_recovery,
    parse_recovery_code,
};
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

// ---------------------------------------------------------------------------------------------------------------------
// Slots, the derivations and the recovery code.

/// The body of a store file (the body nonce to the end), by the slot count in the header.
fn body_of(bytes: &[u8]) -> Vec<u8> {
    bytes[7 + usize::from(bytes[6]) * 89..].to_vec()
}

fn slots_of(store: &EncryptedFileStore) -> usize {
    store.slots().unwrap().len()
}

#[test]
fn slot_each_of_the_four_kinds_opens_the_same_entries() {
    let dir = Scratch::new("store-slot-kinds");
    let path = dir.join("secrets.store");
    let kdf = Kdf::lower_bound();

    // The password module's split: 64 bytes, the first 32 the verifier, the last 32 the key-encryption key.
    let mut split = [0u8; 64];
    argon2id_into(b"correct password", &[5u8; 16], &kdf, &mut split).unwrap();
    let password = Kek::from_bytes(split[32..].try_into().unwrap());
    let passphrase = kek_from_passphrase("a passphrase", &[6u8; 16], &kdf).unwrap();
    let prf = kek_from_prf(&[7u8; 32], &[8u8; 32]).unwrap();
    let code = generate_recovery_code().unwrap();
    let recovery = kek_from_recovery(&code, &[9u8; 16]).unwrap();

    let store = EncryptedFileStore::create(&path, SlotKind::Password, id(1), &password).unwrap();
    store.add_slot(SlotKind::Passphrase, id(2), &passphrase).unwrap();
    store.add_slot(SlotKind::Prf, id(3), &prf).unwrap();
    store.add_slot(SlotKind::Recovery, id(4), &recovery).unwrap();
    store.set("ssh:a", "hunter2").unwrap();
    store.set("ai:key", "sk-test").unwrap();

    for (kind, key) in
        [(SlotKind::Password, &password), (SlotKind::Passphrase, &passphrase), (SlotKind::Prf, &prf), (SlotKind::Recovery, &recovery)]
    {
        let fresh = EncryptedFileStore::open(&path).unwrap();
        fresh.unlock(kind, key).unwrap();
        assert_eq!(fresh.get("ssh:a").unwrap().as_deref(), Some("hunter2"), "{kind:?}");
        assert_eq!(fresh.get("ai:key").unwrap().as_deref(), Some("sk-test"), "{kind:?}");
    }
    // A key opens only the slots of its own kind.
    let fresh = EncryptedFileStore::open(&path).unwrap();
    assert!(is_locked(&fresh.unlock(SlotKind::Prf, &password).unwrap_err().into()));
    assert!(fresh.unlock(SlotKind::Recovery, &passphrase).is_err());
    assert!(!fresh.is_unlocked());
}

#[test]
fn slot_changes_leave_the_body_bytes_alone() {
    let dir = Scratch::new("store-slot-body");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    store.set("b", "two").unwrap();
    let body = body_of(&std::fs::read(&path).unwrap());

    store.add_slot(SlotKind::Passphrase, id(2), &kek(2)).unwrap();
    assert_eq!(body_of(&std::fs::read(&path).unwrap()), body, "after add");
    store.replace_slot(SlotKind::Passphrase, id(2), &kek(3)).unwrap();
    assert_eq!(body_of(&std::fs::read(&path).unwrap()), body, "after replace");
    store.remove_slot(SlotKind::Passphrase, id(2)).unwrap();
    assert_eq!(body_of(&std::fs::read(&path).unwrap()), body, "after remove");
    assert_eq!(store.get("a").unwrap().as_deref(), Some("one"));
}

#[test]
fn slot_replace_and_remove_end_the_old_key() {
    let dir = Scratch::new("store-slot-replace");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    store.add_slot(SlotKind::Passphrase, id(2), &kek(2)).unwrap();

    store.replace_slot(SlotKind::Passphrase, id(2), &kek(3)).unwrap();
    let fresh = EncryptedFileStore::open(&path).unwrap();
    assert!(fresh.unlock(SlotKind::Passphrase, &kek(2)).is_err());
    fresh.unlock(SlotKind::Passphrase, &kek(3)).unwrap();
    assert_eq!(fresh.get("a").unwrap().as_deref(), Some("one"));

    store.remove_slot(SlotKind::Passphrase, id(2)).unwrap();
    assert_eq!(slots_of(&store), 1);
    let fresh = EncryptedFileStore::open(&path).unwrap();
    assert!(fresh.unlock(SlotKind::Passphrase, &kek(3)).is_err());
    fresh.unlock(SlotKind::Password, &kek(1)).unwrap();
}

#[test]
fn slot_the_last_slot_cannot_be_removed() {
    let dir = Scratch::new("store-slot-last");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    let before = std::fs::read(&path).unwrap();
    let error = store.remove_slot(SlotKind::Password, id(1)).unwrap_err();
    assert!(matches!(error, StoreError::Slot(_)), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(slots_of(&store), 1);
}

#[test]
fn slot_a_seventeenth_is_refused_and_a_sixteenth_is_not() {
    let dir = Scratch::new("store-slot-many");
    let (store, path) = new_store(&dir);
    for n in 2..=16u8 {
        store.add_slot(SlotKind::Prf, id(n), &kek(n)).unwrap();
    }
    assert_eq!(slots_of(&store), 16);
    let before = std::fs::read(&path).unwrap();
    let error = store.add_slot(SlotKind::Prf, id(17), &kek(17)).unwrap_err();
    assert!(matches!(error, StoreError::Slot(_)), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // Every one of the sixteen opens the store.
    for n in 2..=16u8 {
        let fresh = EncryptedFileStore::open(&path).unwrap();
        fresh.unlock(SlotKind::Prf, &kek(n)).unwrap();
    }
}

#[test]
fn slot_the_same_kind_and_id_twice_and_an_unknown_slot_are_refused() {
    let dir = Scratch::new("store-slot-dup");
    let (store, _) = new_store(&dir);
    assert!(matches!(store.add_slot(SlotKind::Password, id(1), &kek(2)).unwrap_err(), StoreError::Slot(_)));
    assert!(matches!(store.replace_slot(SlotKind::Prf, id(1), &kek(2)).unwrap_err(), StoreError::Slot(_)));
    assert!(matches!(store.remove_slot(SlotKind::Prf, id(1)).unwrap_err(), StoreError::Slot(_)));
    // The same id under another kind is another slot.
    store.add_slot(SlotKind::Prf, id(1), &kek(2)).unwrap();
}

#[test]
fn slot_a_locked_store_refuses_every_slot_change_and_changes_nothing() {
    let dir = Scratch::new("store-slot-locked");
    let (store, path) = new_store(&dir);
    store.add_slot(SlotKind::Passphrase, id(2), &kek(2)).unwrap();
    store.lock();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(store.add_slot(SlotKind::Prf, id(3), &kek(3)).unwrap_err(), StoreError::Locked));
    assert!(matches!(store.replace_slot(SlotKind::Passphrase, id(2), &kek(3)).unwrap_err(), StoreError::Locked));
    assert!(matches!(store.remove_slot(SlotKind::Passphrase, id(2)).unwrap_err(), StoreError::Locked));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(store.slots().unwrap().len(), 2);
}

#[test]
fn kdf_the_default_is_the_contract_s() {
    let kdf = Kdf::default();
    assert_eq!((kdf.m_kib(), kdf.t(), kdf.p()), (64 * 1024, 3, 1));
    eprintln!("Kdf: {kdf:?}");
}

#[test]
fn kdf_the_bounds_are_enforced_on_every_side() {
    let min = 19 * 1024;
    let max = 256 * 1024;
    assert!(Kdf::new(min, 2, 1).is_ok());
    assert!(Kdf::new(max, 10, 4).is_ok());
    for (m, t, p) in [(min - 1, 2, 1), (max + 1, 2, 1), (min, 1, 1), (min, 11, 1), (min, 2, 0), (min, 2, 5), (0, 0, 0), (u32::MAX, 3, 1)] {
        assert_eq!(Kdf::new(m, t, p).unwrap_err(), UnlockError::KdfOutOfBounds, "{m} {t} {p}");
    }
    assert_eq!(Kdf::lower_bound(), Kdf::new(min, 2, 1).unwrap());
}

#[test]
fn kdf_a_passphrase_key_is_deterministic_and_depends_on_every_input() {
    let kdf = Kdf::lower_bound();
    let a = kek_from_passphrase("one passphrase", &[1u8; 16], &kdf).unwrap();
    let again = kek_from_passphrase("one passphrase", &[1u8; 16], &kdf).unwrap();
    let other_salt = kek_from_passphrase("one passphrase", &[2u8; 16], &kdf).unwrap();
    let other_text = kek_from_passphrase("another passphrase", &[1u8; 16], &kdf).unwrap();
    let other_kdf = kek_from_passphrase("one passphrase", &[1u8; 16], &Kdf::new(19 * 1024, 3, 1).unwrap()).unwrap();
    assert!(a.same_as(&again));
    assert!(!a.same_as(&other_salt));
    assert!(!a.same_as(&other_text));
    assert!(!a.same_as(&other_kdf));
    assert_eq!(kek_from_passphrase(&"x".repeat(4097), &[1u8; 16], &kdf).unwrap_err(), UnlockError::BadLength);
}

#[test]
fn kdf_argon2id_into_takes_16_to_64_bytes_of_output() {
    let kdf = Kdf::lower_bound();
    for len in [0usize, 15, 65, 1000] {
        let mut out = vec![0u8; len];
        assert_eq!(argon2id_into(b"x", &[1u8; 16], &kdf, &mut out).unwrap_err(), UnlockError::BadLength, "{len}");
    }
    for len in [16usize, 32, 64] {
        let mut out = vec![0u8; len];
        argon2id_into(b"x", &[1u8; 16], &kdf, &mut out).unwrap();
        assert_ne!(out, vec![0u8; len]);
    }
}

#[test]
fn kdf_prf_and_recovery_keys_are_deterministic_and_depend_on_the_salt() {
    let prf = kek_from_prf(&[1u8; 32], &[2u8; 32]).unwrap();
    assert!(prf.same_as(&kek_from_prf(&[1u8; 32], &[2u8; 32]).unwrap()));
    assert!(!prf.same_as(&kek_from_prf(&[1u8; 32], &[3u8; 32]).unwrap()));
    assert!(!prf.same_as(&kek_from_prf(&[4u8; 32], &[2u8; 32]).unwrap()));
    assert_eq!(kek_from_prf(&[1u8; 32], &[2u8; 15]).unwrap_err(), UnlockError::BadLength);

    let code = parse_recovery_code("0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ").unwrap();
    let recovery = kek_from_recovery(&code, &[2u8; 16]).unwrap();
    assert!(recovery.same_as(&kek_from_recovery(&code, &[2u8; 16]).unwrap()));
    assert!(!recovery.same_as(&kek_from_recovery(&code, &[3u8; 16]).unwrap()));
    assert_eq!(kek_from_recovery(&code, &[2u8; 65]).unwrap_err(), UnlockError::BadLength);
    // The two kinds never give the same key, even for the same salt and a related input.
    assert!(!prf.same_as(&recovery));
}

fn first4(kek: &Kek) -> String {
    hex(&kek.expose()[..4])
}

#[test]
fn kdf_known_answers_make_a_change_of_a_derivation_visible() {
    let kdf = Kdf::lower_bound();
    let passphrase = first4(&kek_from_passphrase("correct horse", &[1u8; 16], &kdf).unwrap());
    let default = first4(&kek_from_passphrase("correct horse", &[1u8; 16], &Kdf::default()).unwrap());
    let prf = first4(&kek_from_prf(&[2u8; 32], &[3u8; 16]).unwrap());
    let code = parse_recovery_code("0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ").unwrap();
    let recovery = first4(&kek_from_recovery(&code, &[4u8; 16]).unwrap());
    let mut split = [0u8; 64];
    argon2id_into(b"correct horse", &[1u8; 16], &kdf, &mut split).unwrap();
    let verifier = hex(&split[..4]);
    let password_kek = hex(&split[32..36]);
    eprintln!(
        "Known answers: passphrase(lower bound)={passphrase} passphrase(default)={default} prf={prf} recovery={recovery} \
         password verifier={verifier} password kek={password_kek}"
    );
    assert_eq!(passphrase, "814eaae1");
    assert_eq!(default, "dcc3ec3c");
    assert_eq!(prf, "ce23a5c0");
    assert_eq!(recovery, "53c49383");
    assert_eq!(verifier, "758f813c");
    assert_eq!(password_kek, "a43c7ae8");
}

#[test]
fn recovery_a_code_is_eight_groups_of_four_crockford_characters() {
    let alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let text = generate_recovery_code().unwrap().display();
    let groups: Vec<&str> = text.split('-').collect();
    assert_eq!(groups.len(), 8, "{}", groups.len());
    for group in &groups {
        assert_eq!(group.len(), 4);
        assert!(group.chars().all(|c| alphabet.contains(c)));
    }
}

#[test]
fn recovery_codes_never_repeat_in_a_thousand_draws() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1000 {
        let text = generate_recovery_code().unwrap().display();
        assert!(seen.insert(text.to_string()), "a code repeated");
    }
}

#[test]
fn recovery_lower_case_spaces_and_look_alikes_parse_to_the_same_code() {
    let code = generate_recovery_code().unwrap();
    let key = kek_from_recovery(&code, &[1u8; 16]).unwrap();
    let text = code.display().to_string();
    let lower_with_spaces = text.to_lowercase().replace('-', " ");
    let squeezed = text.replace('-', "");
    for variant in [text.clone(), lower_with_spaces, squeezed, format!("  {text}\n"), text.replace('0', "O").replace('1', "I"), text.replace('1', "l").replace('0', "o")] {
        let parsed = parse_recovery_code(&variant).unwrap();
        assert!(key.same_as(&kek_from_recovery(&parsed, &[1u8; 16]).unwrap()), "{variant}");
    }
    let o = parse_recovery_code("O000-0000-0000-0000-0000-0000-0000-0000").unwrap();
    let zero = parse_recovery_code("0000-0000-0000-0000-0000-0000-0000-0000").unwrap();
    assert!(kek_from_recovery(&o, &[1u8; 16]).unwrap().same_as(&kek_from_recovery(&zero, &[1u8; 16]).unwrap()));
}

#[test]
fn recovery_text_that_is_not_a_code_is_refused() {
    let good = "0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ";
    assert!(parse_recovery_code(good).is_ok());
    for bad in [
        "",
        "0123",
        &good[..good.len() - 1],
        &format!("{good}0"),
        &good.replace('Z', "U"),
        &good.replace('Z', "!"),
        &"0".repeat(500),
        "0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXY\u{e9}",
    ] {
        assert_eq!(parse_recovery_code(bad).unwrap_err(), UnlockError::RecoveryCode, "{bad:?}");
    }
}

#[test]
fn recovery_a_wrong_code_unwraps_nothing_and_the_right_one_opens_the_store() {
    let dir = Scratch::new("store-recovery");
    let (store, path) = new_store(&dir);
    store.set("ssh:a", "hunter2").unwrap();
    let code = generate_recovery_code().unwrap();
    let salt = [5u8; 16];
    store.add_slot(SlotKind::Recovery, id(4), &kek_from_recovery(&code, &salt).unwrap()).unwrap();
    let text = code.display();

    let wrong = generate_recovery_code().unwrap();
    let fresh = EncryptedFileStore::open(&path).unwrap();
    let error = fresh.unlock(SlotKind::Recovery, &kek_from_recovery(&wrong, &salt).unwrap()).unwrap_err();
    assert!(matches!(error, StoreError::Locked));
    let right = parse_recovery_code(&text.to_lowercase()).unwrap();
    fresh.unlock(SlotKind::Recovery, &kek_from_recovery(&right, &salt).unwrap()).unwrap();
    assert_eq!(fresh.get("ssh:a").unwrap().as_deref(), Some("hunter2"));
}

// ---------------------------------------------------------------------------------------------------------------------
// The format as documented, tampering, limits, two writers, a crash and redaction.

const HEADER: usize = 7;
const SLOT: usize = 89;

/// A store file written by hand from the documented format, with the cipher used directly. This is the second
/// implementation that keeps the format honest, and it makes files the public API cannot (4097 entries).
fn hand_built(dek: &[u8; 32], slots: &[(u8, [u8; 16], [u8; 32])], entries: &[(String, String)]) -> Vec<u8> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};

    let seal = |key: &[u8; 32], nonce: [u8; 24], aad: &[u8], plain: &[u8]| {
        XChaCha20Poly1305::new(key.into()).encrypt(&XNonce::from(nonce), Payload { msg: plain, aad }).unwrap()
    };
    let mut file = b"HWSS".to_vec();
    file.extend_from_slice(&1u16.to_be_bytes());
    file.push(u8::try_from(slots.len()).unwrap());
    for (n, (kind, slot_id, kek)) in slots.iter().enumerate() {
        let nonce = [0x40 + u8::try_from(n).unwrap(); 24];
        let mut aad = b"HWSS".to_vec();
        aad.extend_from_slice(&1u16.to_be_bytes());
        aad.push(*kind);
        aad.extend_from_slice(slot_id);
        file.push(*kind);
        file.extend_from_slice(slot_id);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&seal(kek, nonce, &aad, dek));
    }
    let map: serde_json::Map<String, serde_json::Value> =
        entries.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    let body_nonce = [0x77u8; 24];
    let mut aad = b"HWSS".to_vec();
    aad.extend_from_slice(&1u16.to_be_bytes());
    file.extend_from_slice(&body_nonce);
    file.extend_from_slice(&seal(dek, body_nonce, &aad, &serde_json::to_vec(&map).unwrap()));
    file
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn format_a_file_written_by_hand_from_the_documentation_opens() {
    let dir = Scratch::new("store-hand");
    let path = dir.join("secrets.store");
    let bytes = hand_built(
        &[0x11; 32],
        &[(1, id(1), [1; 32]), (3, id(2), [2; 32])],
        &[("ssh:a".into(), "hunter2".into()), ("b".into(), "two".into())],
    );
    write_file(&path, &bytes);
    for (kind, n) in [(SlotKind::Password, 1u8), (SlotKind::Prf, 2)] {
        let store = EncryptedFileStore::open(&path).unwrap();
        store.unlock(kind, &kek(n)).unwrap();
        assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("hunter2"));
        assert_eq!(store.get("b").unwrap().as_deref(), Some("two"));
    }
    // A write by the store keeps the slots and the data key.
    let store = EncryptedFileStore::open(&path).unwrap();
    store.unlock(SlotKind::Prf, &kek(2)).unwrap();
    store.set("c", "three").unwrap();
    let again = EncryptedFileStore::open(&path).unwrap();
    again.unlock(SlotKind::Password, &kek(1)).unwrap();
    assert_eq!(again.get("c").unwrap().as_deref(), Some("three"));
    assert_eq!(again.slots().unwrap(), vec![(SlotKind::Password, id(1)), (SlotKind::Prf, id(2))]);
}

/// A store with two slots (password key 1, passphrase key 2) and three entries, and its bytes.
fn two_slot_store(dir: &Scratch) -> (std::path::PathBuf, Vec<u8>) {
    let (store, path) = new_store(dir);
    store.add_slot(SlotKind::Passphrase, id(2), &kek(2)).unwrap();
    store.set("ssh:a", "hunter2").unwrap();
    store.set("ssh:b", "swordfish").unwrap();
    store.set("ai:key", "sk-test-0001").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

const ORIGINAL: [(&str, &str); 3] = [("ssh:a", "hunter2"), ("ssh:b", "swordfish"), ("ai:key", "sk-test-0001")];

/// What one damaged file does: the class of the first refusal of `open`, or of each of the two unlocks.
#[derive(Debug, PartialEq, Eq, Clone, Copy, PartialOrd, Ord)]
enum Outcome {
    FormatOnOpen,
    LimitOnOpen,
    Locked,
    Unlocked,
}

/// Open the file at `path` and try both keys. An unlock that succeeds must read the original values and nothing else.
fn try_both(path: &std::path::Path) -> [Outcome; 2] {
    let store = match EncryptedFileStore::open(path) {
        Ok(store) => store,
        Err(StoreError::Format(_)) => return [Outcome::FormatOnOpen; 2],
        Err(StoreError::Limit(_)) => return [Outcome::LimitOnOpen; 2],
        Err(other) => panic!("a damaged file gave {other:?}, which is neither a format error nor locked"),
    };
    let mut out = [Outcome::Locked; 2];
    for (n, (kind, key)) in [(SlotKind::Password, 1u8), (SlotKind::Passphrase, 2)].into_iter().enumerate() {
        match store.unlock(kind, &kek(key)) {
            Ok(()) => {
                for (account, value) in ORIGINAL {
                    assert_eq!(store.get(account).unwrap().as_deref(), Some(value), "an unlock gave a changed value");
                }
                store.lock();
                out[n] = Outcome::Unlocked;
            }
            Err(StoreError::Locked) => {}
            Err(StoreError::Format(_)) => out[n] = Outcome::FormatOnOpen,
            Err(other) => panic!("an unlock of a damaged file gave {other:?}"),
        }
    }
    out
}

#[test]
fn tampering_any_region_fails_closed() {
    let dir = Scratch::new("store-tamper");
    let (path, original) = two_slot_store(&dir);
    assert_eq!(try_both(&path), [Outcome::Unlocked; 2], "the unflipped file unlocks");
    let slots_end = HEADER + 2 * SLOT;
    let mut classes = std::collections::BTreeMap::<String, usize>::new();
    // A flip in one slot leaves the other slot's key working, unless the flip makes the slot table unreadable (an unknown kind
    // byte), which refuses the whole file at open. Either way the flipped slot itself stays closed.
    let other_slot = |position_in_slot: usize, outcome: Outcome| {
        if position_in_slot == 0 {
            assert!(matches!(outcome, Outcome::Unlocked | Outcome::FormatOnOpen), "{outcome:?}");
        } else {
            assert_eq!(outcome, Outcome::Unlocked, "a flip at {position_in_slot} of a slot closed the other slot");
        }
    };
    for mask in [0x01u8, 0x80, 0xff] {
        for offset in 0..original.len() {
            let mut bytes = original.clone();
            bytes[offset] ^= mask;
            write_file(&path, &bytes);
            let outcome = try_both(&path);
            // The slot a flip is in is the only way in that it can close; a flip in the header or the body closes both.
            let region = if offset < HEADER {
                "header"
            } else if offset < slots_end {
                if (offset - HEADER) / SLOT == 0 { "slot 0" } else { "slot 1" }
            } else if offset < slots_end + 24 {
                "body nonce"
            } else {
                "body ciphertext and tag"
            };
            match region {
                "slot 0" => {
                    assert_ne!(outcome[0], Outcome::Unlocked, "offset {offset} mask {mask:#x}: the flipped slot still opens");
                    other_slot(offset - HEADER, outcome[1]);
                }
                "slot 1" => {
                    assert_ne!(outcome[1], Outcome::Unlocked, "offset {offset} mask {mask:#x}: the flipped slot still opens");
                    other_slot(offset - HEADER - SLOT, outcome[0]);
                }
                _ => {
                    assert!(
                        outcome.iter().all(|o| *o != Outcome::Unlocked),
                        "offset {offset} mask {mask:#x} in {region}: a changed file unlocks ({outcome:?})"
                    );
                }
            }
            for o in outcome {
                *classes.entry(format!("{region}: {o:?}")).or_default() += 1;
            }
        }
    }
    let total: usize = classes.values().sum();
    eprintln!("Tamper classes ({} flips, {total} outcomes): {classes:?}", original.len() * 3);
    write_file(&path, &original);
    assert_eq!(try_both(&path), [Outcome::Unlocked; 2]);
}

#[test]
fn tampering_under_an_unlocked_store_fails_closed_and_is_not_overwritten() {
    let dir = Scratch::new("store-tamper-open");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    write_file(&path, &bytes);
    assert_eq!(store.get("a").unwrap_err().to_string(), LOCKED);
    assert_eq!(store.set("a", "two").unwrap_err().to_string(), LOCKED);
    assert_eq!(store.delete("a").unwrap_err().to_string(), LOCKED);
    assert!(matches!(store.add_slot(SlotKind::Prf, id(5), &kek(5)).unwrap_err(), StoreError::Locked));
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "a failed write must leave the damaged file as it was");
}

#[test]
fn truncation_and_appending_fail_closed() {
    let dir = Scratch::new("store-truncate");
    let (path, original) = two_slot_store(&dir);
    let mut classes = std::collections::BTreeMap::<String, usize>::new();
    for length in 0..original.len() {
        write_file(&path, &original[..length]);
        let outcome = try_both(&path);
        assert!(outcome.iter().all(|o| *o != Outcome::Unlocked), "a prefix of {length} bytes unlocks");
        *classes.entry(format!("{:?}", outcome[0])).or_default() += 1;
    }
    let mut longer = original.clone();
    longer.push(0);
    write_file(&path, &longer);
    let outcome = try_both(&path);
    assert!(outcome.iter().all(|o| *o != Outcome::Unlocked), "an appended byte unlocks");
    eprintln!("Truncation classes ({} prefixes): {classes:?}; appended byte: {outcome:?}", original.len());
    write_file(&path, &original);
    assert_eq!(try_both(&path), [Outcome::Unlocked; 2]);
}

#[test]
fn swapping_two_slots_fails_closed() {
    let dir = Scratch::new("store-swap");
    let (path, original) = two_slot_store(&dir);
    let a = HEADER;
    let b = HEADER + SLOT;
    // Parts of slot 0 and slot 1: kind and id (0..17), the nonce (17..41), the wrapped key and tag (41..89).
    for (name, from, to) in [("kind and id", 0, 17), ("nonce", 17, 41), ("wrapped key", 41, 89), ("nonce and wrapped key", 17, 89)] {
        let mut bytes = original.clone();
        let (first, second) = (bytes[a + from..a + to].to_vec(), bytes[b + from..b + to].to_vec());
        bytes[a + from..a + to].copy_from_slice(&second);
        bytes[b + from..b + to].copy_from_slice(&first);
        write_file(&path, &bytes);
        let outcome = try_both(&path);
        assert!(outcome.iter().all(|o| *o != Outcome::Unlocked), "swapping the {name} of two slots still unlocks: {outcome:?}");
    }
    // The two slots exchanged as whole records stay what they were: a slot is bound to its own kind and id, not to a place.
    let mut bytes = original.clone();
    let (first, second) = (bytes[a..a + SLOT].to_vec(), bytes[b..b + SLOT].to_vec());
    bytes[a..a + SLOT].copy_from_slice(&second);
    bytes[b..b + SLOT].copy_from_slice(&first);
    write_file(&path, &bytes);
    assert_eq!(try_both(&path), [Outcome::Unlocked; 2]);
}

#[test]
fn a_wrong_key_is_the_same_failure_as_a_damaged_slot() {
    let dir = Scratch::new("store-indistinct");
    let (path, original) = two_slot_store(&dir);
    let wrong = EncryptedFileStore::open(&path).unwrap().unlock(SlotKind::Password, &kek(99)).unwrap_err().to_string();
    let no_such_kind = EncryptedFileStore::open(&path).unwrap().unlock(SlotKind::Recovery, &kek(1)).unwrap_err().to_string();
    let mut bytes = original;
    bytes[HEADER + 60] ^= 0x01;
    write_file(&path, &bytes);
    let damaged = EncryptedFileStore::open(&path).unwrap().unlock(SlotKind::Password, &kek(1)).unwrap_err().to_string();
    assert_eq!(wrong, LOCKED);
    assert_eq!(no_such_kind, LOCKED);
    assert_eq!(damaged, LOCKED);
}

#[test]
fn a_wrong_key_changes_nothing() {
    let dir = Scratch::new("store-nothing");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    let before = std::fs::read(&path).unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let dir_entries = |d: &Scratch| {
        let mut names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        names
    };
    let listing = dir_entries(&dir);

    let opened = EncryptedFileStore::open(&path).unwrap();
    assert_eq!(opened.unlock(SlotKind::Password, &kek(2)).unwrap_err().to_string(), LOCKED);
    assert_eq!(opened.set("a", "two").unwrap_err().to_string(), LOCKED);
    assert_eq!(opened.delete("a").unwrap_err().to_string(), LOCKED);
    assert_eq!(opened.get("a").unwrap_err().to_string(), LOCKED);
    assert!(matches!(opened.add_slot(SlotKind::Prf, id(3), &kek(3)).unwrap_err(), StoreError::Locked));
    store.lock();
    assert!(store.set("a", "three").is_err());

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
    assert_eq!(dir_entries(&dir), listing, "no file appeared or went");
}

#[test]
fn limits_a_file_over_four_mib_is_refused() {
    let dir = Scratch::new("store-limit-file");
    let (path, original) = two_slot_store(&dir);
    let mut huge = original.clone();
    huge.resize(4 * 1024 * 1024 + 1, 0);
    write_file(&path, &huge);
    assert!(matches!(EncryptedFileStore::open(&path), Err(StoreError::Limit(_))));
    // Exactly at the cap the size is no reason to refuse (the padding is a damaged body, which unlocks to nothing).
    let mut at_cap = original.clone();
    at_cap.resize(4 * 1024 * 1024, 0);
    write_file(&path, &at_cap);
    let store = EncryptedFileStore::open(&path).unwrap();
    assert!(matches!(store.unlock(SlotKind::Password, &kek(1)).unwrap_err(), StoreError::Locked));
    write_file(&path, &vec![0u8; 5 * 1024 * 1024]);
    assert!(EncryptedFileStore::open(&path).is_err());
}

#[test]
fn limits_4096_entries_are_the_most_and_4097_are_refused() {
    let dir = Scratch::new("store-limit-entries");
    let path = dir.join("secrets.store");
    let entry = |n: usize| (format!("account:{n:05}"), "v".to_string());
    let at_cap: Vec<_> = (0..4096).map(entry).collect();
    write_file(&path, &hand_built(&[0x11; 32], &[(1, id(1), [1; 32])], &at_cap));
    let store = EncryptedFileStore::open(&path).unwrap();
    store.unlock(SlotKind::Password, &kek(1)).unwrap();
    assert_eq!(store.get("account:04095").unwrap().as_deref(), Some("v"));
    // A replacement is not a new entry; a new account is refused and the file stays as it was.
    store.set("account:00000", "w").unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(store.set("one-more", "x").unwrap_err().to_string().as_str(), s if s.contains("4096")));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    store.delete("account:00001").unwrap();
    store.set("one-more", "x").unwrap();

    let too_many: Vec<_> = (0..4097).map(entry).collect();
    write_file(&path, &hand_built(&[0x11; 32], &[(1, id(1), [1; 32])], &too_many));
    let store = EncryptedFileStore::open(&path).unwrap();
    assert!(matches!(store.unlock(SlotKind::Password, &kek(1)).unwrap_err(), StoreError::Locked));
}

#[test]
fn limits_a_write_that_would_pass_four_mib_is_refused_and_changes_nothing() {
    let dir = Scratch::new("store-limit-write");
    let path = dir.join("secrets.store");
    let big = "x".repeat(4096);
    // 1018 entries of 4096 bytes are 4 189 208 bytes in the file, so the second further entry passes the cap of 4 194 304.
    let entries: Vec<_> = (0..1018).map(|n| (format!("account:{n:05}"), big.clone())).collect();
    write_file(&path, &hand_built(&[0x11; 32], &[(1, id(1), [1; 32])], &entries));
    let store = EncryptedFileStore::open(&path).unwrap();
    store.unlock(SlotKind::Password, &kek(1)).unwrap();
    let mut refused = false;
    for n in 1018..1100 {
        let before = std::fs::read(&path).unwrap();
        assert!(before.len() <= 4 * 1024 * 1024);
        match store.set(&format!("account:{n:05}"), &big) {
            Ok(()) => {}
            Err(e) => {
                assert!(e.to_string().contains("4 MiB"), "{e}");
                assert_eq!(std::fs::read(&path).unwrap(), before);
                refused = true;
                break;
            }
        }
    }
    assert!(refused, "the 4 MiB cap was never reached");
    assert!(store.get("account:00000").unwrap().is_some());
}

#[test]
fn limits_a_value_over_the_trait_limit_is_refused() {
    let dir = Scratch::new("store-limit-value");
    let (store, path) = new_store(&dir);
    let before = std::fs::read(&path).unwrap();
    assert!(store.set("a", &"x".repeat(4097)).is_err());
    assert!(store.set(&"a".repeat(4097), "x").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

fn unlocked(path: &std::path::Path) -> EncryptedFileStore {
    let store = EncryptedFileStore::open(path).unwrap();
    store.unlock(SlotKind::Password, &kek(1)).unwrap();
    store
}

#[test]
fn two_writers_threads() {
    let dir = Scratch::new("store-threads");
    let (_first, path) = new_store(&dir);
    let (a, b) = (unlocked(&path), unlocked(&path));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 0..100 {
                a.set(&format!("a:{n:03}"), &format!("from a {n}")).unwrap();
            }
        });
        scope.spawn(|| {
            for n in 0..100 {
                b.set(&format!("b:{n:03}"), &format!("from b {n}")).unwrap();
            }
        });
    });
    let reader = unlocked(&path);
    let mut found = 0;
    for n in 0..100 {
        assert_eq!(reader.get(&format!("a:{n:03}")).unwrap().as_deref(), Some(format!("from a {n}").as_str()));
        assert_eq!(reader.get(&format!("b:{n:03}")).unwrap().as_deref(), Some(format!("from b {n}").as_str()));
        found += 2;
    }
    eprintln!("Two writers (threads): expected 200, found {found}");
    assert_eq!(found, 200);
}

/// The role of a child process of `two_writers_processes`: with the environment variable set it writes its 100 entries
/// and leaves a marker file; without it the test is a no-op.
#[test]
fn two_writers_child_role() {
    let (Ok(role), Ok(path)) = (std::env::var("HW_STORE_TEST_ROLE"), std::env::var("HW_STORE_TEST_PATH")) else {
        return;
    };
    let store = unlocked(Path::new(&path));
    for n in 0..100 {
        store.set(&format!("{role}:{n:03}"), &format!("from {role} {n}")).unwrap();
    }
    write_file(&Path::new(&path).with_file_name(format!("done-{role}")), b"done");
}

#[test]
fn two_writers_processes() {
    let dir = Scratch::new("store-processes");
    let (_first, path) = new_store(&dir);
    let exe = std::env::current_exe().unwrap();
    let mut children: Vec<_> = ["p", "q"]
        .into_iter()
        .map(|role| {
            std::process::Command::new(&exe)
                .args(["two_writers_child_role", "--exact", "--test-threads=1", "--nocapture"])
                .env("HW_STORE_TEST_ROLE", role)
                .env("HW_STORE_TEST_PATH", &path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success(), "a writer process failed");
    }
    for role in ["p", "q"] {
        assert!(dir.join(&format!("done-{role}")).exists(), "the writer {role} never ran its test");
    }
    let reader = unlocked(&path);
    let mut found = 0;
    for n in 0..100 {
        for role in ["p", "q"] {
            assert_eq!(reader.get(&format!("{role}:{n:03}")).unwrap().as_deref(), Some(format!("from {role} {n}").as_str()), "{role} {n}");
            found += 1;
        }
    }
    eprintln!("Two writers (processes): expected 200, found {found}");
    assert_eq!(found, 200);
}

#[test]
fn the_same_key_written_by_both_ends_with_one_of_the_values() {
    let dir = Scratch::new("store-same-key");
    let (_first, path) = new_store(&dir);
    let (a, b) = (unlocked(&path), unlocked(&path));
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for n in 0..25 {
                a.set("shared", "value from a").unwrap();
                a.set(&format!("a:{n}"), "a").unwrap();
            }
        });
        scope.spawn(|| {
            for n in 0..25 {
                b.set("shared", "value from b").unwrap();
                b.set(&format!("b:{n}"), "b").unwrap();
            }
        });
    });
    let reader = unlocked(&path);
    let shared = reader.get("shared").unwrap().unwrap();
    assert!(shared == "value from a" || shared == "value from b", "{shared}");
    for n in 0..25 {
        assert_eq!(reader.get(&format!("a:{n}")).unwrap().as_deref(), Some("a"));
        assert_eq!(reader.get(&format!("b:{n}")).unwrap().as_deref(), Some("b"));
    }
}

#[test]
fn a_slot_added_through_one_object_survives_the_writes_of_another() {
    let dir = Scratch::new("store-slot-concurrent");
    let (_first, path) = new_store(&dir);
    let (a, b) = (unlocked(&path), unlocked(&path));
    a.add_slot(SlotKind::Passphrase, id(2), &kek(2)).unwrap();
    b.set("after", "the slot").unwrap();
    let fresh = EncryptedFileStore::open(&path).unwrap();
    fresh.unlock(SlotKind::Passphrase, &kek(2)).unwrap();
    assert_eq!(fresh.get("after").unwrap().as_deref(), Some("the slot"));
}

#[test]
fn a_crash_between_write_and_rename_leaves_the_old_store() {
    let dir = Scratch::new("store-crash");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    drop(store);
    let before = std::fs::read(&path).unwrap();
    // What a crash leaves: temporary files of the store, one of them half written; and files that are not the store's.
    let stray = dir.write_file("secrets.store.tmp-0123456789abcdef", &before[..before.len() / 2], 0o600);
    let stray_empty = dir.write_file("secrets.store.tmp-ffffffffffffffff", b"", 0o600);
    let other = dir.write_file("other.store.tmp-0123456789abcdef", b"not ours", 0o600);
    let similar = dir.write_file("secrets.store.bak", b"not ours either", 0o600);

    let reopened = EncryptedFileStore::open(&path).unwrap();
    assert!(!stray.exists() && !stray_empty.exists(), "a stray temporary file survived the open");
    assert!(other.exists() && similar.exists(), "a file that is not the store's was removed");
    assert_eq!(std::fs::read(&path).unwrap(), before, "the open changed the store");
    reopened.unlock(SlotKind::Password, &kek(1)).unwrap();
    assert_eq!(reopened.get("a").unwrap().as_deref(), Some("one"));
    // A completed write leaves no temporary file behind.
    reopened.set("b", "two").unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!names.iter().any(|n| n.starts_with("secrets.store.tmp-")), "{names:?}");
}

#[test]
fn a_link_in_place_of_the_store_is_refused_and_a_missing_file_is_an_error() {
    let dir = Scratch::new("store-link");
    let (_store, path) = new_store(&dir);
    let link = dir.join("link.store");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(EncryptedFileStore::open(&link).is_err());
    assert!(EncryptedFileStore::create(&link, SlotKind::Password, id(1), &kek(1)).is_err());
    assert!(EncryptedFileStore::open(&dir.join("missing.store")).is_err());
    write_file(&dir.join("junk.store"), b"this is not a secret store at all, only text");
    assert!(matches!(EncryptedFileStore::open(&dir.join("junk.store")), Err(StoreError::Format(_))));
}

#[test]
fn deleting_a_missing_entry_does_not_rewrite_the_file() {
    let dir = Scratch::new("store-delete-missing");
    let (store, path) = new_store(&dir);
    store.set("a", "one").unwrap();
    let before = std::fs::read(&path).unwrap();
    store.delete("missing").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn nonce_is_fresh_per_write() {
    let dir = Scratch::new("store-nonce");
    let (store, path) = new_store(&dir);
    let nonce_of = |bytes: &[u8]| {
        let at = HEADER + usize::from(bytes[6]) * SLOT;
        bytes[at..at + 24].to_vec()
    };
    let mut seen = std::collections::HashSet::new();
    let mut ciphertexts = std::collections::HashSet::new();
    seen.insert(nonce_of(&std::fs::read(&path).unwrap()));
    for _ in 0..30 {
        // The same value written again must not repeat a nonce or a ciphertext.
        store.set("a", "same value").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(seen.insert(nonce_of(&bytes)), "a body nonce was used twice");
        assert!(ciphertexts.insert(bytes[HEADER + SLOT + 24..].to_vec()), "a ciphertext repeated");
    }
    // A slot is wrapped under a fresh nonce each time too.
    store.add_slot(SlotKind::Prf, id(2), &kek(2)).unwrap();
    let first = std::fs::read(&path).unwrap()[HEADER + SLOT + 17..HEADER + SLOT + 41].to_vec();
    store.replace_slot(SlotKind::Prf, id(2), &kek(2)).unwrap();
    let second = std::fs::read(&path).unwrap()[HEADER + SLOT + 17..HEADER + SLOT + 41].to_vec();
    assert_ne!(first, second);
}

#[test]
fn debug_never_prints_key_bytes() {
    use holzwave_server::worker::secretstore::Dek;

    let kek = Kek::from_bytes(&[0xAB; 32]);
    let dek = Dek::from_bytes(&[0xCD; 32]);
    let text = "0123-4567-89AB-CDEF-GHJK-MNPQ-RSTV-WXYZ";
    let code = parse_recovery_code(text).unwrap();
    let dir = Scratch::new("store-debug");
    let (store, _) = new_store(&dir);
    store.set("ssh:account-name", "secret-value-123").unwrap();

    let shown = [
        format!("{kek:?}"),
        format!("{kek:#?}"),
        format!("{dek:?}"),
        format!("{dek:#?}"),
        format!("{code:?}"),
        format!("{code:#?}"),
        format!("{store:?}"),
        format!("{store:#?}"),
        format!("{:?}", Some(&kek)),
        format!("{:#?}", vec![&dek]),
        format!("{:?}", Kek::from_bytes(&[0xAB; 32])),
        format!("{:?}", Ok::<_, StoreError>(&dek)),
    ];
    for s in &shown {
        for forbidden in ["171", "205", "0xab", "0xAB", "ab, ab", "AB, AB", "abab", "ABAB", "cdcd", "0123", "WXYZ", "secret-value", "account-name"] {
            assert!(!s.contains(forbidden), "{s:?} contains {forbidden:?}");
        }
    }
    assert_eq!(format!("{kek:?}"), "Kek(..)");
    assert_eq!(format!("{dek:?}"), "Dek(..)");
    assert_eq!(format!("{code:?}"), "RecoveryCode(..)");
    // The text of a refusal holds no input either.
    for error in [parse_recovery_code("abc secret-ish text").unwrap_err().to_string(), store.set("", "secret-value-123").unwrap_err().to_string()] {
        assert!(!error.contains("secret-ish") && !error.contains("secret-value"), "{error}");
    }
}

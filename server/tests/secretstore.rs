//! The encrypted secret store: the format, the locked state, the slots, tampering, two writers and redaction.

#[path = "support/scratch.rs"]
mod scratch;

use std::os::unix::fs::PermissionsExt;

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

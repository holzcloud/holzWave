//! Sealed values: what hides the plaintext and what binds a value to its place.

use holzwave_server::keyfile::ServerKey;
use holzwave_server::sealed::Sealer;

fn key(byte: u8) -> ServerKey {
    ServerKey::from_bytes([byte; 32]).expect("a key that is not all zero")
}

#[test]
fn a_secret_is_sealed_and_opens_only_for_its_context() {
    let sealer = Sealer::new(&key(7));
    let secret = b"JBSWY3DPEHPK3PXP";
    let blob = sealer.seal("totp:1", secret).expect("seal");
    assert!(!blob.windows(secret.len()).any(|w| w == secret), "the blob must not contain the plaintext");
    assert_eq!(&**sealer.open("totp:1", &blob).expect("open"), secret);
    let other = sealer.open("totp:2", &blob).expect_err("another context");
    assert_eq!(other.to_string(), "cannot open the sealed value");
    assert_eq!(format!("{sealer:?}"), "Sealer(..)");
}

use holzwave_server::sealed::{MAX_VALUE_BYTES, SealedError};

const SECRET: &[u8] = b"JBSWY3DPEHPK3PXP";

fn sealed_secret() -> (Sealer, Vec<u8>) {
    let sealer = Sealer::new(&key(7));
    let blob = sealer.seal("totp:1", SECRET).expect("seal");
    (sealer, blob)
}

#[test]
fn the_blob_has_a_format_byte_a_nonce_and_a_tag() {
    let (_sealer, blob) = sealed_secret();
    assert_eq!(blob[0], 1, "the format byte");
    assert_eq!(blob.len(), 1 + 24 + SECRET.len() + 16);
}

#[test]
fn flipping_any_byte_never_opens() {
    let (sealer, blob) = sealed_secret();
    let (mut tried, mut opened) = (0usize, 0usize);
    for i in 0..blob.len() {
        for mask in [0x01u8, 0x80, 0xFF] {
            let mut changed = blob.clone();
            changed[i] ^= mask;
            tried += 1;
            if sealer.open("totp:1", &changed).is_ok() {
                opened += 1;
            }
        }
    }
    println!("Tamper: {tried} changes tried, {opened} opened");
    assert_eq!(opened, 0);
    assert_eq!(tried, blob.len() * 3);
}

#[test]
fn every_truncation_and_extension_fails() {
    let (sealer, blob) = sealed_secret();
    for len in 0..blob.len() {
        assert_eq!(sealer.open("totp:1", &blob[..len]).expect_err("truncated"), SealedError::Open, "length {len}");
    }
    let mut longer = blob.clone();
    longer.push(0);
    assert!(sealer.open("totp:1", &longer).is_err());
    assert!(sealer.open("totp:1", &[]).is_err());
}

#[test]
fn another_server_key_does_not_open() {
    let (_sealer, blob) = sealed_secret();
    assert!(Sealer::new(&key(8)).open("totp:1", &blob).is_err());
}

#[test]
fn two_seals_of_one_plaintext_differ_and_both_open() {
    let sealer = Sealer::new(&key(7));
    let one = sealer.seal("totp:1", SECRET).expect("seal");
    let two = sealer.seal("totp:1", SECRET).expect("seal");
    assert_ne!(one, two);
    assert_ne!(one[1..25], two[1..25], "the nonces differ");
    assert_eq!(&**sealer.open("totp:1", &one).expect("open"), SECRET);
    assert_eq!(&**sealer.open("totp:1", &two).expect("open"), SECRET);
}

#[test]
fn a_value_moved_to_another_context_does_not_open() {
    let (sealer, blob) = sealed_secret();
    for other in ["totp:2", "totp:10", "totp:", "totp:1 ", "TOTP:1", "acme:1", ""] {
        assert!(sealer.open(other, &blob).is_err(), "context {other:?}");
    }
    assert!(sealer.open("totp:1", &blob).is_ok());
}

#[test]
fn the_size_cap_is_sixty_four_kib() {
    let sealer = Sealer::new(&key(7));
    let full = vec![0xA5u8; MAX_VALUE_BYTES];
    let blob = sealer.seal("acme:account", &full).expect("64 KiB seals");
    assert_eq!(&**sealer.open("acme:account", &blob).expect("open"), &full[..]);
    let over = vec![0u8; MAX_VALUE_BYTES + 1];
    assert_eq!(sealer.seal("acme:account", &over).expect_err("too large"), SealedError::TooLarge);
    // A blob longer than the cap allows is not opened at all.
    assert!(sealer.open("acme:account", &vec![1u8; MAX_VALUE_BYTES + 100]).is_err());
}

#[test]
fn an_empty_value_seals_and_opens() {
    let sealer = Sealer::new(&key(7));
    let blob = sealer.seal("x", b"").expect("seal");
    assert!(sealer.open("x", &blob).expect("open").is_empty());
}

#[test]
fn every_failure_to_open_is_the_same_error_and_nothing_prints_the_key() {
    let (sealer, blob) = sealed_secret();
    let mut flipped = blob.clone();
    flipped[30] ^= 1;
    let errors = [
        sealer.open("totp:1", &flipped).expect_err("flipped"),
        sealer.open("totp:2", &blob).expect_err("context"),
        sealer.open("totp:1", &blob[..10]).expect_err("short"),
        Sealer::new(&key(9)).open("totp:1", &blob).expect_err("key"),
    ];
    assert!(errors.iter().all(|e| *e == SealedError::Open));
    assert!(errors.iter().all(|e| e.to_string() == "cannot open the sealed value"));
    // Neither the sealer nor the server key shows a byte of itself.
    let shown = format!("{sealer:?} {:?}", key(7));
    assert!(!shown.contains("107") && !shown.contains("0x7") && !shown.contains("[7"), "{shown}");
    assert_eq!(format!("{sealer:?}"), "Sealer(..)");
}

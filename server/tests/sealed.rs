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

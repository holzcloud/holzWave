//! The rules that decide whether a request may change anything: the Host, the Origin, the content type and the anti-forgery
//! token, as pure functions over the headers and the public origin.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::keyfile::KEY_BYTES;
use crate::store::TokenHash;

/// The info string of the token derivation.
const TOKEN_INFO: &[u8] = b"holzwave-server/v1/csrf-token";

/// The anti-forgery token of the session whose stored id is `session_id`: HKDF-SHA256 keyed by the `csrf` purpose key, with
/// the stored id (the hash of the cookie value) as the salt, as 43 base64url characters. Nothing is stored. The token changes
/// with every session, and a person who holds only the cookie value cannot make it, because the key is not theirs.
pub fn token_for(key: &[u8; KEY_BYTES], session_id: &TokenHash) -> String {
    let hkdf = Hkdf::<Sha256>::new(Some(&session_id[..]), &key[..]);
    let mut out = [0u8; 32];
    hkdf.expand(TOKEN_INFO, &mut out).expect("32 bytes is a valid HKDF-SHA256 output length");
    URL_SAFE_NO_PAD.encode(out)
}

/// Whether `given` is `expected`, compared in constant time. A different length is a mismatch at once: the length of the
/// token is public.
pub fn token_ok(given: &[u8], expected: &str) -> bool {
    given.ct_eq(expected.as_bytes()).into()
}

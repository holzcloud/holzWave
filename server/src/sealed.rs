//! Authenticated encryption of small secrets at rest under the server key (TOTP seeds, ACME keys).
//!
//! A [`Sealer`] holds the `sealed` purpose key of the server key. A sealed value is
//!
//! ```text
//! format byte (1) | nonce (24) | XChaCha20-Poly1305 ciphertext and tag
//! ```
//!
//! The associated data is the format byte, a fixed product label and the caller's context string (for example
//! `totp:<account id>`), so a value copied to another row, another account or another purpose does not open. A value that
//! does not open for any reason (a changed byte, a cut-off blob, another key, another context) fails with the one error
//! [`SealedError::Open`], so a caller cannot tell the reasons apart and nothing about the value leaks.

use std::fmt;

use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use zeroize::Zeroizing;

use crate::keyfile::{KEY_BYTES, Purpose, ServerKey};

/// The format byte of a sealed value.
const FORMAT: u8 = 1;

/// The length of the nonce in bytes.
const NONCE_BYTES: usize = 24;

/// The length of the authentication tag in bytes.
const TAG_BYTES: usize = 16;

/// The start of the associated data of every sealed value.
const AAD_LABEL: &[u8] = b"holzwave-server/sealed/v1\0";

/// The largest plaintext that is sealed, in bytes.
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// What the `Debug` text of a sealer prints.
const DEBUG_MARKER: &str = "Sealer(..)";

/// Why a value was not sealed or opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealedError {
    /// The plaintext is larger than [`MAX_VALUE_BYTES`].
    TooLarge,
    /// The operating system gave no random bytes for the nonce.
    NoRandom,
    /// The value does not open. One error for every reason.
    Open,
}

impl fmt::Display for SealedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SealedError::TooLarge => f.write_str("the value to seal is larger than 64 KiB"),
            SealedError::NoRandom => f.write_str("the operating system gave no random bytes to seal the value"),
            SealedError::Open => f.write_str("cannot open the sealed value"),
        }
    }
}

impl std::error::Error for SealedError {}

/// Seals and opens small values under the `sealed` purpose key. It has no `Clone`, its `Debug` text is a fixed marker and
/// its key is wiped when it is dropped.
pub struct Sealer {
    key: Zeroizing<[u8; KEY_BYTES]>,
}

impl fmt::Debug for Sealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DEBUG_MARKER)
    }
}

impl Sealer {
    /// A sealer for the `sealed` purpose key of `key`.
    pub fn new(key: &ServerKey) -> Sealer {
        Sealer { key: key.derive(Purpose::Sealed) }
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&(*self.key).into())
    }

    fn associated_data(context: &str) -> Zeroizing<Vec<u8>> {
        let mut aad = Vec::with_capacity(1 + AAD_LABEL.len() + context.len());
        aad.push(FORMAT);
        aad.extend_from_slice(AAD_LABEL);
        aad.extend_from_slice(context.as_bytes());
        Zeroizing::new(aad)
    }

    /// Seals `plaintext` for `context`. The result is not secret-free to log, but it holds no plaintext.
    pub fn seal(&self, context: &str, plaintext: &[u8]) -> Result<Vec<u8>, SealedError> {
        if plaintext.len() > MAX_VALUE_BYTES {
            return Err(SealedError::TooLarge);
        }
        let mut nonce = [0u8; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|_| SealedError::NoRandom)?;
        let mut out = Vec::with_capacity(1 + NONCE_BYTES + plaintext.len() + TAG_BYTES);
        out.push(FORMAT);
        out.extend_from_slice(&nonce);
        let mut body = Zeroizing::new(Vec::with_capacity(plaintext.len() + TAG_BYTES));
        body.extend_from_slice(plaintext);
        let aad = Sealer::associated_data(context);
        self.cipher().encrypt_in_place(&nonce.into(), &aad, &mut *body).map_err(|_| SealedError::Open)?;
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Opens a value that was sealed for `context`.
    pub fn open(&self, context: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, SealedError> {
        if sealed.len() < 1 + NONCE_BYTES + TAG_BYTES || sealed.len() > 1 + NONCE_BYTES + MAX_VALUE_BYTES + TAG_BYTES {
            return Err(SealedError::Open);
        }
        if sealed[0] != FORMAT {
            return Err(SealedError::Open);
        }
        let mut nonce = [0u8; NONCE_BYTES];
        nonce.copy_from_slice(&sealed[1..1 + NONCE_BYTES]);
        let mut body = Zeroizing::new(sealed[1 + NONCE_BYTES..].to_vec());
        let aad = Sealer::associated_data(context);
        self.cipher().decrypt_in_place(&nonce.into(), &aad, &mut *body).map_err(|_| SealedError::Open)?;
        Ok(body)
    }
}

//! The keys that open the encrypted secret store.
//!
//! A person's secrets are encrypted with a random data key, and the data key is wrapped once for every way the person can
//! unlock: a password, a passphrase, a passkey that returns a pseudo-random function (PRF) result, a recovery code. This
//! module holds the derivations that turn what the person has into a 32-byte key-encryption key ([`Kek`]), the bounded
//! Argon2id parameters ([`Kdf`]) and the recovery code. The slots themselves are in `worker::secretstore`.
//!
//! The derivations return a key only as a value, for the caller to send to the worker over its socket. No function here
//! writes a key to a file, a log, an error text, an environment variable or a command line. [`Kek`] has no `Clone`, a `Debug`
//! text that prints a fixed marker, and wipes its bytes when it is dropped.

use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// The length of a key in bytes.
pub const KEY_BYTES: usize = 32;

/// The length of the salt of the Argon2id derivations in bytes.
pub const SALT_BYTES: usize = 16;

/// What the `Debug` text of a key prints.
const KEK_DEBUG: &str = "Kek(..)";

/// Why a derivation was refused. The sentences hold no input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockError {
    /// A parameter of the Argon2id derivation is outside its bounds.
    KdfOutOfBounds,
    /// A salt or an output has a length the derivation does not take.
    BadLength,
    /// The derivation itself failed.
    Derive,
    /// The operating system gave no random bytes.
    Random,
    /// The text is not a recovery code.
    RecoveryCode,
}

impl fmt::Display for UnlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UnlockError::KdfOutOfBounds => "the key derivation parameters are out of range",
            UnlockError::BadLength => "a salt or an output of the key derivation has a wrong length",
            UnlockError::Derive => "the key derivation failed",
            UnlockError::Random => "the operating system gave no random bytes",
            UnlockError::RecoveryCode => "that is not a recovery code",
        })
    }
}

impl std::error::Error for UnlockError {}

/// `N` random bytes from the operating system.
pub fn random<const N: usize>() -> Result<[u8; N], UnlockError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| UnlockError::Random)?;
    Ok(bytes)
}

/// The Argon2id parameters of one derivation. A value outside the bounds cannot be made, so a stored or received value
/// cannot make the server allocate gigabytes or run for minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kdf {
    m_kib: u32,
    t: u32,
    p: u32,
}

impl Kdf {
    /// The least memory in KiB: 19 MiB, the lowest setting the OWASP guidance names for Argon2id.
    pub const MIN_M_KIB: u32 = 19 * 1024;
    /// The most memory in KiB.
    pub const MAX_M_KIB: u32 = 256 * 1024;
    /// The fewest passes.
    pub const MIN_T: u32 = 2;
    /// The most passes.
    pub const MAX_T: u32 = 10;
    /// The fewest lanes.
    pub const MIN_P: u32 = 1;
    /// The most lanes.
    pub const MAX_P: u32 = 4;

    /// A parameter set, or [`UnlockError::KdfOutOfBounds`].
    pub fn new(m_kib: u32, t: u32, p: u32) -> Result<Kdf, UnlockError> {
        if !(Self::MIN_M_KIB..=Self::MAX_M_KIB).contains(&m_kib)
            || !(Self::MIN_T..=Self::MAX_T).contains(&t)
            || !(Self::MIN_P..=Self::MAX_P).contains(&p)
        {
            return Err(UnlockError::KdfOutOfBounds);
        }
        Ok(Kdf { m_kib, t, p })
    }

    /// The cheapest parameters the bounds allow. For tests that need many derivations; a real derivation uses the default.
    pub fn lower_bound() -> Kdf {
        Kdf { m_kib: Self::MIN_M_KIB, t: Self::MIN_T, p: Self::MIN_P }
    }

    /// The memory in KiB.
    pub fn m_kib(&self) -> u32 {
        self.m_kib
    }

    /// The number of passes.
    pub fn t(&self) -> u32 {
        self.t
    }

    /// The number of lanes.
    pub fn p(&self) -> u32 {
        self.p
    }
}

impl Default for Kdf {
    /// 64 MiB, three passes, one lane.
    fn default() -> Self {
        Kdf { m_kib: 64 * 1024, t: 3, p: 1 }
    }
}

/// Argon2id (version 1.3) over `secret` and `salt` into `out`. The shared primitive of the passphrase key and of the password
/// module's 64-byte split. `out` is 16 to 64 bytes.
pub fn argon2id_into(secret: &[u8], salt: &[u8; SALT_BYTES], kdf: &Kdf, out: &mut [u8]) -> Result<(), UnlockError> {
    if !(16..=64).contains(&out.len()) {
        return Err(UnlockError::BadLength);
    }
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(out.len())).map_err(|_| UnlockError::KdfOutOfBounds)?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(secret, salt, out)
        .map_err(|_| UnlockError::Derive)
}

/// A key-encryption key: it wraps the data key in one slot. Wiped when dropped, never printed, not `Clone`.
pub struct Kek(Zeroizing<[u8; KEY_BYTES]>);

impl Kek {
    /// A key from 32 bytes. The caller wipes its own copy.
    pub fn from_bytes(bytes: &[u8; KEY_BYTES]) -> Kek {
        Kek(Zeroizing::new(*bytes))
    }

    /// The bytes, for the caller to send to the worker over its socket. Nothing else should read them.
    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        &self.0
    }

    /// Whether two keys are equal, compared in constant time.
    pub fn same_as(&self, other: &Kek) -> bool {
        self.0.ct_eq(&*other.0).into()
    }
}

impl fmt::Debug for Kek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(KEK_DEBUG)
    }
}

//! Argon2id password verification and the key-encryption key from the same pass.
//!
//! One Argon2id pass over the password and a 16-byte random salt yields 64 bytes. The first 32 are the verifier that the
//! database holds; the last 32 are the key-encryption key that unlocks the person's secret store and is never stored.
//! Verification derives both again, compares the verifier in constant time and hands the key-encryption key to the caller
//! in a [`Kek`], which wipes itself and prints a marker.

use std::fmt;

use serde::Deserialize;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

use crate::store::{AccountId, AccountState, PasswordCredential, StoreError, Tx};
use crate::unlock::{Kdf, Kek, SALT_BYTES, UnlockError, argon2id_into, random};

/// The length of the verifier in bytes: the first half of the derivation.
pub const VERIFIER_BYTES: usize = 32;

/// The length of the whole derivation in bytes.
const DERIVED_BYTES: usize = 64;

/// The only algorithm name a stored parameters text may carry.
const ALGORITHM: &str = "argon2id";

/// The only Argon2 version a stored parameters text may carry (0x13).
const VERSION: u32 = 19;

/// What the `Debug` text of a derivation prints.
const DERIVED_DEBUG: &str = "Derived(..)";

/// Why a password was refused or could not be checked. No text holds the password, a verifier or a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    /// The name or the password is not right. One error for an unknown account, a missing password, a disabled account and a
    /// wrong password, so that the outcome tells nothing about which case applied.
    InvalidCredentials,
    /// The stored row cannot be used: a parameter outside the bounds, a text that does not parse, a wrong length.
    CorruptCredential,
    /// The operating system or the derivation failed.
    Derive,
    /// The store failed.
    Store(StoreError),
}

impl fmt::Display for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasswordError::InvalidCredentials => f.write_str("the name or the password is not right"),
            PasswordError::CorruptCredential => f.write_str("the stored password credential cannot be used"),
            PasswordError::Derive => f.write_str("the password could not be processed"),
            PasswordError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PasswordError {}

impl From<StoreError> for PasswordError {
    fn from(e: StoreError) -> PasswordError {
        PasswordError::Store(e)
    }
}

impl From<UnlockError> for PasswordError {
    fn from(_: UnlockError) -> PasswordError {
        PasswordError::Derive
    }
}

/// The two halves of one derivation. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct Derived {
    verifier: Zeroizing<[u8; VERIFIER_BYTES]>,
    kek: Kek,
}

impl Derived {
    /// The verifier, the half the database holds.
    pub fn verifier(&self) -> &[u8; VERIFIER_BYTES] {
        &self.verifier
    }

    /// The key-encryption key, the half that is never stored.
    pub fn kek(&self) -> &Kek {
        &self.kek
    }

    /// Gives up the verifier (wiped here) and keeps the key-encryption key.
    pub fn into_kek(self) -> Kek {
        self.kek
    }
}

impl fmt::Debug for Derived {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DERIVED_DEBUG)
    }
}

/// One Argon2id pass over `password` and `salt` split into the verifier (bytes 0 to 31) and the key-encryption key (bytes 32
/// to 63). The password is used byte for byte as typed.
pub fn derive_split(password: &[u8], salt: &[u8; SALT_BYTES], kdf: &Kdf) -> Result<Derived, PasswordError> {
    let mut out = Zeroizing::new([0u8; DERIVED_BYTES]);
    argon2id_into(password, salt, kdf, &mut out[..])?;
    let mut verifier = Zeroizing::new([0u8; VERIFIER_BYTES]);
    verifier.copy_from_slice(&out[..VERIFIER_BYTES]);
    let mut key = Zeroizing::new([0u8; VERIFIER_BYTES]);
    key.copy_from_slice(&out[VERIFIER_BYTES..]);
    Ok(Derived { verifier, kek: Kek::from_bytes(&key) })
}

/// The parameters as the text the row keeps.
pub fn params_text(kdf: &Kdf) -> String {
    format!(r#"{{"alg":"{ALGORITHM}","v":{VERSION},"m":{},"t":{},"p":{}}}"#, kdf.m_kib(), kdf.t(), kdf.p())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredParams {
    alg: String,
    v: u32,
    m: u32,
    t: u32,
    p: u32,
}

/// The parameters of a stored text, or [`PasswordError::CorruptCredential`]. A missing or unknown member, another algorithm
/// or version, and a value outside the bounds of [`Kdf`] are all refused here, before any derivation.
pub fn parse_params(text: &str) -> Result<Kdf, PasswordError> {
    let stored: StoredParams = serde_json::from_str(text).map_err(|_| PasswordError::CorruptCredential)?;
    if stored.alg != ALGORITHM || stored.v != VERSION {
        return Err(PasswordError::CorruptCredential);
    }
    Kdf::new(stored.m, stored.t, stored.p).map_err(|_| PasswordError::CorruptCredential)
}

/// Stores the password of `account` under a new random salt, replacing an earlier one, and answers the key-encryption key.
pub fn enrol_password(tx: &Tx<'_>, account: AccountId, password: &str, kdf: &Kdf, now: i64) -> Result<Kek, PasswordError> {
    let salt = random::<SALT_BYTES>()?;
    let derived = derive_split(password.as_bytes(), &salt, kdf)?;
    tx.password_set(
        account,
        &PasswordCredential {
            verifier: derived.verifier().to_vec(),
            salt: salt.to_vec(),
            params: params_text(kdf),
            changed_at: now,
        },
    )?;
    Ok(derived.into_kek())
}

/// Checks `password` for the account called `username` and answers the account and its key-encryption key.
pub fn verify_password(tx: &Tx<'_>, username: &str, password: &str) -> Result<(AccountId, Kek), PasswordError> {
    let Some(account) = tx.account_by_username(username)? else {
        return Err(PasswordError::InvalidCredentials);
    };
    let Some(credential) = tx.password_get(account.id)? else {
        return Err(PasswordError::InvalidCredentials);
    };
    let kdf = parse_params(&credential.params)?;
    let salt: [u8; SALT_BYTES] = credential.salt.as_slice().try_into().map_err(|_| PasswordError::CorruptCredential)?;
    if credential.verifier.len() != VERIFIER_BYTES {
        return Err(PasswordError::CorruptCredential);
    }
    let derived = derive_split(password.as_bytes(), &salt, &kdf)?;
    let same: bool = derived.verifier().ct_eq(&credential.verifier[..]).into();
    if same && account.state == AccountState::Active {
        Ok((account.id, derived.into_kek()))
    } else {
        Err(PasswordError::InvalidCredentials)
    }
}

//! Time-based one-time codes (RFC 6238) for the second step of the password method.
//!
//! The codes are HMAC-SHA1, six digits, thirty seconds, computed by the `totp-rs` crate from a 20-byte secret that comes
//! from the operating system's random generator. The secret is stored sealed under the server key and opened only inside the
//! check. A code is accepted once: the store's conditional update of `last_step` is what stops a replay.

use std::fmt;

use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Builder, Totp};
use zeroize::Zeroizing;

use crate::sealed::{Sealer, SealedError};
use crate::store::{AccountId, StoreError, Tx};
use crate::unlock::random;

/// The length of a secret in bytes (160 bits, the size RFC 4226 recommends).
pub const SECRET_BYTES: usize = 20;

/// The digits of a code.
pub const DIGITS: usize = 6;

/// The seconds of one step.
pub const STEP_SECONDS: i64 = 30;

/// Why a code was refused or could not be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TotpError {
    /// The code is not right. One error for a code of the wrong shape, one outside the window, one that was used, and one for
    /// an account that is disabled, deleted or has no secret.
    InvalidCode,
    /// The operating system gave no random bytes.
    Random,
    /// The secret could not be sealed or opened.
    Seal(SealedError),
    /// The store failed.
    Store(StoreError),
}

impl fmt::Display for TotpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TotpError::InvalidCode => f.write_str("the code is not right"),
            TotpError::Random => f.write_str("the operating system gave no random bytes"),
            TotpError::Seal(e) => write!(f, "{e}"),
            TotpError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TotpError {}

impl From<StoreError> for TotpError {
    fn from(e: StoreError) -> TotpError {
        TotpError::Store(e)
    }
}

impl From<SealedError> for TotpError {
    fn from(e: SealedError) -> TotpError {
        TotpError::Seal(e)
    }
}

/// A new secret from the operating system's random generator.
pub fn generate_secret() -> Result<Zeroizing<[u8; SECRET_BYTES]>, TotpError> {
    Ok(Zeroizing::new(random::<SECRET_BYTES>().map_err(|_| TotpError::Random)?))
}

/// The context string the secret of an account is sealed under.
pub fn seal_context(account: AccountId) -> String {
    format!("totp:{}", account.get())
}

fn engine(secret: &[u8]) -> Result<Totp, TotpError> {
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(DIGITS as u8)
        .with_step_duration(STEP_SECONDS as u64)
        .with_secret(secret.to_vec())
        .build()
        .map_err(|_| TotpError::InvalidCode)
}

/// The step of a time: whole seconds since the epoch divided by thirty.
pub fn step_of(unix: i64) -> i64 {
    unix.div_euclid(STEP_SECONDS)
}

/// The code of a secret at a step.
fn code_at_step(totp: &Totp, step: i64) -> Zeroizing<String> {
    let time = u64::try_from(step.max(0)).unwrap_or(0).saturating_mul(STEP_SECONDS as u64);
    Zeroizing::new(totp.generate(time).to_string())
}

/// The code a secret gives at a time (unix seconds).
pub fn code_at(secret: &[u8], unix: i64) -> Result<Zeroizing<String>, TotpError> {
    Ok(code_at_step(&engine(secret)?, step_of(unix)))
}

/// Checks `code` against the current step of `secret`, answering the step.
fn check_current(secret: &[u8], code: &str, now: i64) -> Result<i64, TotpError> {
    let totp = engine(secret)?;
    let step = step_of(now);
    let expected = code_at_step(&totp, step);
    if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) { Ok(step) } else { Err(TotpError::InvalidCode) }
}

/// Checks the code of an account and accepts its step once.
pub fn verify_and_consume(
    tx: &Tx<'_>,
    sealer: &Sealer,
    account: AccountId,
    code: &str,
    now: i64,
) -> Result<(), TotpError> {
    let Some(row) = tx.totp_get(account)? else {
        return Err(TotpError::InvalidCode);
    };
    let secret = sealer.open(&seal_context(account), &row.sealed_secret)?;
    let step = check_current(&secret, code, now)?;
    if step > row.last_step && tx.totp_accept_step(account, step)? { Ok(()) } else { Err(TotpError::InvalidCode) }
}

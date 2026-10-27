//! Time-based one-time codes (RFC 6238) for the second step of the password method.
//!
//! The codes are HMAC-SHA1, six digits, thirty seconds, computed by the `totp-rs` crate from a 20-byte secret that comes
//! from the operating system's random generator. The secret is stored sealed under the server key and opened only inside the
//! check. A code is accepted once: the store's conditional update of `last_step` is what stops a replay.

use std::fmt;

use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};
use totp_rs::{Algorithm, Builder, Secret, Totp};
use zeroize::Zeroizing;

use crate::sealed::{SealedError, Sealer};
use crate::store::{AccountId, AccountState, StoreError, Tx};
use crate::unlock::random;

/// The length of a secret in bytes (160 bits, the size RFC 4226 recommends).
pub const SECRET_BYTES: usize = 20;

/// The digits of a code.
pub const DIGITS: usize = 6;

/// The seconds of one step.
pub const STEP_SECONDS: i64 = 30;

/// How many steps before and after the clock's are accepted (network and clock drift).
pub const WINDOW_STEPS: i64 = 1;

/// Why a code was refused or could not be checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TotpError {
    /// The code is not right. One error for a code of the wrong shape, one outside the window, one that was used, and one for
    /// an account that is disabled, deleted or has no secret.
    InvalidCode,
    /// The operating system gave no random bytes.
    Random,
    /// A secret is not 20 bytes.
    BadSecret,
    /// The issuer or the account name of a provisioning link is empty, too long or holds a colon.
    BadLabel,
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
            TotpError::BadSecret => f.write_str("a one-time code secret is 20 bytes"),
            TotpError::BadLabel => f.write_str("the issuer and the account name of a code link are 1 to 128 bytes without a colon"),
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
    if secret.len() != SECRET_BYTES {
        return Err(TotpError::BadSecret);
    }
    Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(DIGITS as u8)
        .with_step_duration(STEP_SECONDS as u64)
        .with_skew(WINDOW_STEPS as u16)
        .with_secret(secret.to_vec())
        .build()
        .map_err(|_| TotpError::BadSecret)
}

/// The step of a time: whole seconds since the epoch divided by thirty.
pub fn step_of(unix: i64) -> i64 {
    unix.div_euclid(STEP_SECONDS)
}

/// The code of a secret at a step. A step before the epoch has no code; it gives the code of step zero, which a caller never
/// accepts because the matching step must be greater than zero.
fn code_at_step(totp: &Totp, step: i64) -> Zeroizing<String> {
    let time = u64::try_from(step.max(0)).unwrap_or(0).saturating_mul(STEP_SECONDS as u64);
    Zeroizing::new(totp.generate(time).to_string())
}

/// The code a secret gives at a time (unix seconds): six ASCII digits, with leading zeros.
pub fn code_at(secret: &[u8], unix: i64) -> Result<Zeroizing<String>, TotpError> {
    Ok(code_at_step(&engine(secret)?, step_of(unix)))
}

/// Whether `code` has the shape of a code: exactly six ASCII digits. Anything else is refused before any HMAC is computed.
pub fn well_formed(code: &str) -> bool {
    code.len() == DIGITS && code.bytes().all(|b| b.is_ascii_digit())
}

/// The step among `now - 1`, `now` and `now + 1` whose code is `code`, compared in constant time over all three (no early
/// exit). When two steps give the same code, the later one is answered, so that the guard against reuse covers both.
///
/// `totp-rs` has `check`, which also looks at this window, but it stops at the first match; the three codes are computed with
/// its `generate` and compared here with `subtle`.
fn match_window(totp: &Totp, code: &str, now: i64) -> Option<i64> {
    let centre = step_of(now);
    let mut found = Choice::from(0);
    let mut matched = 0i64;
    for offset in -WINDOW_STEPS..=WINDOW_STEPS {
        let step = centre.saturating_add(offset);
        let equal = if step < 0 { Choice::from(0) } else { code_at_step(totp, step).as_bytes().ct_eq(code.as_bytes()) };
        matched = i64::conditional_select(&matched, &step, equal);
        found |= equal;
    }
    if bool::from(found) { Some(matched) } else { None }
}

/// Checks a code against a secret that is not stored yet (an enrolment, before the person has proved the app works) and
/// answers the step it matched, or [`TotpError::InvalidCode`]. The format gate and the window are those of the sign-in; no
/// store is involved and nothing is remembered.
pub fn check_candidate(secret: &[u8], code: &str, now: i64) -> Result<i64, TotpError> {
    if !well_formed(code) {
        return Err(TotpError::InvalidCode);
    }
    match_window(&engine(secret)?, code, now).ok_or(TotpError::InvalidCode)
}

/// Stores a new secret for `account`, sealed under the account's context, replacing any earlier secret completely, with
/// `last_step` set to `step` (the step [`check_candidate`] answered), so the code that has just been shown to work cannot
/// sign anyone in a second time.
pub fn store_secret(
    tx: &Tx<'_>,
    sealer: &Sealer,
    account: AccountId,
    secret: &[u8],
    step: i64,
) -> Result<(), TotpError> {
    if secret.len() != SECRET_BYTES {
        return Err(TotpError::BadSecret);
    }
    let sealed = sealer.seal(&seal_context(account), secret)?;
    tx.totp_set(account, &sealed)?;
    tx.totp_accept_step(account, step)?;
    Ok(())
}

/// Checks the code of an account and accepts its step once. In order: the shape of the code; the account is active and has a
/// secret; the secret is opened (and wiped when this returns); the code matches the step before, at or after the clock's;
/// that step is greater than the account's `last_step`; and the store's one conditional update takes it, so of several
/// callers with the same code exactly one succeeds. Every refusal is the one [`TotpError::InvalidCode`].
pub fn verify_and_consume(
    tx: &Tx<'_>,
    sealer: &Sealer,
    account: AccountId,
    code: &str,
    now: i64,
) -> Result<(), TotpError> {
    if !well_formed(code) {
        return Err(TotpError::InvalidCode);
    }
    match tx.account_get(account)? {
        Some(found) if found.state == AccountState::Active => {}
        _ => return Err(TotpError::InvalidCode),
    }
    let Some(row) = tx.totp_get(account)? else {
        return Err(TotpError::InvalidCode);
    };
    let secret = sealer.open(&seal_context(account), &row.sealed_secret)?;
    let step = match_window(&engine(&secret)?, code, now).ok_or(TotpError::InvalidCode)?;
    if step > row.last_step && tx.totp_accept_step(account, step)? { Ok(()) } else { Err(TotpError::InvalidCode) }
}

// ---- the provisioning link ----

/// The longest issuer or account name in a provisioning link, in bytes.
const MAX_LABEL_BYTES: usize = 128;

/// Percent-encodes everything except the unreserved characters of RFC 3986 (`A-Z a-z 0-9 - _ . ~`).
fn percent_encode(text: &str, out: &mut String) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            out.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 15)]));
        }
    }
}

/// The `otpauth://` link an authenticator app reads (usually from a QR code): the issuer and the account name in the label,
/// and the secret (base32 without padding), the issuer, the algorithm, the digits and the period as parameters. Every part is
/// percent-encoded. The algorithm, the digits and the period are always written, although the usual defaults match, because
/// an app that guesses wrong would show codes that never fit. The text holds the secret: it is wiped when dropped.
pub fn provisioning_uri(issuer: &str, account_name: &str, secret: &[u8]) -> Result<Zeroizing<String>, TotpError> {
    for part in [issuer, account_name] {
        if part.is_empty() || part.len() > MAX_LABEL_BYTES || part.contains(':') {
            return Err(TotpError::BadLabel);
        }
    }
    if secret.len() != SECRET_BYTES {
        return Err(TotpError::BadSecret);
    }
    let base32 = Zeroizing::new(Secret::from(secret).to_base32());
    let mut uri = Zeroizing::new(String::with_capacity(160));
    uri.push_str("otpauth://totp/");
    percent_encode(issuer, &mut uri);
    uri.push(':');
    percent_encode(account_name, &mut uri);
    uri.push_str("?secret=");
    uri.push_str(&base32);
    uri.push_str("&issuer=");
    percent_encode(issuer, &mut uri);
    uri.push_str("&algorithm=SHA1&digits=6&period=30");
    Ok(uri)
}

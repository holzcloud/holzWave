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
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// The length of a key in bytes.
pub const KEY_BYTES: usize = 32;

/// The length of the salt of the Argon2id derivations in bytes.
pub const SALT_BYTES: usize = 16;

/// What the `Debug` text of a key prints.
const KEK_DEBUG: &str = "Kek(..)";

/// What the `Debug` text of a recovery code prints.
const RECOVERY_DEBUG: &str = "RecoveryCode(..)";

/// The HKDF info string of the key from a passkey's PRF result.
pub const PRF_INFO: &[u8] = b"holzwave/prf/v1";

/// The HKDF info string of the key from a recovery code.
pub const RECOVERY_INFO: &[u8] = b"holzwave/recovery/v1";

/// The longest passphrase that is derived from, in bytes. A longer text is not a passphrase a person types.
pub const MAX_PASSPHRASE_BYTES: usize = 4096;

/// The shortest and the longest salt of an HKDF derivation, in bytes.
pub const HKDF_SALT_BYTES: std::ops::RangeInclusive<usize> = 16..=64;

/// The random bytes in a recovery code: 160 bits, which Crockford base32 spells in 32 characters.
const RECOVERY_BYTES: usize = 20;

/// The characters of a recovery code (Crockford base32: no `I`, `L`, `O`, `U`).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The longest recovery code text that is parsed, in bytes: far more than 32 characters with separators.
const MAX_RECOVERY_TEXT_BYTES: usize = 128;

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

/// HKDF-SHA256 over `ikm` with `salt` and `info` into a 32-byte key.
fn hkdf_kek(ikm: &[u8], salt: &[u8], info: &[u8]) -> Result<Kek, UnlockError> {
    if !HKDF_SALT_BYTES.contains(&salt.len()) {
        return Err(UnlockError::BadLength);
    }
    let mut out = Zeroizing::new([0u8; KEY_BYTES]);
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut *out).map_err(|_| UnlockError::Derive)?;
    Ok(Kek(out))
}

/// The key-encryption key of a passphrase: Argon2id with the person's own 16-byte salt and `kdf`. The passphrase is used as
/// typed, with no Unicode normalisation.
pub fn kek_from_passphrase(passphrase: &str, salt: &[u8; SALT_BYTES], kdf: &Kdf) -> Result<Kek, UnlockError> {
    if passphrase.len() > MAX_PASSPHRASE_BYTES {
        return Err(UnlockError::BadLength);
    }
    let mut out = Zeroizing::new([0u8; KEY_BYTES]);
    argon2id_into(passphrase.as_bytes(), salt, kdf, &mut *out)?;
    Ok(Kek(out))
}

/// The key-encryption key of a passkey: HKDF-SHA256 over the 32-byte PRF extension output with the credential's salt (16 to 64
/// bytes) and the info string `holzwave/prf/v1`.
pub fn kek_from_prf(prf_output: &[u8; KEY_BYTES], salt: &[u8]) -> Result<Kek, UnlockError> {
    hkdf_kek(prf_output, salt, PRF_INFO)
}

/// The key-encryption key of a recovery code: HKDF-SHA256 over the decoded 20 bytes with a salt (16 to 64 bytes) and the info
/// string `holzwave/recovery/v1`. The separators of the display form do not matter, because the decoded bytes are used.
pub fn kek_from_recovery(code: &RecoveryCode, salt: &[u8]) -> Result<Kek, UnlockError> {
    hkdf_kek(&code.0[..], salt, RECOVERY_INFO)
}

/// A recovery code: 160 random bits, shown to the person once. Not `Clone`, a redacting `Debug`, wiped when dropped.
pub struct RecoveryCode(Zeroizing<[u8; RECOVERY_BYTES]>);

impl RecoveryCode {
    /// The text: eight groups of four Crockford base32 characters separated by `-`. It takes the code, so it can be shown
    /// once; the caller derives the key first. The text is wiped when the caller drops it.
    pub fn display(self) -> Zeroizing<String> {
        let mut text = Zeroizing::new(String::with_capacity(32 + 7));
        let mut buffer: u16 = 0;
        let mut bits = 0u32;
        let mut written = 0usize;
        for byte in self.0.iter() {
            buffer = (buffer << 8) | u16::from(*byte);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                if written > 0 && written.is_multiple_of(4) {
                    text.push('-');
                }
                text.push(char::from(CROCKFORD[usize::from((buffer >> bits) & 31)]));
                written += 1;
            }
            buffer &= (1 << bits) - 1;
        }
        text
    }
}

impl fmt::Debug for RecoveryCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(RECOVERY_DEBUG)
    }
}

/// A new recovery code from the operating system's random bytes.
pub fn generate_recovery_code() -> Result<RecoveryCode, UnlockError> {
    Ok(RecoveryCode(Zeroizing::new(random::<RECOVERY_BYTES>()?)))
}

/// Read a recovery code the way a person types it: lower case, spaces and `-` are fine, `O` is `0`, `I` and `L` are `1`.
/// Any other text, or a text that is not 32 characters of the alphabet, is [`UnlockError::RecoveryCode`]. The code has no
/// checksum: a mistyped code is a valid code that unwraps nothing.
pub fn parse_recovery_code(text: &str) -> Result<RecoveryCode, UnlockError> {
    if text.len() > MAX_RECOVERY_TEXT_BYTES {
        return Err(UnlockError::RecoveryCode);
    }
    let mut bytes = Zeroizing::new([0u8; RECOVERY_BYTES]);
    let mut buffer: u16 = 0;
    let mut bits = 0u32;
    let mut count = 0usize;
    let mut next = 0usize;
    for c in text.chars().filter(|c| !c.is_whitespace() && *c != '-') {
        let value = match c.to_ascii_uppercase() {
            'O' => 0,
            'I' | 'L' => 1,
            other => {
                let index = CROCKFORD.iter().position(|a| char::from(*a) == other).ok_or(UnlockError::RecoveryCode)?;
                u16::try_from(index).map_err(|_| UnlockError::RecoveryCode)?
            }
        };
        count += 1;
        if count > 32 {
            return Err(UnlockError::RecoveryCode);
        }
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            bytes[next] = u8::try_from((buffer >> bits) & 0xff).map_err(|_| UnlockError::RecoveryCode)?;
            next += 1;
            buffer &= (1 << bits) - 1;
        }
    }
    if count != 32 {
        return Err(UnlockError::RecoveryCode);
    }
    Ok(RecoveryCode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_info_string_separates_the_keys_of_the_same_input() {
        let ikm = [7u8; 32];
        let salt = [9u8; 16];
        let prf = hkdf_kek(&ikm, &salt, PRF_INFO).unwrap();
        let recovery = hkdf_kek(&ikm, &salt, RECOVERY_INFO).unwrap();
        let other = hkdf_kek(&ikm, &salt, b"holzwave/other/v1").unwrap();
        assert!(!prf.same_as(&recovery));
        assert!(!prf.same_as(&other));
        assert!(!recovery.same_as(&other));
        assert!(prf.same_as(&hkdf_kek(&ikm, &salt, PRF_INFO).unwrap()));
    }

    #[test]
    fn the_salt_changes_the_key_and_its_length_is_bounded() {
        let ikm = [7u8; 32];
        let a = hkdf_kek(&ikm, &[1u8; 16], PRF_INFO).unwrap();
        let b = hkdf_kek(&ikm, &[2u8; 16], PRF_INFO).unwrap();
        assert!(!a.same_as(&b));
        assert_eq!(hkdf_kek(&ikm, &[1u8; 15], PRF_INFO).unwrap_err(), UnlockError::BadLength);
        assert_eq!(hkdf_kek(&ikm, &[1u8; 65], PRF_INFO).unwrap_err(), UnlockError::BadLength);
        assert!(hkdf_kek(&ikm, &[1u8; 64], PRF_INFO).is_ok());
    }

    #[test]
    fn the_info_strings_are_the_contract_s() {
        assert_eq!(PRF_INFO, b"holzwave/prf/v1");
        assert_eq!(RECOVERY_INFO, b"holzwave/recovery/v1");
    }

    #[test]
    fn a_code_survives_display_and_parse_for_every_bit_pattern_of_the_edges() {
        for fill in [0x00u8, 0xff, 0xa5, 0x5a] {
            let code = RecoveryCode(Zeroizing::new([fill; RECOVERY_BYTES]));
            let kek = kek_from_recovery(&code, &[3u8; 16]).unwrap();
            let text = code.display();
            let again = parse_recovery_code(&text).unwrap();
            assert!(kek.same_as(&kek_from_recovery(&again, &[3u8; 16]).unwrap()), "{fill:#x}");
        }
    }
}

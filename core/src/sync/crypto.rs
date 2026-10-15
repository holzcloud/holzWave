//! The encrypted file format of the sync (docs/sync.md). Fixed first, because every later byte on a server depends on it.
//!
//! A passphrase becomes a 256 bit key with Argon2id (the parameters and a random salt live in the plain `meta.json` of
//! the sync folder, so every device derives the same key). Each file is one blob:
//!
//! ```text
//! "HWX" 0x01 | counter u64 BE | time i64 BE | device [u8; 16] | nonce [u8; 24] | XChaCha20-Poly1305(plaintext)
//! ```
//!
//! The header up to the nonce is authenticated as associated data together with the file's name, so a blob cannot be
//! moved to another name, nor its version stamp changed, without the tag failing. The server sees names, sizes and
//! the header, never a content.

use std::io;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::{Error, Result};

pub const MAGIC: &[u8; 3] = b"HWX";
pub const FORMAT: u8 = 1;
/// Magic, version, counter, time, device.
const HEADER: usize = 3 + 1 + 8 + 8 + 16;
const NONCE: usize = 24;
const TAG: usize = 16;
/// The largest blob that is decrypted: a settings file is kilobytes, so this refuses a hostile server's giant answer.
pub const MAX_BLOB: usize = 8 * 1024 * 1024;

/// Argon2id parameters, stored in `meta.json`. The defaults are the OWASP minimum for Argon2id (19 MiB, 2 passes) raised
/// to 64 MiB and 3 passes: a derivation costs about a quarter of a second once per start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kdf {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for Kdf {
    fn default() -> Self {
        Self { m_kib: 64 * 1024, t: 3, p: 1 }
    }
}

impl Kdf {
    /// The parameters a server may ask for are bounded, so a hostile `meta.json` cannot make a device allocate gigabytes.
    pub fn check(&self) -> Result<()> {
        if !(8 * 1024..=1024 * 1024).contains(&self.m_kib) || !(1..=10).contains(&self.t) || !(1..=8).contains(&self.p) {
            return Err(err("the key derivation parameters of the sync folder are out of range"));
        }
        Ok(())
    }
}

/// The key of a sync folder. Zeroed when dropped; never printed.
pub struct Key(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

/// The version stamp and writer of a blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// Counts writes of this file across all devices; the higher one is the newer.
    pub counter: u64,
    /// Seconds since the epoch on the writer's clock; breaks a tie of counters only.
    pub time: i64,
    /// A random id of the device that wrote it.
    pub device: [u8; 16],
}

fn err(m: &str) -> Error {
    Error::Io(io::Error::other(m.to_owned()))
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| err(&format!("no random bytes: {e}")))?;
    Ok(b)
}

impl Key {
    /// Derive the key from `passphrase` and the folder's `salt`.
    pub fn derive(passphrase: &str, salt: &[u8], kdf: Kdf) -> Result<Key> {
        kdf.check()?;
        if salt.len() < 16 || salt.len() > 64 {
            return Err(err("the salt of the sync folder has a wrong length"));
        }
        let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| err(&format!("key derivation: {e}")))?;
        let mut out = Zeroizing::new([0u8; 32]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), salt, &mut *out)
            .map_err(|e| err(&format!("key derivation: {e}")))?;
        Ok(Key(out))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&*self.0).into())
    }

    /// Encrypt `plain` as the file `name` with `stamp`.
    pub fn seal(&self, name: &str, stamp: Stamp, plain: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(HEADER + NONCE + plain.len() + TAG);
        out.extend_from_slice(MAGIC);
        out.push(FORMAT);
        out.extend_from_slice(&stamp.counter.to_be_bytes());
        out.extend_from_slice(&stamp.time.to_be_bytes());
        out.extend_from_slice(&stamp.device);
        let nonce: [u8; NONCE] = random()?;
        out.extend_from_slice(&nonce);
        let aad = aad(name, &out[..HEADER]);
        let ct = self
            .cipher()
            .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad: &aad })
            .map_err(|_| err("encryption failed"))?;
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypt a blob. The tag also proves the name and the stamp, and a wrong passphrase fails here.
    pub fn open(&self, name: &str, blob: &[u8]) -> Result<(Stamp, Zeroizing<Vec<u8>>)> {
        let stamp = stamp_of(blob)?;
        let aad = aad(name, &blob[..HEADER]);
        let nonce = XNonce::try_from(&blob[HEADER..HEADER + NONCE]).map_err(|_| err("short header"))?;
        let plain = self
            .cipher()
            .decrypt(&nonce, Payload { msg: &blob[HEADER + NONCE..], aad: &aad })
            .map_err(|_| err("the file could not be decrypted: wrong passphrase, or the file was changed"))?;
        Ok((stamp, Zeroizing::new(plain)))
    }
}

fn aad(name: &str, header: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(header.len() + name.len() + 1);
    a.extend_from_slice(header);
    a.push(0);
    a.extend_from_slice(name.as_bytes());
    a
}

/// The stamp in a blob's header, without the key. Refuses a blob that is too short, too long or of another version.
pub fn stamp_of(blob: &[u8]) -> Result<Stamp> {
    if blob.len() < HEADER + NONCE + TAG || blob.len() > MAX_BLOB || &blob[..3] != MAGIC {
        return Err(err("not a holzWave sync file"));
    }
    if blob[3] != FORMAT {
        return Err(err("this sync file is of a newer format; update holzWave"));
    }
    let counter = u64::from_be_bytes(blob[4..12].try_into().map_err(|_| err("short header"))?);
    let time = i64::from_be_bytes(blob[12..20].try_into().map_err(|_| err("short header"))?);
    let mut device = [0u8; 16];
    device.copy_from_slice(&blob[20..36]);
    Ok(Stamp { counter, time, device })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cheap parameters keep the tests quick; the range check still passes.
    fn fast() -> Kdf {
        Kdf { m_kib: 8 * 1024, t: 1, p: 1 }
    }

    fn key(pass: &str) -> Key {
        Key::derive(pass, &[7u8; 16], fast()).unwrap()
    }

    const STAMP: Stamp = Stamp { counter: 3, time: 1_760_000_000, device: [9; 16] };

    #[test]
    fn a_blob_round_trips_and_shows_only_its_stamp() {
        let blob = key("pw").seal("settings.json", STAMP, b"{\"a\":1}").unwrap();
        assert!(!blob.windows(5).any(|w| w == b"{\"a\":"), "the content is not in the blob");
        assert_eq!(stamp_of(&blob).unwrap(), STAMP);
        let (stamp, plain) = key("pw").open("settings.json", &blob).unwrap();
        assert_eq!(stamp, STAMP);
        assert_eq!(&*plain, b"{\"a\":1}");
    }

    #[test]
    fn two_seals_of_the_same_text_differ() {
        let k = key("pw");
        assert_ne!(k.seal("a", STAMP, b"x").unwrap(), k.seal("a", STAMP, b"x").unwrap());
    }

    #[test]
    fn a_wrong_passphrase_a_moved_name_and_a_changed_stamp_all_fail() {
        let blob = key("pw").seal("a.json", STAMP, b"x").unwrap();
        assert!(key("other").open("a.json", &blob).is_err());
        assert!(key("pw").open("b.json", &blob).is_err(), "bound to its name");
        let mut forged = blob.clone();
        forged[11] ^= 1; // the counter
        assert!(key("pw").open("a.json", &forged).is_err(), "the stamp is authenticated");
        let mut flipped = blob;
        *flipped.last_mut().unwrap() ^= 1;
        assert!(key("pw").open("a.json", &flipped).is_err());
    }

    #[test]
    fn foreign_or_oversized_input_is_refused_before_any_work() {
        assert!(stamp_of(b"").is_err());
        assert!(stamp_of(&[0u8; 100]).is_err());
        let mut blob = key("pw").seal("a", STAMP, b"x").unwrap();
        blob[3] = 2;
        assert!(stamp_of(&blob).unwrap_err().to_string().contains("newer format"));
        assert!(stamp_of(&vec![b'H'; MAX_BLOB + 1]).is_err());
    }

    #[test]
    fn hostile_key_derivation_parameters_are_refused() {
        assert!(Key::derive("p", &[0; 16], Kdf { m_kib: 4_000_000, t: 3, p: 1 }).is_err());
        assert!(Key::derive("p", &[0; 16], Kdf { m_kib: 64, t: 3, p: 1 }).is_err());
        assert!(Key::derive("p", &[0; 4], Kdf::default()).is_err());
        assert!(Kdf::default().check().is_ok());
    }

    #[test]
    fn the_same_passphrase_and_salt_give_the_same_key() {
        let blob = key("pw").seal("a", STAMP, b"x").unwrap();
        assert!(key("pw").open("a", &blob).is_ok());
        assert_eq!(format!("{:?}", key("pw")), "Key(..)");
    }
}

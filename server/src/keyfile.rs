//! The server key file: created by the keygen subcommand with mode 0600, read at start, never printed.
//!
//! The file holds one line, `hws1:` and 43 base64url characters, which are 32 random bytes. The key never leaves
//! [`ServerKey`]: it has no `Clone`, a `Debug` text that prints a fixed marker, and it is wiped when dropped. Parts of the
//! server that need a key of their own ask for one by [`Purpose`], and each purpose gets an independent key.
//!
//! A refusal is one sentence that names the file and what to do, and never holds a byte of the file's content.

use std::fmt;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use rustix::fs::OFlags;
use sha2::Sha256;
use zeroize::Zeroizing;

/// The start of the key line; the digit is the version of the format.
pub const KEY_PREFIX: &str = "hws1:";

/// The length of the key in bytes.
pub const KEY_BYTES: usize = 32;

/// The number of base64url characters that spell 32 bytes without padding.
const ENCODED_BYTES: usize = 43;

/// The largest size of a key file that is read; anything bigger is not a key file.
pub const MAX_KEY_FILE_BYTES: u64 = 256;

/// The permission bits that make a private file too open: group write, group execute and every bit for others. The group
/// read bit is allowed because a secret mounted into a container shows up with mode 0440 and a file group.
pub const LOOSE_MODE_MASK: u32 = 0o037;

/// The start of the HKDF info string of every purpose key.
const INFO_PREFIX: &str = "holzwave-server/v1/";

/// What the `Debug` text of a key prints.
const DEBUG_MARKER: &str = "ServerKey(..)";

/// The name the sentences use for the key file.
const KEY_FILE: &str = "server key file";

/// A refusal to use a file: one sentence that names the file and the fix. It never holds file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateFileError(String);

impl PrivateFileError {
    fn new(sentence: String) -> PrivateFileError {
        PrivateFileError(sentence)
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PrivateFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PrivateFileError {}

/// A path as it appears in a sentence: control characters are replaced, so the sentence stays on one line.
pub fn shown(path: &Path) -> String {
    path.display().to_string().chars().map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// The flags every open of a private file adds: refuse a link as the last component, and never block on a pipe.
fn guarded_flags() -> i32 {
    i32::try_from((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits()).expect("the open flags fit in an i32")
}

/// Creates the key file at `path` with 32 random bytes from the operating system.
///
/// The file is made with exclusive creation, so an existing file or link is refused and left as it was, and with mode
/// 0600 given at creation and set again through the descriptor so that the umask cannot loosen or tighten it. A failure
/// after the file was made removes the file again. Nothing is printed.
pub fn generate(path: &Path) -> Result<(), PrivateFileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(guarded_flags())
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                PrivateFileError::new(format!(
                    "the {KEY_FILE} {} already exists and is never overwritten; name a new file or remove it yourself",
                    shown(path)
                ))
            } else {
                PrivateFileError::new(format!(
                    "the {KEY_FILE} {} cannot be created ({}); check that its directory exists and can be written",
                    shown(path),
                    e.kind()
                ))
            }
        })?;

    let written = write_new_key(&mut file);
    if let Err(kind) = written {
        drop(file);
        // The file was made by this call a moment ago, so removing it leaves nothing of someone else's behind.
        let _ = std::fs::remove_file(path);
        return Err(PrivateFileError::new(format!(
            "the {KEY_FILE} {} could not be written ({kind}); nothing was left behind",
            shown(path)
        )));
    }
    Ok(())
}

/// Writes the key line into the new file and makes it durable. The error is a short kind, never the key.
fn write_new_key(file: &mut File) -> Result<(), String> {
    file.set_permissions(Permissions::from_mode(0o600)).map_err(|e| e.kind().to_string())?;
    let mut raw = Zeroizing::new([0u8; KEY_BYTES]);
    getrandom::fill(&mut raw[..]).map_err(|_| "the operating system gave no random bytes".to_owned())?;
    let mut line = Zeroizing::new([0u8; KEY_PREFIX.len() + ENCODED_BYTES + 1]);
    line[..KEY_PREFIX.len()].copy_from_slice(KEY_PREFIX.as_bytes());
    let end = KEY_PREFIX.len() + ENCODED_BYTES;
    let n = URL_SAFE_NO_PAD
        .encode_slice(&raw[..], &mut line[KEY_PREFIX.len()..end])
        .map_err(|_| "the key does not fit its line".to_owned())?;
    debug_assert_eq!(n, ENCODED_BYTES);
    line[end] = b'\n';
    file.write_all(&line[..]).map_err(|e| e.kind().to_string())?;
    file.sync_all().map_err(|e| e.kind().to_string())
}

/// Opens a private file for reading and checks it through the opened descriptor.
///
/// `what` names the kind of file in the sentences, for example `server key file`. The open refuses a link as the last
/// component and never blocks on a pipe. The type and mode are read from the descriptor, so the file that was checked is the
/// file that is returned. The file must be a regular file and must not have a permission bit in [`LOOSE_MODE_MASK`]. The
/// owner is not checked, because a mounted secret belongs to another user.
pub fn open_private(path: &Path, what: &str) -> Result<File, PrivateFileError> {
    let file = OpenOptions::new().read(true).custom_flags(guarded_flags()).open(path).map_err(|e| {
        if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
            PrivateFileError::new(format!(
                "the {what} {} is a link and links are refused; name the file itself",
                shown(path)
            ))
        } else if e.kind() == std::io::ErrorKind::NotFound {
            PrivateFileError::new(format!("the {what} {} does not exist; check the path or create the file", shown(path)))
        } else {
            PrivateFileError::new(format!(
                "the {what} {} cannot be opened ({}); check the path and that the server may read it",
                shown(path),
                e.kind()
            ))
        }
    })?;
    let meta = file.metadata().map_err(|e| {
        PrivateFileError::new(format!("the {what} {} cannot be examined ({})", shown(path), e.kind()))
    })?;
    if !meta.file_type().is_file() {
        return Err(PrivateFileError::new(format!(
            "the {what} {} is not a regular file; name a regular file",
            shown(path)
        )));
    }
    let mode = meta.permissions().mode() & 0o7777;
    if mode & LOOSE_MODE_MASK != 0 {
        return Err(PrivateFileError::new(format!(
            "the {what} {} has mode {mode:04o}, which is too open; run chmod 0600 on it (a group may read it, nobody else may write or read it)",
            shown(path)
        )));
    }
    Ok(file)
}

/// What a key is derived for. A purpose is an enum, so that no free string can be passed and every use is listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Sealing secrets that live in the database, such as the TOTP seeds.
    Sealed,
    /// Binding the anti-forgery token of a session.
    Csrf,
    /// Sealing the ACME account key and the certificate keys that live on disk.
    Acme,
}

impl Purpose {
    /// The name that goes into the HKDF info string.
    pub fn name(self) -> &'static str {
        match self {
            Purpose::Sealed => "sealed",
            Purpose::Csrf => "csrf",
            Purpose::Acme => "acme",
        }
    }
}

/// The server key: 32 random bytes that only [`Purpose`] keys leave.
pub struct ServerKey(Zeroizing<[u8; KEY_BYTES]>);

impl fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DEBUG_MARKER)
    }
}

impl ServerKey {
    /// A key from its bytes; `None` for the all-zero key, which is never a real key.
    pub fn from_bytes(bytes: [u8; KEY_BYTES]) -> Option<ServerKey> {
        let bytes = Zeroizing::new(bytes);
        if bytes.iter().all(|b| *b == 0) {
            return None;
        }
        Some(ServerKey(bytes))
    }

    /// Reads and checks the key file at `path`.
    ///
    /// Refused, each with a sentence that names the file: a missing file, a link, a file that is not a regular file, a
    /// file with a permission bit in [`LOOSE_MODE_MASK`], a file larger than [`MAX_KEY_FILE_BYTES`], a file of the wrong
    /// shape or length, and the all-zero key.
    pub fn load(path: &Path) -> Result<ServerKey, PrivateFileError> {
        let file = open_private(path, KEY_FILE)?;
        let mut content = Zeroizing::new(Vec::with_capacity(MAX_KEY_FILE_BYTES as usize + 1));
        (&file).take(MAX_KEY_FILE_BYTES + 1).read_to_end(&mut content).map_err(|e| {
            PrivateFileError::new(format!("the {KEY_FILE} {} cannot be read ({})", shown(path), e.kind()))
        })?;
        let malformed = || {
            PrivateFileError::new(format!(
                "the {KEY_FILE} {} is not a key made by `holzwave-server keygen`; make a new key file with that command",
                shown(path)
            ))
        };
        if content.len() as u64 > MAX_KEY_FILE_BYTES {
            return Err(malformed());
        }
        let bytes = parse_line(&content).ok_or_else(malformed)?;
        ServerKey::from_bytes(*bytes).ok_or_else(|| {
            PrivateFileError::new(format!(
                "the {KEY_FILE} {} holds the all-zero key; make a new key file with `holzwave-server keygen`",
                shown(path)
            ))
        })
    }

    /// The key for one purpose: HKDF-SHA256 with an empty salt and the info string `holzwave-server/v1/<purpose>`.
    pub fn derive(&self, purpose: Purpose) -> Zeroizing<[u8; KEY_BYTES]> {
        let hkdf = Hkdf::<Sha256>::new(None, &self.0[..]);
        let mut out = Zeroizing::new([0u8; KEY_BYTES]);
        hkdf.expand_multi_info(&[INFO_PREFIX.as_bytes(), purpose.name().as_bytes()], &mut out[..])
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }
}

/// The 32 bytes of a key line, or `None` when the content is not exactly `hws1:` and 43 base64url characters followed by
/// at most one newline.
fn parse_line(content: &[u8]) -> Option<Zeroizing<[u8; KEY_BYTES]>> {
    let line = content.strip_suffix(b"\n").unwrap_or(content);
    let encoded = line.strip_prefix(KEY_PREFIX.as_bytes())?;
    if encoded.len() != ENCODED_BYTES {
        return None;
    }
    // The decoder wants room for a whole last group; the length is checked after it.
    let mut out = Zeroizing::new([0u8; KEY_BYTES + 3]);
    let n = URL_SAFE_NO_PAD.decode_slice(encoded, &mut out[..]).ok()?;
    if n != KEY_BYTES {
        return None;
    }
    let mut bytes = Zeroizing::new([0u8; KEY_BYTES]);
    bytes.copy_from_slice(&out[..KEY_BYTES]);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(bytes: [u8; KEY_BYTES]) -> Vec<u8> {
        let mut line = KEY_PREFIX.as_bytes().to_vec();
        line.extend_from_slice(URL_SAFE_NO_PAD.encode(bytes).as_bytes());
        line
    }

    #[test]
    fn a_line_parses_with_or_without_the_closing_newline() {
        let mut line = line_of([7; KEY_BYTES]);
        assert_eq!(*parse_line(&line).expect("without newline"), [7; KEY_BYTES]);
        line.push(b'\n');
        assert_eq!(*parse_line(&line).expect("with newline"), [7; KEY_BYTES]);
        line.push(b'\n');
        assert!(parse_line(&line).is_none(), "a second newline is another line");
    }

    #[test]
    fn a_line_with_a_set_unused_bit_is_refused() {
        // 43 characters carry 258 bits; the last two must be zero. 'B' sets the lowest of them.
        let mut line = line_of([0; KEY_BYTES]);
        let last = line.len() - 1;
        line[last] = b'B';
        assert!(parse_line(&line).is_none());
    }
}

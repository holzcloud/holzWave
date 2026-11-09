//! Device credentials: the token a signed-in device presents, and the numbers that bound devices.
//!
//! A device token is 32 random bytes written as 43 base64url characters behind the prefix `hwd_`, the same shape as a
//! session value or an invitation. Only the SHA-256 of the 32 bytes is stored, as for a session: the token has 256 bits of
//! entropy, so a fast unkeyed hash is enough and a database that leaks holds nothing a person could present. The type wipes
//! its text when dropped, prints a marker instead of its text, and can neither be cloned nor serialised; the one accessor
//! that shows the text exists for the one answer that hands the token to its owner.

use std::fmt;

use zeroize::Zeroizing;

use crate::auth::session::{generate_token, parse_token};
use crate::store::TokenHash;

/// What a device token starts with.
pub const DEVICE_PREFIX: &str = "hwd_";

/// The most devices one account has at once (devices that have not expired).
pub const MAX_DEVICES_PER_ACCOUNT: usize = 20;

/// The longest device name that is kept, in characters.
pub const MAX_DEVICE_NAME: usize = 64;

/// The longest platform word that is kept, in characters.
pub const MAX_PLATFORM: usize = 32;

/// The fewest seconds between two writes of the activity of one device.
pub const TOUCH_INTERVAL: i64 = 60;

/// What the `Debug` and `Display` text of a device token print.
const TOKEN_MARKER: &str = "DeviceToken(..)";

/// Why a code could not be made or read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeError {
    /// The operating system gave no random bytes.
    Random,
    /// The text is not a code of this kind. It says nothing about why.
    Malformed,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodeError::Random => f.write_str("the operating system gave no random bytes"),
            CodeError::Malformed => f.write_str("the code is not valid"),
        }
    }
}

impl std::error::Error for CodeError {}

/// A device token in its written form and the hash that is stored for it.
pub struct DeviceToken {
    text: Zeroizing<String>,
    hash: TokenHash,
}

impl DeviceToken {
    /// A new token from the operating system's randomness.
    pub fn generate() -> Result<DeviceToken, CodeError> {
        let (body, bytes) = generate_token().map_err(|_| CodeError::Random)?;
        let mut text = Zeroizing::new(String::with_capacity(DEVICE_PREFIX.len() + body.expose().len()));
        text.push_str(DEVICE_PREFIX);
        text.push_str(body.expose());
        Ok(DeviceToken { text, hash: crate::store::hash_token(&bytes[..]) })
    }

    /// Reads a token a device presented. Anything but the prefix and exactly 43 characters of the base64url alphabet that
    /// decode to 32 bytes is refused, and so is a text that decodes to the same bytes in another spelling.
    pub fn parse(text: &str) -> Result<DeviceToken, CodeError> {
        let hash = parse_token(text.strip_prefix(DEVICE_PREFIX).ok_or(CodeError::Malformed)?)
            .map_err(|_| CodeError::Malformed)?;
        Ok(DeviceToken { text: Zeroizing::new(text.to_owned()), hash })
    }

    /// The SHA-256 of the 32 bytes: what the store keeps and looks up.
    pub fn hash(&self) -> TokenHash {
        self.hash
    }

    /// The token as the device keeps it. Call it only to hand the token to its owner, once.
    pub fn expose(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_MARKER)
    }
}

impl fmt::Display for DeviceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(TOKEN_MARKER)
    }
}

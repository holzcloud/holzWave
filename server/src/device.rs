//! Device credentials: the token a signed-in device presents, the codes of the device grant, and the numbers that bound them.
//!
//! A device token is 32 random bytes written as 43 base64url characters behind the prefix `hwd_`, the same shape as a
//! session value or an invitation. Only the SHA-256 of the 32 bytes is stored, as for a session: the token has 256 bits of
//! entropy, so a fast unkeyed hash is enough and a database that leaks holds nothing a person could present. The type wipes
//! its text when dropped, prints a marker instead of its text, and can neither be cloned nor serialised; the one accessor
//! that shows the text exists for the one answer that hands the token to its owner.
//!
//! A device that has no token yet asks for one with a grant: it gets a device code (`hwg_` plus 43 characters, the same
//! shape, the same redacting type) that only it knows, and a user code of eight letters that the person types or follows in
//! a browser that is signed in. The server stores only the hashes of both. The user code is drawn by rejection sampling, so
//! no letter is likelier than another, and the alphabet has no vowel (no word) and no look-alike (no `0` and `O`).

use std::fmt;

use zeroize::Zeroizing;

use crate::auth::session::{generate_token, parse_token};
use crate::store::{Grant, ThrottleRule, TokenHash, hash_token};
use crate::unlock::random;

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

/// What a device code starts with.
pub const GRANT_PREFIX: &str = "hwg_";

/// How long a grant lasts before the person must start again, in seconds: ten minutes.
pub const GRANT_TTL: i64 = 600;

/// The fewest seconds between two polls of one grant before the server says to slow down.
pub const POLL_INTERVAL: i64 = 5;

/// The longest poll interval a grant is slowed down to, in seconds.
pub const MAX_POLL_INTERVAL: i64 = 60;

/// The most grants that are open (not expired, not taken) at once, on the whole server.
pub const MAX_PENDING: usize = 200;

/// The most grants that are open at once from one client address.
pub const MAX_PENDING_PER_ADDR: usize = 5;

/// How long a device token works without the device being seen, in seconds: 90 days.
pub const DEVICE_IDLE_DEFAULT: i64 = 90 * 24 * 3_600;

/// The most devices a list of the administrator holds.
pub const MAX_LISTED_DEVICES: usize = 1_000;

/// How many wrong codes one account, and one address, may try within the window before every further try is refused: a wrong code
/// is an unknown, an expired or a malformed one. The same numbers count the lookups and decisions of a signed-in person and the
/// polls of an anonymous device, each under its own key. Five tries in ten minutes against 20^8 possible codes that live ten
/// minutes; the lock lasts as long as the window.
pub const GRANT_FAILURES: ThrottleRule = ThrottleRule { window_secs: 600, limit: 5, lock_secs: 600 };

/// The seconds a client is told to wait when a cap on open grants refuses a new one.
pub const GRANT_CAP_RETRY: u64 = 60;

/// The platform words a device may give. The list is closed; anything else is refused.
pub const PLATFORMS: [&str; 5] = ["macos", "windows", "linux", "android", "other"];

/// The letters of a user code: twenty consonants without the look-alikes of a vowel or a digit.
pub const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";

/// The letters of a user code.
pub const USER_CODE_LEN: usize = 8;

/// The longest text `normalise` looks at, in bytes. A person types eight letters and a dash; the rest is refused unread.
const MAX_USER_CODE_INPUT: usize = 64;

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

/// A prefixed random value in its written form and the hash that is stored for it. The text is wiped when dropped.
struct Secret {
    text: Zeroizing<String>,
    hash: TokenHash,
}

impl Secret {
    fn generate(prefix: &str) -> Result<Secret, CodeError> {
        let (body, bytes) = generate_token().map_err(|_| CodeError::Random)?;
        let mut text = Zeroizing::new(String::with_capacity(prefix.len() + body.expose().len()));
        text.push_str(prefix);
        text.push_str(body.expose());
        Ok(Secret { text, hash: hash_token(&bytes[..]) })
    }

    fn parse(prefix: &str, text: &str) -> Result<Secret, CodeError> {
        let body = text.strip_prefix(prefix).ok_or(CodeError::Malformed)?;
        let hash = parse_token(body).map_err(|_| CodeError::Malformed)?;
        Ok(Secret { text: Zeroizing::new(text.to_owned()), hash })
    }
}

/// A device token in its written form and the hash that is stored for it.
pub struct DeviceToken(Secret);

impl DeviceToken {
    /// A new token from the operating system's randomness.
    pub fn generate() -> Result<DeviceToken, CodeError> {
        Secret::generate(DEVICE_PREFIX).map(DeviceToken)
    }

    /// Reads a token a device presented. Anything but the prefix and exactly 43 characters of the base64url alphabet that
    /// decode to 32 bytes is refused, and so is a text that decodes to the same bytes in another spelling.
    pub fn parse(text: &str) -> Result<DeviceToken, CodeError> {
        Secret::parse(DEVICE_PREFIX, text).map(DeviceToken)
    }

    /// The SHA-256 of the 32 bytes: what the store keeps and looks up.
    pub fn hash(&self) -> TokenHash {
        self.0.hash
    }

    /// The token as the device keeps it. Call it only to hand the token to its owner, once.
    pub fn expose(&self) -> &str {
        &self.0.text
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

/// The device code of a grant in its written form: the secret only the asking device knows. Same shape and same rules as a
/// [`DeviceToken`], with the prefix `hwg_`.
pub struct GrantCode(Secret);

impl GrantCode {
    /// A new device code.
    pub fn generate() -> Result<GrantCode, CodeError> {
        Secret::generate(GRANT_PREFIX).map(GrantCode)
    }

    /// Reads a device code a device presented; the shape rules are those of [`DeviceToken::parse`].
    pub fn parse(text: &str) -> Result<GrantCode, CodeError> {
        Secret::parse(GRANT_PREFIX, text).map(GrantCode)
    }

    /// The SHA-256 of the 32 bytes: what the store keeps and looks up.
    pub fn hash(&self) -> TokenHash {
        self.0.hash
    }

    /// The code as the device keeps it. Call it only to hand the code to the device that asked, once.
    pub fn expose(&self) -> &str {
        &self.0.text
    }
}

impl fmt::Debug for GrantCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GrantCode(..)")
    }
}

impl fmt::Display for GrantCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GrantCode(..)")
    }
}

/// The user code of a grant: eight letters of [`USER_CODE_ALPHABET`], shown as `XXXX-XXXX`. It prints a marker, because it
/// names a pending sign-in.
#[derive(Clone, PartialEq, Eq)]
pub struct UserCode([u8; USER_CODE_LEN]);

impl UserCode {
    /// A new user code, by rejection sampling: a random byte is used only when it is below the largest multiple of 20 that
    /// fits in a byte, so every letter has the same chance (a plain modulo would favour the first sixteen letters).
    pub fn generate() -> Result<UserCode, CodeError> {
        const LIMIT: u8 = 240; // 12 * 20
        let mut letters = [0u8; USER_CODE_LEN];
        let mut filled = 0;
        // 32 bytes hold eight accepted ones with a probability that leaves this bound unreachable with a sound source of
        // randomness; a source that only gives rejected values fails here instead of looping.
        for _ in 0..16 {
            let draw = Zeroizing::new(random::<32>().map_err(|_| CodeError::Random)?);
            for &byte in draw.iter() {
                if byte < LIMIT {
                    letters[filled] = USER_CODE_ALPHABET[usize::from(byte) % USER_CODE_ALPHABET.len()];
                    filled += 1;
                    if filled == USER_CODE_LEN {
                        return Ok(UserCode(letters));
                    }
                }
            }
        }
        Err(CodeError::Random)
    }

    /// Reads what a person typed: upper-cased, every character that is not a letter of the alphabet dropped (a dash, a
    /// space, a vowel), and anything that is not exactly eight letters afterwards refused. A text of more than 64 bytes is
    /// refused unread.
    pub fn normalise(text: &str) -> Option<UserCode> {
        if text.len() > MAX_USER_CODE_INPUT {
            return None;
        }
        let mut letters = [0u8; USER_CODE_LEN];
        let mut filled = 0;
        for byte in text.bytes().map(|b| b.to_ascii_uppercase()) {
            if USER_CODE_ALPHABET.contains(&byte) {
                *letters.get_mut(filled)? = byte;
                filled += 1;
            }
        }
        (filled == USER_CODE_LEN).then_some(UserCode(letters))
    }

    /// The code as the person reads it: `XXXX-XXXX`.
    pub fn display(&self) -> String {
        let (first, second) = self.0.split_at(USER_CODE_LEN / 2);
        // The letters are ASCII by construction.
        format!("{}-{}", String::from_utf8_lossy(first), String::from_utf8_lossy(second))
    }

    /// The SHA-256 of the eight letters (without the dash): what the store keeps and looks up.
    pub fn hash(&self) -> TokenHash {
        hash_token(&self.0)
    }
}

impl fmt::Debug for UserCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UserCode(..)")
    }
}

/// What a poll of a grant is answered, by arithmetic on the stored times alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollVerdict {
    /// Nobody has decided yet.
    Pending,
    /// The device polled sooner than the interval allows; its interval goes up.
    SlowDown,
    /// A signed-in person approved; the token can be taken once.
    Approved,
    /// A signed-in person refused.
    Denied,
    /// The grant ran out of time.
    Expired,
}

/// What a poll at `now` is answered. Order: a grant past its time is `Expired` whatever else it holds; a poll sooner than the
/// grant's interval after the last one is `SlowDown`; then `Denied`, `Approved` and `Pending`. The caller records the poll
/// with [`crate::store::Tx::grant_poll_mark`] and, on `Approved`, takes the grant with [`crate::store::Tx::grant_take`].
pub fn poll_verdict(grant: &Grant, now: i64) -> PollVerdict {
    if now >= grant.expires_at {
        PollVerdict::Expired
    } else if grant.last_poll.is_some_and(|last| now - last < grant.poll_interval) {
        PollVerdict::SlowDown
    } else if grant.denied {
        PollVerdict::Denied
    } else if grant.approved_by.is_some() {
        PollVerdict::Approved
    } else {
        PollVerdict::Pending
    }
}

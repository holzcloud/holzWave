//! The wire between the vault of Rust and the Keystore code of Kotlin: the only place where untyped data from the Android
//! side becomes a typed value, and the one rule that decides when the app switcher card is hidden.
//!
//! Everything here is plain `serde_json::Value` in and out, compiled on every platform, with no Android code in it, so it is
//! tested on the desktop. The functions are strict and total: no input makes one panic, a size is checked before anything is
//! allocated for it, and an error never carries text that came from the other side. A platform message may be about the
//! device or hold something a person typed, so a failure is one of a few fixed words (the plugin's error `code`) and the text
//! that goes with it is written here, never received.

use holzwave_core::vault::{KeystoreCaps, KeystoreError, Protection, VaultStatus};
use serde_json::{Value, json};

/// The one text of every failure that is not one of the named words. It says nothing about the device or about the cause.
pub const FAILED_TEXT: &str = "the device key store failed";

/// The most bytes of the data key that cross the wire (the vault's data key is 32 bytes).
pub const MAX_KEY_BYTES: usize = 32;
/// The most bytes of a wrapped data key that cross the wire. The Kotlin side makes at most 64 (a key number, the IV, the
/// key and the tag); the bound leaves room and still refuses anything absurd before it is read.
pub const MAX_WRAPPED_BYTES: usize = 256;

/// The error of a Keystore call that the plugin rejected with the word `code`.
///
/// The words are `cancelled`, `key_gone`, `no_screen_lock`, `invalid` (the bytes were not made by this device key or were
/// changed) and `failed`. Every other word, an empty one, a very long one and one with control characters is `failed` with
/// the same fixed text, so what the platform said never reaches a log or the page.
pub fn keystore_error(code: &str) -> KeystoreError {
    match code {
        "cancelled" => KeystoreError::Cancelled,
        "key_gone" => KeystoreError::KeyGone,
        "no_screen_lock" => KeystoreError::NoScreenLock,
        "invalid" => KeystoreError::Invalid,
        _ => KeystoreError::Failed(FAILED_TEXT.to_owned()),
    }
}

fn failed() -> KeystoreError {
    KeystoreError::Failed(FAILED_TEXT.to_owned())
}

/// A byte string as the JSON array of numbers the plugin takes (0 to 255 each).
pub fn bytes_to_wire(bytes: &[u8]) -> Value {
    Value::Array(bytes.iter().map(|b| Value::from(*b)).collect())
}

/// A byte string read from the JSON array of numbers the plugin answers. Refuses anything that is not an array, an element
/// that is not a whole number from 0 to 255 (a fraction, a negative, a number too large, a string), and an array longer than
/// `max`, which is checked before anything is allocated.
pub fn bytes_from_wire(value: &Value, max: usize) -> Result<Vec<u8>, KeystoreError> {
    let Value::Array(items) = value else { return Err(failed()) };
    if items.len() > max {
        return Err(failed());
    }
    let mut bytes = Vec::with_capacity(items.len());
    for item in items {
        let Some(n) = item.as_u64() else { return Err(failed()) };
        let Ok(byte) = u8::try_from(n) else { return Err(failed()) };
        bytes.push(byte);
    }
    Ok(bytes)
}

/// What the device can do, from `{ "has_screen_lock": bool, "strongbox": bool }`. A missing or mistyped field is a failure,
/// never a default: a device is not assumed to have a lock or a secure chip.
pub fn caps_from_wire(value: &Value) -> Result<KeystoreCaps, KeystoreError> {
    let has_screen_lock = value.get("has_screen_lock").and_then(Value::as_bool).ok_or_else(failed)?;
    let strongbox = value.get("strongbox").and_then(Value::as_bool).ok_or_else(failed)?;
    Ok(KeystoreCaps { has_screen_lock, strongbox })
}

/// The name of a protection on the wire: `auth` or `plain`, the same names the vault's status uses.
pub fn protection_to_wire(protection: Protection) -> &'static str {
    protection.as_str()
}

/// The clock's answer, from `{ "ms": number }`: a whole number of milliseconds since the device started, suspended time
/// included. Anything else is a failure.
pub fn millis_from_wire(value: &Value) -> Result<u64, KeystoreError> {
    value.get("ms").and_then(Value::as_u64).ok_or_else(failed)
}

/// What the app switcher control answered: whether the card is hidden now, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentsAnswer {
    /// The system hides the card of the app in the app switcher.
    pub applied: bool,
    /// `system` (Android 13 and later, the framework call) or `unsupported` (Android 12 and 12L, where nothing hides it).
    pub mode: RecentsMode,
}

/// How the app switcher card is handled on this Android version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecentsMode {
    System,
    Unsupported,
}

impl RecentsMode {
    /// The word the plugin answers and the self-test logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RecentsMode::System => "system",
            RecentsMode::Unsupported => "unsupported",
        }
    }
}

/// The answer of the plugin's `recentsHidden`, from `{ "applied": bool, "mode": "system" | "unsupported" }`. `None` for
/// anything else, so a misread answer is a failed call that the watcher repeats.
pub fn recents_from_wire(value: &Value) -> Option<RecentsAnswer> {
    let applied = value.get("applied")?.as_bool()?;
    let mode = match value.get("mode")?.as_str()? {
        "system" => RecentsMode::System,
        "unsupported" => RecentsMode::Unsupported,
        _ => return None,
    };
    Some(RecentsAnswer { applied, mode })
}

/// The argument of the plugin's `recentsHidden`.
pub fn recents_to_wire(hidden: bool) -> Value {
    json!({ "hidden": hidden })
}

/// Whether the app switcher card must be hidden while the app is not in the foreground: exactly when the vault's protection
/// is `auth`, whatever its state. A protected vault that locked five minutes after the app went to the background has no card
/// either. Nothing but the protection is read, so the rule cannot depend on a secret, and whether the app is in the
/// foreground is not an input: the system applies the flag to the card only, so it has no effect while the app is in front.
pub fn recents_hidden(status: &VaultStatus) -> bool {
    status.protection == Protection::Auth
}

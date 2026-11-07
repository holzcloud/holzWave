//! The wire between the file picker seam of the host and the Kotlin code that shows the system's document dialogs: the only
//! place where an untyped answer of the Android side becomes a typed value for the hand-over.
//!
//! Everything here is plain `serde_json::Value` in and out, compiled on every platform, with no Android code in it, so it is
//! tested on the desktop. The functions are strict and total: no input makes one panic, a size is a whole number or the answer
//! is refused, and an error never carries text that came from the other side. A provider's words may name a path or an app,
//! so a failure is the one fixed [`PickError::Failed`], and the text that goes with it is written here, never received.
//!
//! The only received text that survives is the provider's display name of a file, cut to [`MAX_NAME_CHARS`] characters. It is
//! another app's text: the host's `clean_name` is the one place where it becomes a file name.
//!
//! An answer is an object tagged by `kind`:
//!
//! - `{ "kind": "done", "name": text, "size": number }` for an open (the name may be missing),
//! - `{ "kind": "done", "size": number }` for a save,
//! - `{ "kind": "cancelled" }`,
//! - `{ "kind": "too_large", "name": text, "size": number }` for an open, where each of the two may be missing.
//!
//! Any other field, any other kind and any mistyped value is a failure.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::seams::{PickError, PickedIn, PickedOut};

/// The text of every failure of the file dialog. It says nothing about the provider, the path or the cause.
pub const FAILED_TEXT: &str = "the file dialog failed";

/// The most characters of a provider's display name that leave this module (the host then cleans it to 255 bytes).
pub const MAX_NAME_CHARS: usize = 1024;

/// The request of the open dialog: the staging file the plugin writes to and the most bytes it may copy. `None` when the
/// path is not text (the plugin takes text only, and a path that is not text cannot be one the app made).
pub fn pick_in_to_wire(staging: &Path, max_bytes: u64) -> Option<Value> {
    Some(json!({ "staging_path": staging.to_str()?, "max_bytes": max_bytes }))
}

/// The request of the create-document dialog: the regular file to copy and the name offered in the dialog (the host cleaned
/// it). `None` when the path is not text.
pub fn save_out_to_wire(source: &Path, suggested_name: &str) -> Option<Value> {
    Some(json!({ "source_path": source.to_str()?, "suggested_name": suggested_name }))
}

/// The kind of an answer and the other fields, when the value is an object with a text `kind`.
fn fields(value: &Value) -> Option<(&str, &Map<String, Value>)> {
    let object = value.as_object()?;
    let kind = object.get("kind")?.as_str()?;
    Some((kind, object))
}

/// Whether the object holds no field but those in `allowed`. `kind` is always allowed.
fn only(object: &Map<String, Value>, allowed: &[&str]) -> bool {
    object.keys().all(|key| key == "kind" || allowed.contains(&key.as_str()))
}

/// The name field: `Ok(None)` when it is missing, the capped text when it is a string, an error otherwise.
fn name_of(object: &Map<String, Value>) -> Result<Option<String>, PickError> {
    match object.get("name") {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.chars().take(MAX_NAME_CHARS).collect())),
        Some(_) => Err(PickError::Failed),
    }
}

/// The size field: `Ok(None)` when it is missing, the whole number when it is one, an error otherwise (a fraction, a negative
/// number, a float, text, null).
fn size_of(object: &Map<String, Value>) -> Result<Option<u64>, PickError> {
    match object.get("size") {
        None => Ok(None),
        Some(number @ Value::Number(_)) => number.as_u64().map(Some).ok_or(PickError::Failed),
        Some(_) => Err(PickError::Failed),
    }
}

/// What an answer of the open dialog is: the document that was copied, or why there is none.
pub fn picked_in_from_wire(value: &Value) -> Result<PickedIn, PickError> {
    let Some((kind, object)) = fields(value) else { return Err(PickError::Failed) };
    match kind {
        "done" => {
            if !only(object, &["name", "size"]) {
                return Err(PickError::Failed);
            }
            let name = name_of(object)?.unwrap_or_default();
            let size = size_of(object)?.ok_or(PickError::Failed)?;
            Ok(PickedIn { name, size })
        }
        "cancelled" => {
            if only(object, &[]) { Err(PickError::Cancelled) } else { Err(PickError::Failed) }
        }
        "too_large" => {
            if !only(object, &["name", "size"]) {
                return Err(PickError::Failed);
            }
            let name = name_of(object)?.unwrap_or_default();
            let size = size_of(object)?;
            Err(PickError::TooLarge { name, size })
        }
        _ => Err(PickError::Failed),
    }
}

/// What an answer of the create-document dialog is: the bytes that were copied, or why nothing was.
pub fn picked_out_from_wire(value: &Value) -> Result<PickedOut, PickError> {
    let Some((kind, object)) = fields(value) else { return Err(PickError::Failed) };
    match kind {
        "done" => {
            if !only(object, &["size"]) {
                return Err(PickError::Failed);
            }
            let size = size_of(object)?.ok_or(PickError::Failed)?;
            Ok(PickedOut { size })
        }
        "cancelled" => {
            if only(object, &[]) { Err(PickError::Cancelled) } else { Err(PickError::Failed) }
        }
        _ => Err(PickError::Failed),
    }
}

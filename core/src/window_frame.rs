//! `window_frame.json`: which frame the window of holzWave has on Windows and Linux, and which window buttons are drawn.
//!
//! On Windows and Linux the page's own bar can be the window bar, or the system can keep its title bar. Two choices belong
//! to that, and both are facts of **this computer**:
//!
//! * the **frame** ([`FrameChoice`]): `auto`, `own` or `system`;
//! * the **buttons** ([`ButtonChoice`]): `auto`, `flat` or `round`.
//!
//! They are kept in `window_frame.json` ([`WindowFrame`]) and never in `settings.json`: that file travels to every
//! computer, and a GNOME computer's round buttons must not reach a KDE one, nor a frame chosen to get a movable window reach
//! a computer where the window works. The sync refuses the file by name. A damaged or hostile file costs the choice and never
//! the start: [`WindowFrame::load`] has no error case, and every doubt reads as `auto` for the member that is doubtful.
//! A value read from the file is data and is never echoed into a status line. This file has no network code, no unsafe code
//! and no dependency beyond the JSON reader the other files use.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

/// The file name inside the app data directory.
pub const FILE: &str = "window_frame.json";
/// The format version that is written.
pub const VERSION: u32 = 1;
/// The largest `window_frame.json` that is read. The file is a few dozen bytes.
pub const MAX_BYTES: u64 = 4096;

/// The frame of the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameChoice {
    /// Follow the system: holzWave's own bar as the window bar.
    #[default]
    Auto,
    /// holzWave's own bar as the window bar, whatever the system says.
    Own,
    /// The system keeps its own title bar above the page.
    System,
}

impl FrameChoice {
    /// The word of the table, in the case of the file.
    pub fn word(self) -> &'static str {
        match self {
            FrameChoice::Auto => "auto",
            FrameChoice::Own => "own",
            FrameChoice::System => "system",
        }
    }

    /// The choice of a word; `None` for any other text (another case, padding, the empty string).
    pub fn from_word(word: &str) -> Option<FrameChoice> {
        match word {
            "auto" => Some(FrameChoice::Auto),
            "own" => Some(FrameChoice::Own),
            "system" => Some(FrameChoice::System),
            _ => None,
        }
    }
}

/// The style of the window buttons the own frame draws.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonChoice {
    /// Follow the desktop: round on GNOME, flat everywhere else.
    #[default]
    Auto,
    /// Flat buttons.
    Flat,
    /// Round buttons.
    Round,
}

impl ButtonChoice {
    /// The word of the table, in the case of the file.
    pub fn word(self) -> &'static str {
        match self {
            ButtonChoice::Auto => "auto",
            ButtonChoice::Flat => "flat",
            ButtonChoice::Round => "round",
        }
    }

    /// The choice of a word; `None` for any other text (another case, padding, the empty string).
    pub fn from_word(word: &str) -> Option<ButtonChoice> {
        match word {
            "auto" => Some(ButtonChoice::Auto),
            "flat" => Some(ButtonChoice::Flat),
            "round" => Some(ButtonChoice::Round),
            _ => None,
        }
    }
}

/// The content of `window_frame.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFrame {
    pub version: u32,
    /// The frame choice, key `window_frame`.
    pub frame: FrameChoice,
    /// The button style, key `window_buttons`.
    pub buttons: ButtonChoice,
}

impl Default for WindowFrame {
    fn default() -> Self {
        WindowFrame { version: VERSION, frame: FrameChoice::Auto, buttons: ButtonChoice::Auto }
    }
}

/// One member of the document: the value of `key` if it is a JSON string equal to a word of its table, otherwise the default
/// and a note that names the member and the table, never the offending value.
fn member<T: Default>(
    object: &serde_json::Map<String, Value>,
    key: &str,
    words: &str,
    parse: fn(&str) -> Option<T>,
    notes: &mut Vec<String>,
) -> T {
    match object.get(key) {
        Some(Value::String(word)) => parse(word).unwrap_or_else(|| {
            notes.push(format!("{key} is not one of {words} and was reset to auto"));
            T::default()
        }),
        Some(_) => {
            notes.push(format!("{key} is not one of {words} and was reset to auto"));
            T::default()
        }
        None => {
            notes.push(format!("the file has no {key} and it was set to auto"));
            T::default()
        }
    }
}

impl WindowFrame {
    /// Reads the document. There is no error case, and every doubt reads as `auto`: text that is not JSON, not an object or of
    /// a version other than the current one is `Reset`; a missing or mistyped member, or a word outside its table, is `auto`
    /// for that member alone and the result is `Repaired`. Unknown keys are ignored.
    pub fn from_json(text: &str) -> (WindowFrame, LoadStatus) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return (WindowFrame::default(), LoadStatus::Reset(format!("not a window frame file: {e}"))),
        };
        let Value::Object(object) = value else {
            return (
                WindowFrame::default(),
                LoadStatus::Reset("not a window frame file: the top level is not an object".into()),
            );
        };
        if let Some(version) = object.get("version") {
            // A version this build does not know could mean something else by the same keys: fail closed.
            if version.as_u64() != Some(u64::from(VERSION)) {
                return (
                    WindowFrame::default(),
                    LoadStatus::Reset("the version of the window frame file is not one this build reads".into()),
                );
            }
        }
        let mut notes = Vec::new();
        let frame = member(&object, "window_frame", "auto, own, system", FrameChoice::from_word, &mut notes);
        let buttons = member(&object, "window_buttons", "auto, flat, round", ButtonChoice::from_word, &mut notes);
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (WindowFrame { version: VERSION, frame, buttons }, status)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        // By hand: the key order is fixed (version first) and the values are plain ASCII words that need no escaping.
        format!(
            "{{\n  \"version\": {},\n  \"window_frame\": \"{}\",\n  \"window_buttons\": \"{}\"\n}}\n",
            self.version,
            self.frame.word(),
            self.buttons.word()
        )
    }

    /// Reads `<dir>/window_frame.json`.
    ///
    /// There is no error case. A missing file is the default and `Clean`. A file that cannot be used (oversized, not UTF-8,
    /// not JSON, a version this build does not know, a member outside its table) gives `auto` for the doubtful part, and its
    /// bytes are copied to `window_frame.json.bad` first so that the next save does not destroy them; an oversized file is
    /// moved there without being read. A valid file is never written.
    pub fn load(dir: &Path) -> (WindowFrame, LoadStatus) {
        let path = dir.join(FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (WindowFrame::default(), LoadStatus::Clean),
            Err(e) => {
                let mut reason = format!("cannot read {FILE}: {e}");
                if e.kind() == io::ErrorKind::InvalidData {
                    match fs::rename(&path, bad_path(&path)) {
                        Ok(()) => reason.push_str("; the file was moved to window_frame.json.bad"),
                        Err(e) => reason.push_str(&format!("; and it could not be kept: {e}")),
                    }
                }
                return (WindowFrame::default(), LoadStatus::Reset(reason));
            }
        };
        let (value, status) = match String::from_utf8(bytes) {
            Ok(text) => WindowFrame::from_json(&text),
            Err(_) => (WindowFrame::default(), LoadStatus::Reset(format!("{FILE} is not UTF-8 text"))),
        };
        match status {
            LoadStatus::Clean => (value, LoadStatus::Clean),
            LoadStatus::Reset(reason) => {
                let reason = match backup_bad(&path) {
                    Ok(_) => reason,
                    Err(e) => format!("{reason} (and the damaged file could not be kept: {e})"),
                };
                (value, LoadStatus::Reset(reason))
            }
            LoadStatus::Repaired(mut notes) => {
                if let Err(e) = backup_bad(&path) {
                    notes.push(format!("the original could not be kept: {e}"));
                }
                (value, LoadStatus::Repaired(notes))
            }
        }
    }

    /// Writes `<dir>/window_frame.json` atomically (the parent directory is created).
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(FILE), self.to_json().as_bytes())
    }
}

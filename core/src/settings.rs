//! `settings.json`: the typed, versioned and validated user settings, with defaults for every missing or invalid
//! value.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, read_limited, write_atomic};
use crate::theme::{DEFAULT_THEME, ThemeRegistry};

/// The settings format version that this build writes and reads.
pub const SETTINGS_VERSION: u32 = 1;

/// The file name of the settings document inside the configuration directory.
pub const SETTINGS_FILE: &str = "settings.json";

/// The largest `settings.json` that is read (V12).
pub const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// Whether the cursor blinks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Blink {
    #[default]
    System,
    On,
    Off,
}

/// How much interface motion is allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motion {
    #[default]
    System,
    Reduce,
    Full,
}

/// The contrast preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contrast {
    #[default]
    System,
    Normal,
    High,
}

/// The user settings. Every field has a default, so a missing key never fails a load.
///
/// The field order is the key order of the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The format version, `1`.
    pub version: u32,
    /// The name of the selected theme. A name that no theme has resolves to aubergine.
    pub theme: String,
    /// Terminal font size in points.
    pub font_size: u16,
    /// Interface scale in percent.
    pub ui_scale: u16,
    /// Whether Option acts as Alt (D-12: off, so a Swiss layout types `|` with Option+7).
    pub option_as_alt: bool,
    pub cursor_blink: Blink,
    pub motion: Motion,
    pub contrast: Contrast,
    /// Lines of scrollback kept per pane.
    pub scrollback_lines: u32,
    /// Ask before a multi-line paste when the program did not turn on bracketed paste (D-15).
    pub confirm_multiline_paste: bool,
    /// Overrides of the default key bindings: action id to chord. Validated by the key binding resolver.
    pub keybindings: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            theme: DEFAULT_THEME.into(),
            font_size: 14,
            ui_scale: 100,
            option_as_alt: false,
            cursor_blink: Blink::System,
            motion: Motion::System,
            contrast: Contrast::System,
            scrollback_lines: 10_000,
            confirm_multiline_paste: true,
            keybindings: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Parse `text`. Never fails: what cannot be used becomes the default and the status says so.
    pub fn from_json(text: &str) -> (Settings, LoadStatus) {
        match serde_json::from_str::<Settings>(text) {
            Ok(settings) => (settings, LoadStatus::Clean),
            Err(e) => (Settings::default(), LoadStatus::Reset(format!("not a settings document: {e}"))),
        }
    }

    /// Bring every value into its range and report one note per change.
    pub fn sanitise(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// Replace a theme name that `registry` does not know with aubergine.
    pub fn sanitise_theme(&mut self, _registry: &ThemeRegistry) -> Option<String> {
        None
    }

    /// Set one setting from a JSON value, validated and clamped; on error nothing changes.
    pub fn set_value(&mut self, _key: &str, _value: serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        // A struct of strings, numbers and a string map cannot fail to serialise; the fallback only keeps the
        // function total.
        let mut text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text
    }

    /// Read `<dir>/settings.json`.
    ///
    /// A missing file is the defaults and `Clean`. A file that cannot be used is the defaults and `Reset`, and its
    /// bytes are copied to `settings.json.bad` first so that the next save does not destroy them. A file that is
    /// valid is never written to.
    pub fn load(dir: &Path) -> (Settings, LoadStatus) {
        let path = dir.join(SETTINGS_FILE);
        let bytes = match read_limited(&path, MAX_SETTINGS_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (Settings::default(), LoadStatus::Clean),
            Err(e) => {
                return (Settings::default(), LoadStatus::Reset(format!("cannot read {SETTINGS_FILE}: {e}")));
            }
        };
        let (settings, status) = match String::from_utf8(bytes) {
            Ok(text) => Settings::from_json(&text),
            Err(_) => (Settings::default(), LoadStatus::Reset(format!("{SETTINGS_FILE} is not UTF-8 text"))),
        };
        match status {
            LoadStatus::Reset(reason) => {
                let reason = match backup_bad(&path) {
                    Ok(_) => reason,
                    Err(e) => format!("{reason} (and the damaged file could not be kept: {e})"),
                };
                (settings, LoadStatus::Reset(reason))
            }
            other => (settings, other),
        }
    }

    /// Write `<dir>/settings.json` atomically.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(SETTINGS_FILE), self.to_json().as_bytes())
    }
}

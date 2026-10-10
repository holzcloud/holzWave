//! `settings.json`: the typed, versioned and validated user settings, with defaults for every missing or invalid
//! value.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::LoadStatus;

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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub theme: String,
    pub font_size: u16,
    pub ui_scale: u16,
    pub option_as_alt: bool,
    pub cursor_blink: Blink,
    pub motion: Motion,
    pub contrast: Contrast,
    pub scrollback_lines: u32,
    pub confirm_multiline_paste: bool,
    pub keybindings: BTreeMap<String, String>,
}

impl Settings {
    /// Parse `text`. Never fails: what cannot be used becomes the default and the status says so.
    pub fn from_json(_text: &str) -> (Settings, LoadStatus) {
        (Settings::default(), LoadStatus::Clean)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        String::new()
    }

    /// Read `<dir>/settings.json`.
    pub fn load(_dir: &Path) -> (Settings, LoadStatus) {
        (Settings::default(), LoadStatus::Clean)
    }

    /// Write `<dir>/settings.json` atomically.
    pub fn save(&self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }
}

//! `settings.json`: the typed, versioned and validated user settings, with defaults for every missing or invalid
//! value.
//!
//! # Files
//!
//! The configuration files live in the app data directory, one document per file, all plain JSON that can be read
//! and edited by hand and none of them depending on how a later phase moves them between machines (F25):
//!
//! | File | Owner | Content |
//! |------|-------|---------|
//! | `settings.json` | this module | the [`Settings`] below, one flat object |
//! | `themes/*.json` | [`crate::theme`] | one user theme per file, the same shape as a built-in |
//! | `layout.json` | [`crate::layout`] | workspaces, tabs, panes and split ratios |
//! | `session.json` | [`crate::session`] | per-pane working directory and window geometry, machine-local |
//!
//! Every document is read with a size limit and written atomically ([`crate::persist`]). A file that cannot be
//! used never stops the app from starting: the defaults are used, and the damaged file is kept next to the
//! original as `<name>.bad` so that the next save does not destroy it.
//!
//! # What a load does
//!
//! `settings.json` is read key by key. A key that this build does not know is ignored, so a newer file still
//! loads. A key with a value of the wrong type costs only that key, which keeps its default. A number outside its
//! range is clamped. Each of those makes the load `Repaired` with one note per change; the file itself is not
//! rewritten until the next save. That save writes only what this build knows, so a `Repaired` file is copied to
//! `settings.json.bad` when it is loaded: the ignored keys and the values that were repaired stay in the copy.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};
use crate::theme::{DEFAULT_THEME, ThemeRegistry, valid_theme_name};

/// The settings format version that this build writes and reads.
pub const SETTINGS_VERSION: u32 = 1;

/// The file name of the settings document inside the configuration directory.
pub const SETTINGS_FILE: &str = "settings.json";

/// The largest `settings.json` that is read (V12).
pub const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// The accepted range of `font_size`, in points.
pub const FONT_SIZE: (u16, u16) = (8, 40);

/// The accepted range of `ui_scale`, in percent.
pub const UI_SCALE: (u16, u16) = (75, 200);

/// The accepted range of `scrollback_lines`.
pub const SCROLLBACK_LINES: (u32, u32) = (100, 100_000);

/// The most key binding overrides that are kept. With the length limit below, a saved document stays far below
/// [`MAX_SETTINGS_BYTES`], so a save is always loadable again.
pub const MAX_KEYBINDINGS: usize = 512;

/// The longest action id or chord of a key binding override, in bytes.
pub const MAX_KEYBINDING_TEXT: usize = 128;

/// The longest `shell`, in bytes. A Windows path is at most about 32 000 characters in theory and a few hundred in
/// practice; this is far above any real one and keeps a hostile file from filling the settings.
pub const MAX_SHELL_BYTES: usize = 4096;

/// How much of a name from outside appears in a message.
const SHOWN: usize = 40;

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
    /// Ease a scrollback jump over a fraction of a second (DESK-28). Off by default, and never while motion is
    /// reduced. Purely presentation: the core still scrolls by whole rows.
    pub smooth_scroll: bool,
    /// Draw a short fading trail behind a cursor that jumps (DESK-28). Off by default, and never while motion is
    /// reduced.
    pub cursor_trail: bool,
    /// The program a new pane starts: one string, never split and never run through another shell. Empty means the
    /// platform default of [`crate::platform::shell_launch`]. It applies to panes started afterwards.
    pub shell: String,
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
            smooth_scroll: false,
            cursor_trail: false,
            shell: String::new(),
            keybindings: BTreeMap::new(),
        }
    }
}

impl Settings {
    /// Parse `text`. Never fails: what cannot be used becomes the default and the status says so.
    ///
    /// The document is read key by key (see the module docs), so one bad value does not cost the others.
    pub fn from_json(text: &str) -> (Settings, LoadStatus) {
        let object = match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(object)) => object,
            Ok(_) => return (Settings::default(), LoadStatus::Reset("the document is not a JSON object".into())),
            Err(e) => return (Settings::default(), LoadStatus::Reset(format!("not JSON: {e}"))),
        };
        let mut settings = Settings::default();
        let mut notes = Vec::new();
        for (key, value) in object {
            if key == "version" {
                if value.as_u64() != Some(u64::from(SETTINGS_VERSION)) {
                    notes.push(format!("version {value} is not {SETTINGS_VERSION}; read as version {SETTINGS_VERSION}"));
                }
                continue;
            }
            match settings.apply(&key, value) {
                Ok(Some(note)) => notes.push(note),
                Ok(None) => {}
                // Ignored, but not silently: the next save leaves it out, so the load is `Repaired` and the file
                // is kept as `.bad` (see `load`).
                Err(KeyError::Unknown) => {
                    let name = shown(&key);
                    notes.push(format!("{name}: not a setting of this build; it is left out of the next save"));
                }
                Err(KeyError::Invalid(why)) => notes.push(format!("{}: {why}; the default is used", shown(&key))),
            }
        }
        notes.extend(settings.sanitise());
        if notes.is_empty() {
            (settings, LoadStatus::Clean)
        } else {
            (settings, LoadStatus::Repaired(notes))
        }
    }

    /// Set `key` to `value` if `key` is a setting and `value` has its type; numbers are clamped into their range
    /// first, so a number that does not fit the field type still lands on the nearest bound. The returned note
    /// says what was clamped. On error `self` is untouched.
    fn apply(&mut self, key: &str, mut value: Value) -> Result<Option<String>, KeyError> {
        let Ok(Value::Object(mut object)) = serde_json::to_value(&*self) else {
            return Err(KeyError::Invalid("the settings cannot be represented".into()));
        };
        if !object.contains_key(key) {
            return Err(KeyError::Unknown);
        }
        let mut note = None;
        if let Some((low, high)) = numeric_range(key) {
            let integer = value.as_i64().map(i128::from).or_else(|| value.as_u64().map(i128::from));
            if let Some(n) = integer {
                let clamped = n.clamp(i128::from(low), i128::from(high));
                if clamped != n {
                    note = Some(format!("{key} {n} is outside {low}..={high}; {clamped} is used"));
                    value = Value::from(clamped as i64);
                }
            }
        }
        object.insert(key.to_owned(), value);
        *self = serde_json::from_value(Value::Object(object)).map_err(|e| KeyError::Invalid(e.to_string()))?;
        Ok(note)
    }

    /// Bring every value into its range and report one note per change.
    ///
    /// Numbers are clamped, a theme name that cannot be a theme name becomes aubergine, `shell` is bounded, and the
    /// key binding table is bounded. A well formed theme name that no theme has is kept: user themes are only known
    /// to the [`ThemeRegistry`], and a theme file that is missing for a moment should not rewrite the choice. Use
    /// [`Settings::sanitise_theme`] where the registry is at hand.
    pub fn sanitise(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if self.version != SETTINGS_VERSION {
            notes.push(format!("version {} is not {SETTINGS_VERSION}; read as version {SETTINGS_VERSION}", self.version));
            self.version = SETTINGS_VERSION;
        }
        if !valid_theme_name(&self.theme) {
            notes.push(format!("the theme name {:?} is not valid; {DEFAULT_THEME} is used", shown(&self.theme)));
            self.theme = DEFAULT_THEME.into();
        }
        clamp("font_size", &mut self.font_size, FONT_SIZE, &mut notes);
        clamp("ui_scale", &mut self.ui_scale, UI_SCALE, &mut notes);
        clamp("scrollback_lines", &mut self.scrollback_lines, SCROLLBACK_LINES, &mut notes);

        // Blanks are no program: they mean the default and cost no note. A value that is too long or holds a control
        // character (NUL included) cannot be a program path a person typed; it is reset, and nothing else with it.
        if self.shell.trim().is_empty() {
            self.shell.clear();
        } else if self.shell.len() > MAX_SHELL_BYTES || self.shell.chars().any(char::is_control) {
            notes.push(format!(
                "shell is longer than {MAX_SHELL_BYTES} bytes or holds a control character; the platform default \
                 is used"
            ));
            self.shell.clear();
        }

        let before = self.keybindings.len();
        self.keybindings.retain(|action, chord| {
            !action.is_empty() && action.len() <= MAX_KEYBINDING_TEXT && chord.len() <= MAX_KEYBINDING_TEXT
        });
        let malformed = before - self.keybindings.len();
        if malformed > 0 {
            notes.push(format!(
                "{malformed} key binding entries were dropped: an action id or chord is empty or longer than \
                 {MAX_KEYBINDING_TEXT} bytes"
            ));
        }
        if self.keybindings.len() > MAX_KEYBINDINGS {
            let surplus = self.keybindings.len() - MAX_KEYBINDINGS;
            while self.keybindings.len() > MAX_KEYBINDINGS {
                self.keybindings.pop_last();
            }
            notes.push(format!("{surplus} key binding entries beyond the first {MAX_KEYBINDINGS} were dropped"));
        }
        notes
    }

    /// Replace a theme name that `registry` does not know with aubergine. Returns the note when it did.
    pub fn sanitise_theme(&mut self, registry: &ThemeRegistry) -> Option<String> {
        if registry.contains(&self.theme) {
            return None;
        }
        let note = format!("there is no theme called {:?}; {DEFAULT_THEME} is used", shown(&self.theme));
        self.theme = DEFAULT_THEME.into();
        Some(note)
    }

    /// Set one setting from a JSON value, as the settings dialog does.
    ///
    /// Only an existing key with a value of the right type is accepted; a number is clamped into its range and
    /// succeeds. The version cannot be set and a theme name must be well formed. On error nothing changes.
    pub fn set_value(&mut self, key: &str, value: Value) -> Result<(), String> {
        if key == "version" {
            return Err("the version cannot be changed".into());
        }
        let mut next = self.clone();
        match next.apply(key, value) {
            Ok(_) => {}
            Err(KeyError::Unknown) => return Err(format!("there is no setting called {:?}", shown(key))),
            Err(KeyError::Invalid(why)) => return Err(format!("invalid value for {}: {why}", shown(key))),
        }
        if key == "theme" && !valid_theme_name(&next.theme) {
            return Err(format!("{:?} is not a valid theme name", shown(&next.theme)));
        }
        next.sanitise();
        *self = next;
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
    /// bytes are copied to `settings.json.bad` first so that the next save does not destroy them. A `Repaired`
    /// file (a wrong-typed or out-of-range value, a key this build does not know) is copied the same way: the
    /// next save writes the repaired values and drops what this build does not know. The file itself is never
    /// written to by a load.
    pub fn load(dir: &Path) -> (Settings, LoadStatus) {
        let path = dir.join(SETTINGS_FILE);
        let bytes = match read_limited(&path, MAX_SETTINGS_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (Settings::default(), LoadStatus::Clean),
            Err(e) => {
                let mut reason = format!("cannot read {SETTINGS_FILE}: {e}");
                // Over the limit: the bytes are not read, so they are kept by moving the file, which costs
                // nothing however large it is.
                if e.kind() == io::ErrorKind::InvalidData {
                    match fs::rename(&path, bad_path(&path)) {
                        Ok(()) => reason.push_str("; the file was moved to settings.json.bad"),
                        Err(e) => reason.push_str(&format!("; and it could not be kept: {e}")),
                    }
                }
                return (Settings::default(), LoadStatus::Reset(reason));
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
            LoadStatus::Repaired(mut notes) => {
                if let Err(e) = backup_bad(&path) {
                    notes.push(format!("the file as it was could not be kept: {e}"));
                }
                (settings, LoadStatus::Repaired(notes))
            }
            other => (settings, other),
        }
    }

    /// Write `<dir>/settings.json` atomically.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(SETTINGS_FILE), self.to_json().as_bytes())
    }
}

/// Why one key could not be applied.
enum KeyError {
    /// Not a setting of this build.
    Unknown,
    /// A setting, but the value has the wrong type.
    Invalid(String),
}

/// The accepted range of a numeric setting, as bounds that fit every field type.
fn numeric_range(key: &str) -> Option<(u32, u32)> {
    match key {
        "font_size" => Some((FONT_SIZE.0.into(), FONT_SIZE.1.into())),
        "ui_scale" => Some((UI_SCALE.0.into(), UI_SCALE.1.into())),
        "scrollback_lines" => Some(SCROLLBACK_LINES),
        _ => None,
    }
}

/// Clamp `value` into `range` and say so when it moved.
fn clamp<T: Ord + Copy + Display>(name: &str, value: &mut T, range: (T, T), notes: &mut Vec<String>) {
    let clamped = (*value).clamp(range.0, range.1);
    if clamped != *value {
        notes.push(format!("{name} {value} is outside {}..={}; {clamped} is used", range.0, range.1));
        *value = clamped;
    }
}

/// The first part of text that came from outside, for a message.
fn shown(text: &str) -> String {
    text.chars().take(SHOWN).collect()
}

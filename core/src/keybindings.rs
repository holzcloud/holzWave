//! Key bindings: the action ids, the chord grammar, the defaults per platform, and validation of user overrides.
//!
//! One chord has one canonical text form, `Cmd+Shift+D`: the modifiers in the fixed order Cmd, Ctrl, Alt, Shift and
//! then the key, letters in upper case. The webview builds that string from a `KeyboardEvent`, the keybinding
//! editor writes it to `settings.json`, and [`Chord::parse`] reads it back, so all three agree on one spelling.
//!
//! std only. The defaults are the macOS set; [`actions`] is the single place where another platform will pick its
//! own table (Phase 2.2).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

/// The named (non-character) keys of the grammar, in their canonical spelling. `F1` to `F24` are handled apart.
const NAMED_KEYS: &[&str] = &[
    "Space",
    "Enter",
    "Tab",
    "Escape",
    "Backspace",
    "Delete",
    "Insert",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
];

/// Other spellings that are read as a named key.
const KEY_ALIASES: &[(&str, &str)] = &[("return", "Enter"), ("esc", "Escape")];

/// The highest function key of the grammar.
const MAX_FUNCTION_KEY: u8 = 24;

/// One key combination: the modifiers that are held and the key.
///
/// `key` is always in canonical form (see [`Chord::parse`]); a value built by hand with another spelling does not
/// compare equal to the parsed one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The Command key (the Super or Windows key on other platforms).
    pub cmd: bool,
    pub ctrl: bool,
    /// The Alt key, called Option on macOS.
    pub alt: bool,
    pub shift: bool,
    /// A single character such as `D`, `]` or `1`, or one of the named keys `Space`, `Enter`, `Tab`, `Escape`,
    /// `Backspace`, `Delete`, `Insert`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown` and
    /// `F1` to `F24`.
    pub key: String,
}

impl Chord {
    /// Read a chord from its text form.
    ///
    /// Parts are separated by `+`; every part but the last is a modifier (`Cmd`, `Command`, `Meta`, `Super`,
    /// `Ctrl`, `Control`, `Alt`, `Option`, `Shift`, any case, each at most once) and the last is the key: one
    /// printable character, or a named key in any case. `+` itself is not a key of the grammar (it is `Shift+=`
    /// on most layouts), which keeps the separator unambiguous.
    ///
    /// The error says what is wrong in a plain sentence.
    pub fn parse(text: &str) -> Result<Chord, String> {
        if text.trim().is_empty() {
            return Err("the key combination is empty".into());
        }
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("{:?} has an empty part between its plus signs", shown(text)));
        }
        let (key_part, modifier_parts) = parts.split_last().expect("split of a non-empty text has a last part");
        let mut chord = Chord { cmd: false, ctrl: false, alt: false, shift: false, key: String::new() };
        for part in modifier_parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "cmd" | "command" | "meta" | "super" => &mut chord.cmd,
                "ctrl" | "control" => &mut chord.ctrl,
                "alt" | "option" | "opt" => &mut chord.alt,
                "shift" => &mut chord.shift,
                _ => return Err(format!("{:?} is not a modifier (Cmd, Ctrl, Alt or Shift)", shown(part))),
            };
            if *flag {
                return Err(format!("the modifier {:?} appears twice", shown(part)));
            }
            *flag = true;
        }
        chord.key = parse_key(key_part)?;
        Ok(chord)
    }

    /// How many of the four modifiers are held.
    pub fn modifier_count(&self) -> usize {
        [self.cmd, self.ctrl, self.alt, self.shift].iter().filter(|m| **m).count()
    }
}

/// The canonical spelling of a key, or why the text is not a key.
fn parse_key(text: &str) -> Result<String, String> {
    let lower = text.to_ascii_lowercase();
    if let Some(name) = NAMED_KEYS.iter().find(|n| n.to_ascii_lowercase() == lower) {
        return Ok((*name).to_string());
    }
    if let Some((_, name)) = KEY_ALIASES.iter().find(|(alias, _)| *alias == lower) {
        return Ok((*name).to_string());
    }
    if let Some(digits) = lower.strip_prefix('f') {
        // `F01` is refused so that every function key has exactly one spelling.
        let plain = !digits.is_empty() && !digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit());
        let number = digits.parse::<u8>().ok().filter(|n| (1..=MAX_FUNCTION_KEY).contains(n));
        if let (true, Some(n)) = (plain, number) {
            return Ok(format!("F{n}"));
        }
    }
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_control() || c.is_whitespace() => {
            Err(format!("{:?} is not a printable key; write Space, Tab or Enter by name", c.to_string()))
        }
        (Some(c), None) => Ok(upper(c)),
        _ => Err(format!("{:?} is not a key (one character or a name such as Enter or F5)", shown(text))),
    }
}

/// A letter in upper case when that is a single character; anything else as it is.
fn upper(c: char) -> String {
    let mut up = c.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) => u.to_string(),
        _ => c.to_string(),
    }
}

/// The first part of text that came from outside, for a message.
fn shown(text: &str) -> String {
    text.chars().take(40).collect()
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [(self.cmd, "Cmd"), (self.ctrl, "Ctrl"), (self.alt, "Alt"), (self.shift, "Shift")] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key)
    }
}

/// One user-visible action.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionInfo {
    /// The stable id used in `settings.json`, in the command palette and by the frontend: `pane.split_right`.
    pub id: &'static str,
    /// A short English label for the command palette and the editor.
    pub title: &'static str,
    /// The default chord in canonical form, or `None` for an action that is unbound until the user binds it.
    pub default: Option<&'static str>,
}

const fn action(id: &'static str, title: &'static str, default: &'static str) -> ActionInfo {
    ActionInfo { id, title, default: Some(default) }
}

/// The macOS default table. Order is the order of the editor.
static MACOS_ACTIONS: [ActionInfo; 33] = [
    action("pane.split_right", "Split pane right", "Cmd+D"),
    action("pane.split_down", "Split pane down", "Cmd+Shift+D"),
    action("pane.close", "Close pane", "Cmd+W"),
    action("pane.focus_next", "Focus next pane", "Cmd+]"),
    action("pane.focus_prev", "Focus previous pane", "Cmd+["),
    action("tab.new", "New tab", "Cmd+T"),
    action("tab.select_1", "Select tab 1", "Cmd+1"),
    action("tab.select_2", "Select tab 2", "Cmd+2"),
    action("tab.select_3", "Select tab 3", "Cmd+3"),
    action("tab.select_4", "Select tab 4", "Cmd+4"),
    action("tab.select_5", "Select tab 5", "Cmd+5"),
    action("tab.select_6", "Select tab 6", "Cmd+6"),
    action("tab.select_7", "Select tab 7", "Cmd+7"),
    action("tab.select_8", "Select tab 8", "Cmd+8"),
    action("tab.select_9", "Select tab 9", "Cmd+9"),
    action("workspace.new", "New workspace", "Cmd+Shift+N"),
    action("workspace.select_1", "Select workspace 1", "Cmd+Alt+1"),
    action("workspace.select_2", "Select workspace 2", "Cmd+Alt+2"),
    action("workspace.select_3", "Select workspace 3", "Cmd+Alt+3"),
    action("workspace.select_4", "Select workspace 4", "Cmd+Alt+4"),
    action("workspace.select_5", "Select workspace 5", "Cmd+Alt+5"),
    action("workspace.select_6", "Select workspace 6", "Cmd+Alt+6"),
    action("workspace.select_7", "Select workspace 7", "Cmd+Alt+7"),
    action("workspace.select_8", "Select workspace 8", "Cmd+Alt+8"),
    action("workspace.select_9", "Select workspace 9", "Cmd+Alt+9"),
    action("palette.open", "Open command palette", "Cmd+K"),
    action("search.open", "Search in scrollback", "Cmd+F"),
    action("copy_mode.enter", "Enter copy mode", "Cmd+Shift+X"),
    action("hints.open", "Show hints", "Cmd+Shift+Space"),
    action("font.increase", "Increase font size", "Cmd+="),
    action("font.decrease", "Decrease font size", "Cmd+-"),
    action("font.reset", "Reset font size", "Cmd+0"),
    action("settings.open", "Open settings", "Cmd+,"),
];

/// Every action of this build with its default chord. The one place that chooses the default table for the
/// platform: today the macOS set, Phase 2.2 adds the others here.
pub fn actions() -> &'static [ActionInfo] {
    &MACOS_ACTIONS
}

/// One row of the key binding editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingRow {
    pub action: &'static str,
    pub title: &'static str,
    /// The chord in canonical form, or `None` when the action is unbound.
    pub chord: Option<String>,
    /// Whether the action has the chord of the default table (including "no chord" for an action without one).
    pub is_default: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    info: &'static ActionInfo,
    chord: Option<Chord>,
    is_default: bool,
}

/// The resolved binding table: for each action the chord that is in force, and the reverse lookup the matcher
/// uses on every key press.
#[derive(Clone, Debug)]
pub struct Keybindings {
    /// In the order of [`actions`].
    entries: Vec<Entry>,
    by_chord: HashMap<Chord, usize>,
}

impl Default for Keybindings {
    fn default() -> Self {
        Keybindings::resolve(&BTreeMap::new()).0
    }
}

impl Keybindings {
    /// Apply the `overrides` of `settings.json` (action id to chord text, as `Settings::keybindings` holds them)
    /// to the default table. Returns the table and one plain sentence per override that could not be used.
    pub fn resolve(overrides: &BTreeMap<String, String>) -> (Keybindings, Vec<String>) {
        let warnings = Vec::new();
        let mut chords: Vec<Option<Chord>> =
            actions().iter().map(|a| a.default.and_then(|d| Chord::parse(d).ok())).collect();
        for (action, text) in overrides {
            let Some(index) = actions().iter().position(|a| a.id == action) else { continue };
            if text.is_empty() {
                chords[index] = None;
            } else if let Ok(chord) = Chord::parse(text) {
                chords[index] = Some(chord);
            }
        }
        (Keybindings::from_chords(chords), warnings)
    }

    fn from_chords(chords: Vec<Option<Chord>>) -> Keybindings {
        let mut entries = Vec::with_capacity(chords.len());
        let mut by_chord = HashMap::new();
        for (index, (info, chord)) in actions().iter().zip(chords).enumerate() {
            let default = info.default.and_then(|d| Chord::parse(d).ok());
            if let Some(c) = &chord {
                by_chord.insert(c.clone(), index);
            }
            entries.push(Entry { info, is_default: chord == default, chord });
        }
        Keybindings { entries, by_chord }
    }

    /// The chord in force for `action`; `None` for an unknown action and for one that is unbound.
    pub fn chord_of(&self, action: &str) -> Option<&Chord> {
        self.entries.iter().find(|e| e.info.id == action).and_then(|e| e.chord.as_ref())
    }

    /// The action a pressed `chord` triggers, if any.
    pub fn action_of(&self, chord: &Chord) -> Option<&'static str> {
        self.by_chord.get(chord).map(|&i| self.entries[i].info.id)
    }

    /// One row per action in the order of [`actions`], for the editor.
    pub fn table(&self) -> Vec<BindingRow> {
        self.entries
            .iter()
            .map(|e| BindingRow {
                action: e.info.id,
                title: e.info.title,
                chord: e.chord.as_ref().map(ToString::to_string),
                is_default: e.is_default,
            })
            .collect()
    }
}

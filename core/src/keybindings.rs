//! Key bindings: the action ids, the chord grammar, the defaults per platform, and validation of user overrides.

use std::collections::BTreeMap;
use std::fmt;

/// One key combination: the modifiers that are held and the key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl Chord {
    pub fn parse(_text: &str) -> Result<Chord, String> {
        Err("not implemented".into())
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

/// One user-visible action.
#[derive(Debug)]
pub struct ActionInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub default: Option<&'static str>,
}

pub fn actions() -> &'static [ActionInfo] {
    &[]
}

/// One row of the key binding editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingRow {
    pub action: &'static str,
    pub title: &'static str,
    pub chord: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Default)]
pub struct Keybindings;

impl Keybindings {
    pub fn resolve(_overrides: &BTreeMap<String, String>) -> (Keybindings, Vec<String>) {
        (Keybindings, Vec::new())
    }

    pub fn chord_of(&self, _action: &str) -> Option<&Chord> {
        None
    }

    pub fn action_of(&self, _chord: &Chord) -> Option<&'static str> {
        None
    }

    pub fn table(&self) -> Vec<BindingRow> {
        Vec::new()
    }
}

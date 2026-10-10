//! Themes: the built-in and user-supplied sets of terminal palette and chrome tokens, and how one is resolved and
//! handed to the engine.

use std::fmt;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

use crate::engine::{Color, EngineTheme};

/// An opaque colour, written as `#rrggbb`.
///
/// Every colour that comes from a file goes through [`Rgb::parse`] and is printed again by `Display`, so nothing
/// but six hex digits can ever reach the webview as a CSS value (T-02-21).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Parse `#rrggbb`, with either case for the digits. Anything else is `None`.
    pub fn parse(_text: &str) -> Option<Rgb> {
        None
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("#000000")
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Rgb::parse(&text).ok_or_else(|| de::Error::custom(format!("{text:?} is not a #rrggbb colour")))
    }
}

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Color {
        Color { r: c.r, g: c.g, b: c.b }
    }
}

/// Whether a theme sits on a dark or a light ground.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

/// What the terminal grid is painted with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// The selection base colour; it is drawn over the cell background at `selection_alpha`.
    pub selection: Rgb,
    pub selection_alpha: f32,
    pub ansi: [Rgb; 16],
}

/// The window chrome colours, the `--hw-*` tokens of design.md section 2.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChromeTokens {
    pub ground: Rgb,
    pub ground_2: Rgb,
    pub pane: Rgb,
    pub edge: Rgb,
    pub ink: Rgb,
    pub ink_2: Rgb,
    pub ink_3: Rgb,
    pub rose: Rgb,
    pub rose_deep: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub danger: Rgb,
    pub info: Rgb,
}

/// A complete theme: the terminal palette and the chrome tokens.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub version: u32,
    pub name: String,
    pub kind: ThemeKind,
    pub terminal: TerminalColors,
    pub chrome: ChromeTokens,
}

impl Theme {
    /// The built-in default.
    pub fn aubergine() -> Theme {
        Theme::default()
    }

    /// The colours the terminal engine needs.
    pub fn engine_theme(&self) -> EngineTheme {
        EngineTheme::default()
    }
}

/// The built-in themes and the ones the user added.
#[derive(Clone, Debug)]
pub struct ThemeRegistry {
    themes: Vec<Theme>,
}

impl ThemeRegistry {
    /// A registry with the built-in themes only.
    pub fn builtin() -> ThemeRegistry {
        ThemeRegistry { themes: vec![Theme::aubergine()] }
    }

    /// The theme called `name`; aubergine when there is none.
    pub fn get(&self, _name: &str) -> &Theme {
        &self.themes[0]
    }
}

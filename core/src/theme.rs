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

    /// Parse `#rrggbb`, with either case for the digits. Anything else is `None`: no short form, no spaces, no
    /// colour names and no functions.
    pub fn parse(text: &str) -> Option<Rgb> {
        let bytes = text.as_bytes();
        // Checking every byte first keeps the slicing below on character boundaries.
        if bytes.len() != 7 || bytes[0] != b'#' || !bytes[1..].iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&text[i..i + 2], 16).ok();
        Some(Rgb { r: channel(1)?, g: channel(3)?, b: channel(5)? })
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
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

/// The theme file format version that this build writes and reads.
pub const THEME_VERSION: u32 = 1;

/// The name of the default theme, and the one that every unknown name falls back to.
pub const DEFAULT_THEME: &str = "aubergine";

/// `0xRRGGBB` as a colour, so that the constructors below read like the tables in design.md.
const fn hex(v: u32) -> Rgb {
    Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Theme {
    /// The built-in default (D-20), the palette and tokens of design.md section 2.
    ///
    /// Bright black is the one deliberate departure from the table: `#4A3652` is 1.79:1 on the background (measured
    /// in the phase research) and programs use it for text (zsh-autosuggestions), so it is `#7C6A86`, 3.93:1.
    pub fn aubergine() -> Theme {
        Theme {
            version: THEME_VERSION,
            name: DEFAULT_THEME.into(),
            kind: ThemeKind::Dark,
            terminal: TerminalColors {
                foreground: hex(0xF6EFE6),
                background: hex(0x14091A),
                cursor: hex(0xFF8FB8),
                selection: hex(0xFF8FB8),
                selection_alpha: 0.35,
                ansi: [
                    hex(0x1B0E20),
                    hex(0xEC8272),
                    hex(0x8CC271),
                    hex(0xE8C15C),
                    hex(0x6F9BD0),
                    hex(0xFF8FB8),
                    hex(0x7FD1C7),
                    hex(0xD9C9D6),
                    hex(0x7C6A86),
                    hex(0xFFA396),
                    hex(0xAEDD93),
                    hex(0xF5D883),
                    hex(0x93B8E6),
                    hex(0xFFB3D1),
                    hex(0xA4E6DD),
                    hex(0xF6EFE6),
                ],
            },
            chrome: ChromeTokens {
                ground: hex(0x14091A),
                ground_2: hex(0x1D0F26),
                pane: hex(0x1B0E20),
                edge: hex(0x3A2144),
                ink: hex(0xF6EFE6),
                // ink at 70 % over ground, as a solid colour so that every token is a plain `#rrggbb`.
                ink_2: hex(0xB2AAA9),
                ink_3: hex(0x9C8EA0),
                rose: hex(0xFF8FB8),
                rose_deep: hex(0xC23B72),
                ok: hex(0x8CC271),
                warn: hex(0xE8C15C),
                danger: hex(0xEC8272),
                info: hex(0x8FB8D8),
            },
        }
    }

    /// The colours the terminal engine needs.
    pub fn engine_theme(&self) -> EngineTheme {
        let t = &self.terminal;
        EngineTheme {
            fg: t.foreground.into(),
            bg: t.background.into(),
            cursor: t.cursor.into(),
            ansi: t.ansi.map(Color::from),
        }
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
    pub fn get(&self, name: &str) -> &Theme {
        self.themes.iter().find(|t| t.name == name).unwrap_or(&self.themes[0])
    }
}

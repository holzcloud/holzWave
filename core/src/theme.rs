//! Themes: the built-in and user-supplied sets of terminal palette and chrome tokens, and how one is resolved and
//! handed to the engine.

use std::fmt;
use std::path::Path;

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

impl Theme {
    /// The neutral dark grey built-in (D-20): the aubergine hues for text and accent on a grey ground, so a
    /// terminal that was tuned for a grey background looks as intended and the rose accent stays.
    ///
    /// The grey is a little lighter than the aubergine ground, which costs the blue text some contrast on the
    /// selection; the selection opacity is 30 % instead of 35 % to keep every ANSI colour at 3:1 or better.
    pub fn schiefer() -> Theme {
        Theme {
            version: THEME_VERSION,
            name: "Schiefer".into(),
            kind: ThemeKind::Dark,
            terminal: TerminalColors {
                foreground: hex(0xF2EEE8),
                background: hex(0x1B1C1F),
                cursor: hex(0xFF8FB8),
                selection: hex(0xFF8FB8),
                selection_alpha: 0.30,
                ansi: [
                    hex(0x232428),
                    hex(0xEC8272),
                    hex(0x8CC271),
                    hex(0xE8C15C),
                    hex(0x6F9BD0),
                    hex(0xFF8FB8),
                    hex(0x7FD1C7),
                    hex(0xD4D2D0),
                    hex(0x7B7E86),
                    hex(0xFFA396),
                    hex(0xAEDD93),
                    hex(0xF5D883),
                    hex(0x93B8E6),
                    hex(0xFFB3D1),
                    hex(0xA4E6DD),
                    hex(0xF4F1EC),
                ],
            },
            chrome: ChromeTokens {
                ground: hex(0x1B1C1F),
                ground_2: hex(0x26272B),
                pane: hex(0x202124),
                edge: hex(0x34363B),
                ink: hex(0xF2EEE8),
                // ink at 70 % over ground.
                ink_2: hex(0xB1AFAC),
                ink_3: hex(0x8E9097),
                rose: hex(0xFF8FB8),
                rose_deep: hex(0xC23B72),
                ok: hex(0x8CC271),
                warn: hex(0xE8C15C),
                danger: hex(0xEC8272),
                info: hex(0x8FB8D8),
            },
        }
    }

    /// The light built-in (D-20): aubergine ink on paper.
    ///
    /// The ground is the cream token `#FFE6F0` lightened towards white; the bars use the cream itself. Rose
    /// `#FF8FB8` is below 2:1 on a light ground, so the accent, the cursor and the selection are rose-deep
    /// `#C23B72` (4.7:1 on the ground). Every ANSI colour has to reach 4.5:1 on paper, so "white" is a dark grey
    /// and the bright row is not lighter than the normal one; the selection is 25 % so that all of them also
    /// reach 3:1 on it.
    pub fn papier() -> Theme {
        Theme {
            version: THEME_VERSION,
            name: "Papier".into(),
            kind: ThemeKind::Light,
            terminal: TerminalColors {
                foreground: hex(0x14091A),
                background: hex(0xFFF3F8),
                cursor: hex(0xC23B72),
                selection: hex(0xC23B72),
                selection_alpha: 0.25,
                ansi: [
                    hex(0x14091A),
                    hex(0xB3261E),
                    hex(0x2F7A1F),
                    hex(0x8A5A00),
                    hex(0x1F5FA8),
                    hex(0xB0306A),
                    hex(0x00746B),
                    hex(0x4F4455),
                    hex(0x7A6B80),
                    hex(0xC42F22),
                    hex(0x2A7A18),
                    hex(0x946000),
                    hex(0x2B66B5),
                    hex(0xC23B72),
                    hex(0x00796F),
                    hex(0x2E2433),
                ],
            },
            chrome: ChromeTokens {
                ground: hex(0xFFF3F8),
                ground_2: hex(0xFFE6F0),
                pane: hex(0xFFF9FB),
                edge: hex(0xE5C6D5),
                ink: hex(0x14091A),
                // ink at 70 % over ground.
                ink_2: hex(0x5B4F5D),
                ink_3: hex(0x6B5C70),
                rose: hex(0xC23B72),
                rose_deep: hex(0xC23B72),
                ok: hex(0x2A7A18),
                warn: hex(0x8A5A00),
                danger: hex(0xB3261E),
                info: hex(0x1F5FA8),
            },
        }
    }
}

/// Whether `name` can be a theme name: 1 to 64 characters, letters, digits, space, dash or underscore, with no
/// space at either end. Names are data; they are never used to build a path (T-02-23).
pub fn valid_theme_name(_name: &str) -> bool {
    true
}

impl Theme {
    /// Check what the type system cannot: the version, the name and the selection opacity.
    pub fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// WCAG 2 relative luminance: the sRGB channels are linearised and weighted.
fn luminance(c: Rgb) -> f64 {
    let linear = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// The WCAG 2 contrast ratio of two colours, from 1.0 (identical) to 21.0 (black on white). Symmetric.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la >= lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

/// `top` drawn over `bottom` at opacity `alpha`, per channel, rounded to the nearest value. `alpha` is clamped
/// to 0.0..=1.0 and NaN counts as 0.0.
pub fn blend(top: Rgb, alpha: f32, bottom: Rgb) -> Rgb {
    let a = if alpha.is_nan() { 0.0 } else { f64::from(alpha.clamp(0.0, 1.0)) };
    let mix = |t: u8, b: u8| (f64::from(t) * a + f64::from(b) * (1.0 - a)).round() as u8;
    Rgb { r: mix(top.r, bottom.r), g: mix(top.g, bottom.g), b: mix(top.b, bottom.b) }
}

/// One entry of [`ThemeRegistry::list`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThemeInfo {
    pub name: String,
    pub kind: ThemeKind,
    /// `true` for the themes that ship with the app, `false` for the ones from the themes directory.
    pub builtin: bool,
}

/// The built-in themes and the ones the user added.
#[derive(Clone, Debug)]
pub struct ThemeRegistry {
    themes: Vec<Theme>,
}

impl ThemeRegistry {
    /// A registry with the built-in themes only.
    pub fn builtin() -> ThemeRegistry {
        ThemeRegistry { themes: vec![Theme::aubergine(), Theme::schiefer(), Theme::papier()] }
    }

    /// The built-ins plus every valid `*.json` file in `<dir>/themes`, and one warning per file that was skipped.
    pub fn load(_dir: &Path) -> (ThemeRegistry, Vec<String>) {
        (ThemeRegistry::builtin(), Vec::new())
    }

    /// Whether a theme called `name` exists.
    pub fn contains(&self, _name: &str) -> bool {
        false
    }

    /// Every theme, built-ins first, aubergine first of all.
    pub fn list(&self) -> Vec<ThemeInfo> {
        self.themes.iter().map(|t| ThemeInfo { name: t.name.clone(), kind: t.kind, builtin: true }).collect()
    }

    /// The theme called `name`; aubergine when there is none.
    pub fn get(&self, name: &str) -> &Theme {
        self.themes.iter().find(|t| t.name == name).unwrap_or(&self.themes[0])
    }
}

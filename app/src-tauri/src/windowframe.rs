//! The window frame of a desktop window, decided before any window exists.
//!
//! A frame cannot be changed on a window that is built, so the decision is made once, by [`decide`], from the operating
//! system alone, and every window is built with it (`lib.rs`, `build_window`). When there are facts, the window has no
//! system title bar and the page draws the window buttons; they reach the page as the global [`GLOBAL`], written by an
//! initialization script that [`FrameFacts::script`] builds from fixed words only. Nothing from the environment, the
//! process or a setting is ever put into that text, so the page can read it without trusting anything but this file.

/// The name of the page global that holds the facts (`app/src/windowframe.ts` reads the same name).
pub const GLOBAL: &str = "__HW_FRAME__";

/// The operating systems that get an own frame.
// The variants the system decision does not return yet are built by the tests and by the Linux frame that follows.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOs {
    Windows,
    Linux,
}

impl FrameOs {
    /// The word the page reads.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// How the buttons are drawn.
// The variants the system decision does not return yet are built by the tests and by the Linux frame that follows.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Flat,
    Round,
}

impl Style {
    /// The word the page reads.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Round => "round",
        }
    }
}

/// The side of the bar the buttons sit on.
// The variants the system decision does not return yet are built by the tests and by the Linux frame that follows.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// The word the page reads.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// One window button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

impl Button {
    /// The word the page reads.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }
}

/// What the page needs to draw the window buttons of this system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameFacts {
    pub os: FrameOs,
    pub style: Style,
    pub side: Side,
    /// In the order they are drawn.
    pub buttons: Vec<Button>,
}

impl FrameFacts {
    /// The initialization script: one assignment of the global with the four keys in a fixed order. Every value is one
    /// of the fixed words of the enums, each written as a JSON string, so the text has no other source.
    pub fn script(&self) -> String {
        let word = |text: &str| serde_json::to_string(text).expect("a word is a JSON string");
        let buttons: Vec<String> = self.buttons.iter().map(|button| word(button.word())).collect();
        format!(
            "window.{GLOBAL} = {{\"os\":{},\"style\":{},\"side\":{},\"buttons\":[{}]}};",
            word(self.os.word()),
            word(self.style.word()),
            word(self.side.word()),
            buttons.join(",")
        )
    }
}

/// The frame for the operating system `os` (`std::env::consts::OS`): Windows gets the own frame, flat, on the right,
/// with the three buttons. Every other system keeps the frame it has, or has none (`None`): macOS keeps its overlay
/// title bar, Android and iOS have no window frame, and Linux keeps the system's until the Linux frame is decided.
pub fn decide(os: &str) -> Option<FrameFacts> {
    match os {
        "windows" => Some(FrameFacts {
            os: FrameOs::Windows,
            style: Style::Flat,
            side: Side::Right,
            buttons: vec![Button::Minimize, Button::Maximize, Button::Close],
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS_SCRIPT: &str =
        r#"window.__HW_FRAME__ = {"os":"windows","style":"flat","side":"right","buttons":["minimize","maximize","close"]};"#;

    #[test]
    fn only_windows_gets_the_own_frame() {
        let rows = [
            ("windows", true),
            ("macos", false),
            ("android", false),
            ("ios", false),
            // Pinned until the Linux frame is decided: the system's frame stays.
            ("linux", false),
            ("freebsd", false),
            ("Windows", false),
            ("windows ", false),
            ("", false),
        ];
        for (os, own) in rows {
            assert_eq!(decide(os).is_some(), own, "os {os:?}");
        }
    }

    #[test]
    fn windows_is_flat_on_the_right_with_three_buttons() {
        let facts = decide("windows").expect("windows has facts");
        assert_eq!(facts.os, FrameOs::Windows);
        assert_eq!(facts.style, Style::Flat);
        assert_eq!(facts.side, Side::Right);
        assert_eq!(facts.buttons, [Button::Minimize, Button::Maximize, Button::Close]);
    }

    #[test]
    fn the_script_is_exactly_the_contract() {
        assert_eq!(decide("windows").expect("facts").script(), WINDOWS_SCRIPT);
    }

    #[test]
    fn the_script_is_one_assignment_of_a_json_object_with_four_keys() {
        let script = decide("windows").expect("facts").script();
        let json = script
            .strip_prefix("window.__HW_FRAME__ = ")
            .and_then(|rest| rest.strip_suffix(';'))
            .expect("the fixed prefix and the trailing semicolon");
        let value: serde_json::Value = serde_json::from_str(json).expect("the rest is JSON");
        let object = value.as_object().expect("an object");
        assert_eq!(object.len(), 4);
        for key in ["os", "style", "side", "buttons"] {
            assert!(object.contains_key(key), "key {key}");
        }
        assert_eq!(value["buttons"], serde_json::json!(["minimize", "maximize", "close"]));
        // The names the page reads are the ones this file writes.
        assert!(script.contains(GLOBAL));
    }

    #[test]
    fn the_script_holds_nothing_but_fixed_words_and_punctuation() {
        let script = decide("windows").expect("facts").script();
        let body = script.strip_prefix("window.__HW_FRAME__ = ").expect("prefix");
        assert!(
            body.chars().all(|c| c.is_ascii_lowercase() || matches!(c, '{' | '}' | '[' | ']' | '"' | ':' | ',' | ';')),
            "unexpected character in {body}"
        );
        // Every word is a word of an enum: the script cannot carry anything else.
        let words: Vec<&str> = body.split('"').skip(1).step_by(2).collect();
        let known = [
            "os", "style", "side", "buttons", "windows", "linux", "flat", "round", "left", "right", "minimize", "maximize",
            "close",
        ];
        for word in words {
            assert!(known.contains(&word), "unknown word {word}");
        }
    }

    #[test]
    fn every_variant_writes_a_lowercase_word() {
        for word in [
            FrameOs::Windows.word(),
            FrameOs::Linux.word(),
            Style::Flat.word(),
            Style::Round.word(),
            Side::Left.word(),
            Side::Right.word(),
            Button::Minimize.word(),
            Button::Maximize.word(),
            Button::Close.word(),
        ] {
            assert!(!word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase()), "word {word}");
        }
    }
}

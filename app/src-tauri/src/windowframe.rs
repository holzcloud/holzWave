//! The window frame of a desktop window, decided before any window exists.
//!
//! A frame cannot be changed on a window that is built, so the decision is made once, by [`decide`], and every window is
//! built with it (`lib.rs`, `build_window`). When there are facts, the window has no system title bar and the page draws
//! the window buttons; they reach the page as the global [`GLOBAL`], written by an initialization script that
//! [`FrameFacts::script`] builds from fixed words only. Nothing from the environment, the process or a setting is ever
//! put into that text, so the page can read it without trusting anything but this file.
//!
//! [`decide`] is a pure function of [`Inputs`]: the operating system, the stored choices, the desktop name and the text of
//! the GNOME button layout. Every desktop of the table is a data row of `app/tests/fixtures/window-frame.json`, which the
//! page's tests read as well. The session type (X11 or Wayland) is deliberately no input: the own frame is drawn under both
//! (an owner decision), and whether a Wayland window can be moved and resized cannot be detected while the app runs.

use std::time::Duration;

use holzwave_core::window_frame::{ButtonChoice, FrameChoice};

use crate::textscale::{Helper, command_of, output_within};

/// The name of the page global that holds the facts (`app/src/windowframe.ts` reads the same name).
pub const GLOBAL: &str = "__HW_FRAME__";

/// The longest desktop name that is looked at (`XDG_CURRENT_DESKTOP` is a short list; a longer value is not one).
const DESKTOP_LIMIT: usize = 4096;

/// The longest layout text that is read, between the quotes `gsettings` prints. The stock values are under 40 bytes.
const LAYOUT_LIMIT: usize = 128;

/// The longest the layout helper may run before it is given up. The setup waits on it before the first window exists, so
/// it is shorter than the text size helper's 750 ms; a real answer takes a few milliseconds.
const LAYOUT_DEADLINE: Duration = Duration::from_millis(300);

/// The operating systems that get an own frame.
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

/// The facts that decide a frame. Every one is untrusted text or a stored choice, and none of them reaches the page: they
/// only select among the fixed words of the enums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inputs<'a> {
    /// `std::env::consts::OS`.
    pub os: &'a str,
    /// The frame choice the window is built with.
    pub frame: FrameChoice,
    /// The button style choice.
    pub buttons: ButtonChoice,
    /// `XDG_CURRENT_DESKTOP`, if it is set.
    pub desktop: Option<&'a str>,
    /// What `gsettings get org.gnome.desktop.wm.preferences button-layout` printed, if it printed anything. Looked at on
    /// GNOME only.
    pub layout: Option<&'a str>,
}

/// The side and the buttons of the stock layout of every desktop but GNOME, and of a GNOME layout that cannot be read.
fn default_layout() -> Layout {
    Layout { side: Side::Right, buttons: vec![Button::Minimize, Button::Maximize, Button::Close] }
}

/// Whether `desktop` (the colon-separated `XDG_CURRENT_DESKTOP`, such as `ubuntu:GNOME`) names GNOME: one of its tokens is
/// `GNOME`, in any case. `GNOME-Classic` alone is another token and is not GNOME; no value, and a value over 4096 bytes,
/// are not GNOME either.
pub fn is_gnome(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|names| names.len() <= DESKTOP_LIMIT && names.split(':').any(|name| name.eq_ignore_ascii_case("GNOME")))
}

/// A button layout: the side that is drawn and its buttons, in the order they are drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub side: Side,
    pub buttons: Vec<Button>,
}

/// The buttons a comma-separated list names, in order; every other word (`appmenu`, `menu`, `icon`, `spacer`, anything)
/// is ignored. `None` when a button is named twice.
fn buttons_of(list: &str, seen: &mut Vec<Button>) -> Option<Vec<Button>> {
    let mut named = Vec::new();
    for word in list.split(',') {
        let button = match word {
            "minimize" => Button::Minimize,
            "maximize" => Button::Maximize,
            "close" => Button::Close,
            _ => continue,
        };
        if seen.contains(&button) {
            return None;
        }
        seen.push(button);
        named.push(button);
    }
    Some(named)
}

/// The layout of the text `gsettings get org.gnome.desktop.wm.preferences button-layout` printed, or `None` when it is not
/// a layout (the caller then draws [`default_layout`]).
///
/// The text is one pair of single quotes around at most 128 bytes of `a-z , : _ -`, with one colon, and an optional line
/// ending after the closing quote. The words before the colon sit on the left, the words after it on the right; only
/// `minimize`, `maximize` and `close` count, each at most once in the whole text, and every other word is ignored. The
/// side that holds `close` is drawn with the buttons it names; when no side holds `close` the side of the first window
/// button is drawn with its buttons; when the layout names no window button at all (`appmenu:`, `:`, the empty pair) the
/// close button alone is drawn on the right, because a window that cannot be closed by the pointer is worse than a button
/// that was not asked for.
pub fn parse_layout(text: &str) -> Option<Layout> {
    let body = text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')).unwrap_or(text);
    let inner = body.strip_prefix('\'')?.strip_suffix('\'')?;
    if inner.len() > LAYOUT_LIMIT || !inner.bytes().all(|b| matches!(b, b'a'..=b'z' | b',' | b':' | b'_' | b'-')) {
        return None;
    }
    // `gsettings` prints an empty setting as the empty pair: it names no button, which is not a damaged text.
    if inner.is_empty() {
        return Some(Layout { side: Side::Right, buttons: vec![Button::Close] });
    }
    let (left, right) = inner.split_once(':')?;
    if right.contains(':') {
        return None;
    }
    let mut seen = Vec::new();
    let left = buttons_of(left, &mut seen)?;
    let right = buttons_of(right, &mut seen)?;
    Some(if left.contains(&Button::Close) {
        Layout { side: Side::Left, buttons: left }
    } else if right.contains(&Button::Close) {
        Layout { side: Side::Right, buttons: right }
    } else if !left.is_empty() {
        Layout { side: Side::Left, buttons: left }
    } else if !right.is_empty() {
        Layout { side: Side::Right, buttons: right }
    } else {
        Layout { side: Side::Right, buttons: vec![Button::Close] }
    })
}

/// The facts of the own frame on Linux: the style follows the choice or the desktop, and on GNOME the system's layout
/// decides the side and the buttons.
fn linux_facts(input: &Inputs) -> FrameFacts {
    let gnome = is_gnome(input.desktop);
    let style = match input.buttons {
        ButtonChoice::Flat => Style::Flat,
        ButtonChoice::Round => Style::Round,
        ButtonChoice::Auto if gnome => Style::Round,
        ButtonChoice::Auto => Style::Flat,
    };
    let layout = if gnome { input.layout.and_then(parse_layout) } else { None };
    let Layout { side, buttons } = layout.unwrap_or_else(default_layout);
    FrameFacts { os: FrameOs::Linux, style, side, buttons }
}

/// The facts of the own frame for the operating system, or `None` where there is no own frame.
fn own_frame(input: &Inputs) -> Option<FrameFacts> {
    match input.os {
        // Flat, on the right, whatever the button choice says: Windows has one look.
        "windows" => {
            let Layout { side, buttons } = default_layout();
            Some(FrameFacts { os: FrameOs::Windows, style: Style::Flat, side, buttons })
        }
        "linux" => Some(linux_facts(input)),
        // macOS keeps its overlay title bar, Android and iOS have no window frame, and an unknown system keeps its own.
        _ => None,
    }
}

/// The frame for `input`: `None` where the window keeps the system's frame or has none, otherwise the facts the page
/// draws the window buttons from. `Auto` and `Own` are two arms that give the same facts today; a later change of what
/// `Auto` means touches one of them.
#[allow(clippy::match_same_arms)]
pub fn decide(input: &Inputs) -> Option<FrameFacts> {
    match input.frame {
        FrameChoice::System => None,
        FrameChoice::Auto => own_frame(input),
        FrameChoice::Own => own_frame(input),
    }
}

/// `gsettings get` of the GNOME window manager's `button-layout`, by the absolute path the text size helper uses. It is
/// started with a cleared environment and closed standard input, and its output is read to at most 4096 bytes
/// (`textscale::command_of` and `output_within`).
fn layout_helper() -> Helper {
    Helper {
        program: "/usr/bin/gsettings".to_owned(),
        args: ["get", "org.gnome.desktop.wm.preferences", "button-layout"].map(str::to_owned).into(),
    }
}

/// The frame for `os` (as `std::env::consts::OS` spells it) and the stored choices, reading the system through `env`
/// (the lookup of a process variable) and `run` (which starts a helper and gives its output, or `None` for a helper that
/// did not answer). The layout helper runs only on Linux, with the own frame, under a GNOME desktop: never on another
/// system, with the frame `system`, or under another desktop. A layout that is not valid UTF-8 is unreadable, not a
/// lossy one, and an unreadable or missing answer gives the documented default (see [`parse_layout`]).
pub fn facts_via(
    os: &str,
    frame: FrameChoice,
    buttons: ButtonChoice,
    env: &dyn Fn(&str) -> Option<String>,
    run: &dyn Fn(&Helper) -> Option<Vec<u8>>,
) -> Option<FrameFacts> {
    let desktop = env("XDG_CURRENT_DESKTOP");
    let reads_layout = os == "linux" && frame != FrameChoice::System && is_gnome(desktop.as_deref());
    let layout = if reads_layout { run(&layout_helper()).and_then(|bytes| String::from_utf8(bytes).ok()) } else { None };
    decide(&Inputs { os, frame, buttons, desktop: desktop.as_deref(), layout: layout.as_deref() })
}

/// The frame of this system for the choices the windows are built with. Blocks for up to the layout deadline on a GNOME
/// desktop with the own frame, and not at all elsewhere: call it before the first window exists.
pub fn facts_for_this_system(frame: FrameChoice, buttons: ButtonChoice) -> Option<FrameFacts> {
    facts_via(std::env::consts::OS, frame, buttons, &|name| std::env::var(name).ok(), &|helper| {
        output_within(command_of(helper), LAYOUT_DEADLINE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOWS_SCRIPT: &str =
        r#"window.__HW_FRAME__ = {"os":"windows","style":"flat","side":"right","buttons":["minimize","maximize","close"]};"#;

    /// The cases the page's tests read too: inputs, the expected facts and the expected script text.
    const FIXTURE: &str = include_str!("../../tests/fixtures/window-frame.json");

    struct Case {
        name: String,
        os: String,
        frame: FrameChoice,
        buttons: ButtonChoice,
        desktop: Option<String>,
        layout: Option<String>,
        facts: Option<serde_json::Value>,
        script: Option<String>,
    }

    fn cases() -> Vec<Case> {
        let list: Vec<serde_json::Value> = serde_json::from_str(FIXTURE).expect("the fixture is a JSON list");
        list.iter()
            .map(|item| {
                let text = |key: &str| item[key].as_str().map(str::to_owned);
                let name = text("name").expect("a name");
                let word = |key: &str| text(key).unwrap_or_else(|| panic!("case {name}: {key} is a word"));
                Case {
                    os: word("os"),
                    frame: FrameChoice::from_word(&word("frame")).unwrap_or_else(|| panic!("case {name}: a frame word")),
                    buttons: ButtonChoice::from_word(&word("buttons")).unwrap_or_else(|| panic!("case {name}: a buttons word")),
                    desktop: text("desktop"),
                    layout: text("layout"),
                    facts: (!item["facts"].is_null()).then(|| item["facts"].clone()),
                    script: text("script"),
                    name,
                }
            })
            .collect()
    }

    fn inputs(case: &Case) -> Inputs<'_> {
        Inputs {
            os: &case.os,
            frame: case.frame,
            buttons: case.buttons,
            desktop: case.desktop.as_deref(),
            layout: case.layout.as_deref(),
        }
    }

    fn facts_json(facts: &FrameFacts) -> serde_json::Value {
        serde_json::json!({
            "os": facts.os.word(),
            "style": facts.style.word(),
            "side": facts.side.word(),
            "buttons": facts.buttons.iter().map(|b| b.word()).collect::<Vec<_>>(),
        })
    }

    /// The inputs of one row of the hand-written tables below.
    fn linux<'a>(buttons: ButtonChoice, desktop: Option<&'a str>, layout: Option<&'a str>) -> Inputs<'a> {
        Inputs { os: "linux", frame: FrameChoice::Auto, buttons, desktop, layout }
    }

    const MCX: [Button; 3] = [Button::Minimize, Button::Maximize, Button::Close];

    // ---- the shared fixture -----------------------------------------------------------------------------

    #[test]
    fn every_case_of_the_fixture_decides_the_facts_and_the_script_it_states() {
        let cases = cases();
        assert!(cases.len() > 50, "the fixture is the table of the contract");
        for case in &cases {
            let decided = decide(&inputs(case));
            assert_eq!(decided.as_ref().map(facts_json), case.facts, "case {}", case.name);
            assert_eq!(decided.map(|facts| facts.script()), case.script, "case {}", case.name);
        }
    }

    #[test]
    fn the_fixture_has_a_row_for_every_desktop_and_every_kind_of_layout_of_the_table() {
        let cases = cases();
        let has = |needle: &str| assert!(cases.iter().any(|c| c.name.contains(needle)), "no case named like {needle:?}");
        for needle in [
            "windows auto", "windows system", "windows own with round", "gnome stock", "gnome ubuntu", "gnome left layout",
            "buttons on both sides", "names no window button", "minimise and no close", "unreadable layout", "flat buttons choice",
            "flat desktop KDE", "flat desktop XFCE", "unknown desktop", "no desktop", "kde with round", "system frame",
            "os 'macos'", "os 'android'",
        ] {
            has(needle);
        }
        let layouts = cases.iter().filter(|c| c.name.starts_with("gnome unreadable layout")).count();
        assert!(layouts >= 15, "every kind of unreadable layout is a row ({layouts})");
    }

    #[test]
    fn the_windows_case_states_the_literal_of_the_first_tracer_test() {
        let cases = cases();
        let windows = cases.iter().find(|c| c.name == "windows auto").expect("the Windows case");
        assert_eq!(windows.script.as_deref(), Some(WINDOWS_SCRIPT));
    }

    #[test]
    fn the_session_type_is_no_input_so_auto_and_own_give_equal_facts_in_every_case() {
        for case in cases() {
            let own = Inputs { frame: FrameChoice::Own, ..inputs(&case) };
            let auto = Inputs { frame: FrameChoice::Auto, ..inputs(&case) };
            assert_eq!(decide(&own), decide(&auto), "case {}", case.name);
        }
        // Wayland sets this variable and nothing here reads it: the one place that reads the environment names the
        // desktop only.
        let source = include_str!("windowframe.rs");
        let shipped = source.split("\n#[cfg(test)]\nmod tests").next().expect("the shipped part");
        for name in ["WAYLAND_DISPLAY", "XDG_SESSION_TYPE"] {
            assert!(!shipped.lines().filter(|l| !l.trim_start().starts_with("//")).any(|l| l.contains(name)), "{name} is no input");
        }
    }

    #[test]
    fn the_script_of_every_case_with_facts_holds_exactly_four_keys_of_fixed_words() {
        let known = ["os", "style", "side", "buttons", "windows", "linux", "flat", "round", "left", "right", "minimize", "maximize", "close"];
        let mut with_facts = 0;
        for case in cases() {
            let Some(script) = decide(&inputs(&case)).map(|facts| facts.script()) else { continue };
            with_facts += 1;
            let body = script.strip_prefix("window.__HW_FRAME__ = ").unwrap_or_else(|| panic!("case {}: prefix", case.name));
            let json = body.strip_suffix(';').unwrap_or_else(|| panic!("case {}: semicolon", case.name));
            let value: serde_json::Value = serde_json::from_str(json).unwrap_or_else(|_| panic!("case {}: JSON", case.name));
            // The reader may sort the keys; their written order is pinned by the exact script text of the fixture.
            let mut keys: Vec<&String> = value.as_object().expect("an object").keys().collect();
            keys.sort();
            assert_eq!(keys, ["buttons", "os", "side", "style"], "case {}", case.name);
            let buttons = value["buttons"].as_array().expect("a list");
            assert!((1..=3).contains(&buttons.len()), "case {}", case.name);
            assert!(
                body.chars().all(|c| c.is_ascii_lowercase() || matches!(c, '{' | '}' | '[' | ']' | '"' | ':' | ',' | ';')),
                "case {}: unexpected character in {body}",
                case.name
            );
            for word in body.split('"').skip(1).step_by(2) {
                assert!(known.contains(&word), "case {}: unknown word {word}", case.name);
            }
        }
        assert!(with_facts > 30, "the check ran over {with_facts} scripts");
    }

    // ---- the decision ----------------------------------------------------------------------------------

    #[test]
    fn windows_is_flat_on_the_right_with_three_buttons_and_the_button_choice_does_nothing() {
        for frame in [FrameChoice::Auto, FrameChoice::Own] {
            for buttons in [ButtonChoice::Auto, ButtonChoice::Flat, ButtonChoice::Round] {
                let facts = decide(&Inputs { os: "windows", frame, buttons, desktop: Some("GNOME"), layout: Some("'close:'") })
                    .expect("windows has facts");
                assert_eq!(facts.os, FrameOs::Windows);
                assert_eq!(facts.style, Style::Flat);
                assert_eq!(facts.side, Side::Right);
                assert_eq!(facts.buttons, MCX);
            }
        }
        for buttons in [ButtonChoice::Auto, ButtonChoice::Flat, ButtonChoice::Round] {
            let system = Inputs { os: "windows", frame: FrameChoice::System, buttons, desktop: None, layout: None };
            assert_eq!(decide(&system), None);
        }
    }

    #[test]
    fn the_other_systems_have_no_facts_for_every_choice() {
        for os in ["macos", "android", "ios", "freebsd", "Windows", "windows ", "Linux", ""] {
            for frame in [FrameChoice::Auto, FrameChoice::Own, FrameChoice::System] {
                let input = Inputs { os, frame, buttons: ButtonChoice::Round, desktop: Some("GNOME"), layout: Some("'close:'") };
                assert_eq!(decide(&input), None, "os {os:?} frame {frame:?}");
            }
        }
    }

    #[test]
    fn linux_has_the_own_frame_under_every_desktop_unless_the_frame_is_system() {
        for desktop in [None, Some(""), Some("GNOME"), Some("ubuntu:GNOME"), Some("KDE"), Some("XFCE"), Some("sway"), Some("Hyprland")] {
            for frame in [FrameChoice::Auto, FrameChoice::Own] {
                let input = Inputs { frame, ..linux(ButtonChoice::Auto, desktop, None) };
                let facts = decide(&input).unwrap_or_else(|| panic!("no facts for {desktop:?} {frame:?}"));
                assert_eq!(facts.os, FrameOs::Linux);
            }
            let system = Inputs { frame: FrameChoice::System, ..linux(ButtonChoice::Auto, desktop, None) };
            assert_eq!(decide(&system), None, "the system frame under {desktop:?}");
        }
    }

    #[test]
    fn the_style_is_round_on_gnome_and_flat_elsewhere_for_auto_and_the_choice_decides_otherwise() {
        let rows = [
            (Some("GNOME"), ButtonChoice::Auto, Style::Round),
            (Some("ubuntu:GNOME"), ButtonChoice::Auto, Style::Round),
            (Some("KDE"), ButtonChoice::Auto, Style::Flat),
            (Some("XFCE"), ButtonChoice::Auto, Style::Flat),
            (None, ButtonChoice::Auto, Style::Flat),
            (Some("GNOME"), ButtonChoice::Flat, Style::Flat),
            (Some("KDE"), ButtonChoice::Flat, Style::Flat),
            (None, ButtonChoice::Flat, Style::Flat),
            (Some("GNOME"), ButtonChoice::Round, Style::Round),
            (Some("KDE"), ButtonChoice::Round, Style::Round),
            (None, ButtonChoice::Round, Style::Round),
        ];
        for (desktop, buttons, style) in rows {
            let facts = decide(&linux(buttons, desktop, None)).expect("facts");
            assert_eq!(facts.style, style, "{desktop:?} {buttons:?}");
        }
    }

    #[test]
    fn on_gnome_the_layout_decides_and_everywhere_else_it_is_ignored() {
        let stock = Some("'appmenu:close'\n");
        let gnome = decide(&linux(ButtonChoice::Auto, Some("GNOME"), stock)).expect("facts");
        assert_eq!((gnome.side, gnome.buttons), (Side::Right, vec![Button::Close]));
        let left = Some("'close,minimize,maximize:'\n");
        let gnome = decide(&linux(ButtonChoice::Auto, Some("GNOME"), left)).expect("facts");
        assert_eq!((gnome.side, gnome.buttons), (Side::Left, vec![Button::Close, Button::Minimize, Button::Maximize]));
        for desktop in [Some("KDE"), Some("XFCE"), Some("GNOME-Classic"), Some("unknown"), Some(""), None] {
            for layout in [stock, left, None] {
                let facts = decide(&linux(ButtonChoice::Auto, desktop, layout)).expect("facts");
                assert_eq!((facts.side, facts.buttons), (Side::Right, MCX.to_vec()), "{desktop:?} {layout:?}");
            }
        }
    }

    // ---- is_gnome --------------------------------------------------------------------------------------

    #[test]
    fn gnome_is_a_token_of_the_colon_separated_desktop_name_in_any_case() {
        for name in ["GNOME", "ubuntu:GNOME", "pop:GNOME", "Budgie:GNOME", "gnome", "GNOME:ubuntu", "GNOME-Classic:GNOME", "Gnome"] {
            assert!(is_gnome(Some(name)), "{name}");
        }
        for name in [
            "KDE", "XFCE", "X-Cinnamon", "MATE", "GNOME-Classic", "GNOMEX", "XGNOME", "X-GNOME", "GNOME-Flashback", "ubuntu", "GNOME ", " GNOME",
            "GNOME;KDE", "", ":", "::",
        ] {
            assert!(!is_gnome(Some(name)), "{name:?}");
        }
        assert!(!is_gnome(None));
    }

    #[test]
    fn a_desktop_name_over_4096_bytes_is_not_gnome_and_one_of_exactly_4096_is_read() {
        let exactly = format!("{}:GNOME", "x".repeat(4096 - ":GNOME".len()));
        assert_eq!(exactly.len(), 4096);
        assert!(is_gnome(Some(&exactly)));
        let over = format!("{exactly}x");
        assert!(!is_gnome(Some(&over)));
        let over = format!("{}:GNOME", "x".repeat(5000));
        assert!(!is_gnome(Some(&over)));
    }

    // ---- parse_layout ----------------------------------------------------------------------------------

    fn layout(side: Side, buttons: &[Button]) -> Option<Layout> {
        Some(Layout { side, buttons: buttons.to_vec() })
    }

    #[test]
    fn the_layouts_of_stock_gnome_ubuntu_and_the_left_are_read() {
        assert_eq!(parse_layout("'appmenu:close'\n"), layout(Side::Right, &[Button::Close]));
        assert_eq!(parse_layout("'appmenu:minimize,maximize,close'\n"), layout(Side::Right, &MCX));
        assert_eq!(
            parse_layout("'close,minimize,maximize:appmenu'\n"),
            layout(Side::Left, &[Button::Close, Button::Minimize, Button::Maximize])
        );
        // The line ending is optional, and a carriage return belongs to it.
        assert_eq!(parse_layout("'appmenu:close'"), layout(Side::Right, &[Button::Close]));
        assert_eq!(parse_layout("'appmenu:close'\r\n"), layout(Side::Right, &[Button::Close]));
    }

    #[test]
    fn the_side_that_holds_close_is_drawn_with_its_own_buttons() {
        assert_eq!(parse_layout("'close:minimize,maximize'\n"), layout(Side::Left, &[Button::Close]));
        assert_eq!(parse_layout("'minimize:maximize,close'\n"), layout(Side::Right, &[Button::Maximize, Button::Close]));
        assert_eq!(parse_layout("'icon,close:menu,minimize'\n"), layout(Side::Left, &[Button::Close]));
    }

    #[test]
    fn a_layout_that_names_no_window_button_is_the_close_button_alone_on_the_right() {
        for text in ["'appmenu:'\n", "':'\n", "''\n", "''", "'menu,icon,spacer:foo'\n", "'spacer:appmenu'\n"] {
            assert_eq!(parse_layout(text), layout(Side::Right, &[Button::Close]), "{text:?}");
        }
    }

    #[test]
    fn a_layout_with_minimise_and_no_close_draws_the_side_of_its_first_window_button() {
        assert_eq!(parse_layout("':minimize,maximize'\n"), layout(Side::Right, &[Button::Minimize, Button::Maximize]));
        assert_eq!(parse_layout("'minimize,maximize:'\n"), layout(Side::Left, &[Button::Minimize, Button::Maximize]));
        assert_eq!(parse_layout("'minimize:maximize'\n"), layout(Side::Left, &[Button::Minimize]));
    }

    #[test]
    fn unknown_words_are_ignored() {
        assert_eq!(parse_layout("'icon,menu:spacer,minimize,whatever,close'\n"), layout(Side::Right, &[Button::Minimize, Button::Close]));
        assert_eq!(parse_layout("'appmenu,,:,close,,'\n"), layout(Side::Right, &[Button::Close]));
        assert_eq!(parse_layout("'a_b-c:close'\n"), layout(Side::Right, &[Button::Close]));
    }

    #[test]
    fn an_unreadable_text_is_none_and_never_a_partial_layout() {
        let long = format!("'{}:close'", "x".repeat(123));
        let rows = [
            ("empty", ""),
            ("a line ending", "\n"),
            ("no colon", "'appmenu'\n"),
            ("two colons", "'close:minimize:maximize'\n"),
            ("three colons", "'::'\n"),
            ("a button twice on one side", "'minimize,minimize:close'\n"),
            ("a button twice across the sides", "'close:close'\n"),
            ("an upper-case letter", "'appmenu:Close'\n"),
            ("a digit", "'appmenu:close1'\n"),
            ("a space", "'appmenu: close'\n"),
            ("a semicolon", "'appmenu:close;'\n"),
            ("only an opening quote", "'appmenu:close\n"),
            ("only a closing quote", "appmenu:close'\n"),
            ("a lone quote", "'\n"),
            ("double quotes", "\"appmenu:close\"\n"),
            ("no quotes", "appmenu:close\n"),
            ("a quote inside the pair", "'appmenu:cl'ose'\n"),
            ("two lines", "'appmenu:close'\n'x'\n"),
            ("text after the pair", "'appmenu:close' x\n"),
            ("129 bytes", &long),
            ("the text of an error", "No such schema \u{201c}org.gnome.desktop.wm.preferences\u{201d}\n"),
            ("two line endings", "'appmenu:close'\n\n"),
        ];
        for (what, text) in rows {
            assert_eq!(parse_layout(text), None, "{what}: {text:?}");
        }
    }

    #[test]
    fn a_layout_of_128_bytes_is_read_and_one_of_129_is_not() {
        let at = format!("'{}:close'", "x".repeat(122));
        assert_eq!(at.len() - 2, 128);
        assert_eq!(parse_layout(&at), layout(Side::Right, &[Button::Close]));
        let over = format!("'{}:close'", "x".repeat(123));
        assert_eq!(parse_layout(&over), None);
    }

    // ---- the script ------------------------------------------------------------------------------------

    #[test]
    fn the_script_is_exactly_the_contract() {
        let windows = Inputs { os: "windows", frame: FrameChoice::Auto, buttons: ButtonChoice::Auto, desktop: None, layout: None };
        assert_eq!(decide(&windows).expect("facts").script(), WINDOWS_SCRIPT);
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

    // ---- the reader of the system -----------------------------------------------------------------------

    /// A runner that fails the test when it is called.
    fn never(_: &Helper) -> Option<Vec<u8>> {
        panic!("the layout helper was started");
    }

    fn env_of(desktop: Option<&'static str>) -> impl Fn(&str) -> Option<String> {
        move |name| {
            assert_eq!(name, "XDG_CURRENT_DESKTOP", "the desktop name is the only variable read");
            desktop.map(str::to_owned)
        }
    }

    fn answers(text: &'static [u8]) -> impl Fn(&Helper) -> Option<Vec<u8>> {
        move |helper| {
            assert_eq!(helper, &layout_helper());
            Some(text.to_vec())
        }
    }

    fn shape(facts: Option<FrameFacts>) -> Option<(Style, Side, Vec<Button>)> {
        facts.map(|f| (f.style, f.side, f.buttons))
    }

    #[test]
    fn the_layout_helper_is_gsettings_by_absolute_path_with_its_exact_arguments_and_a_short_deadline() {
        let helper = layout_helper();
        assert_eq!(helper.program, "/usr/bin/gsettings");
        assert!(std::path::Path::new(&helper.program).is_absolute());
        assert_eq!(helper.args, ["get", "org.gnome.desktop.wm.preferences", "button-layout"]);
        assert!(LAYOUT_DEADLINE <= Duration::from_millis(300));
        assert!(LAYOUT_DEADLINE > Duration::ZERO);
    }

    #[test]
    fn the_layout_helper_is_not_started_for_any_system_choice_or_desktop_that_does_not_need_it() {
        let own = [FrameChoice::Auto, FrameChoice::Own];
        for os in ["macos", "windows", "android", "ios", "freebsd", ""] {
            for frame in [FrameChoice::Auto, FrameChoice::Own, FrameChoice::System] {
                facts_via(os, frame, ButtonChoice::Auto, &env_of(Some("GNOME")), &never);
            }
        }
        for buttons in [ButtonChoice::Auto, ButtonChoice::Flat, ButtonChoice::Round] {
            for desktop in [Some("GNOME"), Some("ubuntu:GNOME")] {
                assert_eq!(facts_via("linux", FrameChoice::System, buttons, &env_of(desktop), &never), None);
            }
            for desktop in [Some("KDE"), Some("XFCE"), Some("GNOME-Classic"), Some("GNOMEX"), Some(""), None] {
                for frame in own {
                    assert!(facts_via("linux", frame, buttons, &env_of(desktop), &never).is_some(), "{desktop:?}");
                }
            }
        }
    }

    #[test]
    fn the_layout_helper_is_started_on_gnome_with_the_own_frame_and_its_answer_decides() {
        let run = |text, desktop, frame| shape(facts_via("linux", frame, ButtonChoice::Auto, &env_of(desktop), &answers(text)));
        assert_eq!(run(b"'appmenu:close'\n", Some("GNOME"), FrameChoice::Auto), Some((Style::Round, Side::Right, vec![Button::Close])));
        assert_eq!(
            run(b"'appmenu:minimize,maximize,close'\n", Some("ubuntu:GNOME"), FrameChoice::Own),
            Some((Style::Round, Side::Right, MCX.to_vec()))
        );
        assert_eq!(
            run(b"'close,minimize,maximize:appmenu'\n", Some("GNOME"), FrameChoice::Auto),
            Some((Style::Round, Side::Left, vec![Button::Close, Button::Minimize, Button::Maximize]))
        );
    }

    #[test]
    fn a_helper_that_did_not_answer_gives_the_round_right_three_button_default_on_gnome() {
        let default = Some((Style::Round, Side::Right, MCX.to_vec()));
        let gnome = env_of(Some("GNOME"));
        let call = |run: &dyn Fn(&Helper) -> Option<Vec<u8>>| {
            shape(facts_via("linux", FrameChoice::Auto, ButtonChoice::Auto, &gnome, run))
        };
        assert_eq!(call(&|_| None), default, "no program or a timeout");
        assert_eq!(call(&|_| Some(Vec::new())), default, "an empty answer");
        assert_eq!(call(&|_| Some(vec![0xff, 0xfe, b'\n'])), default, "bytes that are not UTF-8");
        assert_eq!(call(&|_| Some(b"'appmenu:close\xff'\n".to_vec())), default, "a stray byte inside the pair");
        assert_eq!(
            call(&|_| Some("No such schema \u{201c}org.gnome.desktop.wm.preferences\u{201d}\n".as_bytes().to_vec())),
            default,
            "the text of an error"
        );
    }

    #[test]
    fn a_text_cut_off_at_the_output_limit_is_unreadable_and_never_a_partial_layout() {
        let gnome = env_of(Some("GNOME"));
        // What `output_within` returns for a helper that never stops: the first 4096 bytes, with no closing quote.
        let mut endless = b"'appmenu:close,".to_vec();
        endless.resize(4096, b'x');
        let facts = facts_via("linux", FrameChoice::Auto, ButtonChoice::Auto, &gnome, &|_| Some(endless.clone()));
        assert_eq!(shape(facts), Some((Style::Round, Side::Right, MCX.to_vec())));
        // A text cut in the middle of a layout has lost its closing quote.
        let facts = facts_via("linux", FrameChoice::Auto, ButtonChoice::Auto, &gnome, &answers(b"'close,minimize,maxi"));
        assert_eq!(shape(facts), Some((Style::Round, Side::Right, MCX.to_vec())));
    }

    #[test]
    fn the_style_choice_still_decides_when_the_layout_is_read() {
        let flat = facts_via("linux", FrameChoice::Auto, ButtonChoice::Flat, &env_of(Some("GNOME")), &answers(b"'appmenu:close'\n"));
        assert_eq!(shape(flat), Some((Style::Flat, Side::Right, vec![Button::Close])));
    }

    #[test]
    fn the_real_read_gives_no_facts_for_the_system_frame_and_never_starts_anything_for_it() {
        // Whatever this machine is, the frame `system` is no own frame and decides without a helper.
        assert_eq!(facts_for_this_system(FrameChoice::System, ButtonChoice::Auto), None);
    }

    // Unix test children only: the Windows test target has no `sleep` (and still compiles this module).
    #[cfg(unix)]
    #[test]
    fn a_hung_helper_costs_the_layout_within_the_deadline_and_never_the_start() {
        use std::time::Instant;
        let hung = |_: &Helper| {
            let helper = Helper { program: "/bin/sleep".to_owned(), args: vec!["30".to_owned()] };
            output_within(command_of(&helper), LAYOUT_DEADLINE)
        };
        let start = Instant::now();
        let facts = facts_via("linux", FrameChoice::Auto, ButtonChoice::Auto, &env_of(Some("GNOME")), &hung);
        assert_eq!(shape(facts), Some((Style::Round, Side::Right, MCX.to_vec())));
        assert!(start.elapsed() < Duration::from_secs(5), "waited {:?} for a helper that hangs", start.elapsed());
    }
}

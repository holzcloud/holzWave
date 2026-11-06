//! `host_shell.json`: whether a Flatpak starts a local pane on the host computer instead of inside its sandbox, and the rule
//! that decides it.
//!
//! A Flatpak runs an app in a sandbox. A terminal in a sandbox sees none of the person's own tools, so holzWave can start a
//! pane on the host through `flatpak-spawn --host`, which takes that pane out of the sandbox and gives it the person's
//! rights. That is a choice about safety, so it is off until the person turns it on. This file holds the three facts the
//! core owns:
//!
//! * the **choice** of the person, kept in `host_shell.json` ([`HostShell`]): one boolean, `false` by default;
//! * the **permission** of this Flatpak to talk to `org.freedesktop.Flatpak`, read from the sandbox's own `/.flatpak-info`
//!   ([`permission_in`]). The choice cannot grant it: the permission is fixed in the manifest when the Flatpak is built, and
//!   a Flatpak tool can only take it away;
//! * the **state** shown to the person ([`State`]) and the rule that joins them ([`state_of`], [`can_set`]).
//!
//! The file is machine-local and is never synced: `settings.json` travels to every computer, and a second computer must not
//! inherit a way out of the sandbox. A damaged or hostile file costs the choice and never the start: [`HostShell::load`] has
//! no error case, and only the exact JSON boolean `true` turns the host shell on. This file has no network code, no unsafe
//! code and no dependency beyond the JSON reader the other files use.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

/// The file name inside the app data directory.
pub const FILE: &str = "host_shell.json";
/// The format version that is written.
pub const VERSION: u32 = 1;
/// The largest `host_shell.json` that is read. The file is a few dozen bytes.
pub const MAX_BYTES: u64 = 4096;
/// The largest `/.flatpak-info` that is read. A real one is about a kilobyte; text above this is never trusted.
pub const MAX_INFO_BYTES: u64 = 64 * 1024;
/// The path of the sandbox's own description of itself, inside a Flatpak.
pub const INFO_PATH: &str = "/.flatpak-info";

/// The section of a Flatpak info text that lists the names on the session bus the app may talk to or own.
const BUS_SECTION: &str = "Session Bus Policy";
/// The name on the session bus whose policy decides whether `flatpak-spawn --host` works.
const BUS_NAME: &str = "org.freedesktop.Flatpak";

/// The content of `host_shell.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostShell {
    pub version: u32,
    /// The person turned the host shell on. `false` (the default) starts every pane in the sandbox.
    pub enabled: bool,
}

impl Default for HostShell {
    fn default() -> Self {
        HostShell { version: VERSION, enabled: false }
    }
}

impl HostShell {
    /// Reads the document. There is no error case, and every doubt reads as off: text that is not JSON, not an object or of a
    /// version other than the current one is `Reset`; a missing or mistyped `host_shell` (a string, a number, `null`) is off and
    /// the result is `Repaired`. Only the exact boolean `true` enables. Unknown keys are ignored.
    pub fn from_json(text: &str) -> (HostShell, LoadStatus) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return (HostShell::default(), LoadStatus::Reset(format!("not a host shell file: {e}"))),
        };
        let Value::Object(object) = value else {
            return (
                HostShell::default(),
                LoadStatus::Reset("not a host shell file: the top level is not an object".into()),
            );
        };
        if let Some(version) = object.get("version") {
            // A version this build does not know could mean something else by the same key: fail closed.
            if version.as_u64() != Some(u64::from(VERSION)) {
                return (
                    HostShell::default(),
                    LoadStatus::Reset("the version of the host shell file is not one this build reads".into()),
                );
            }
        }
        let mut result = HostShell::default();
        let mut notes = Vec::new();
        match object.get("host_shell") {
            Some(Value::Bool(enabled)) => result.enabled = *enabled,
            Some(_) => notes.push("host_shell is not true or false and was reset to false".to_string()),
            None => notes.push("the file has no host_shell and it was set to false".to_string()),
        }
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (result, status)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        // By hand: the key order is fixed (version first) and the values are plain ASCII that needs no escaping.
        format!("{{\n  \"version\": {},\n  \"host_shell\": {}\n}}\n", self.version, self.enabled)
    }

    /// Reads `<dir>/host_shell.json`.
    ///
    /// There is no error case. A missing file is the default (off) and `Clean`. A file that cannot be used (oversized, not
    /// UTF-8, not JSON, a version this build does not know, a value that is not a boolean) gives off, and its bytes are
    /// copied to `host_shell.json.bad` first so that the next save does not destroy them; an oversized file is moved there
    /// without being read. A valid file is never written.
    pub fn load(dir: &Path) -> (HostShell, LoadStatus) {
        let path = dir.join(FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (HostShell::default(), LoadStatus::Clean),
            Err(e) => {
                let mut reason = format!("cannot read {FILE}: {e}");
                if e.kind() == io::ErrorKind::InvalidData {
                    match fs::rename(&path, bad_path(&path)) {
                        Ok(()) => reason.push_str("; the file was moved to host_shell.json.bad"),
                        Err(e) => reason.push_str(&format!("; and it could not be kept: {e}")),
                    }
                }
                return (HostShell::default(), LoadStatus::Reset(reason));
            }
        };
        let (value, status) = match String::from_utf8(bytes) {
            Ok(text) => HostShell::from_json(&text),
            Err(_) => (HostShell::default(), LoadStatus::Reset(format!("{FILE} is not UTF-8 text"))),
        };
        match status {
            LoadStatus::Clean => (value, LoadStatus::Clean),
            LoadStatus::Reset(reason) => {
                let reason = match backup_bad(&path) {
                    Ok(_) => reason,
                    Err(e) => format!("{reason} (and the damaged file could not be kept: {e})"),
                };
                (value, LoadStatus::Reset(reason))
            }
            LoadStatus::Repaired(mut notes) => {
                if let Err(e) = backup_bad(&path) {
                    notes.push(format!("the original could not be kept: {e}"));
                }
                (value, LoadStatus::Repaired(notes))
            }
        }
    }

    /// Writes `<dir>/host_shell.json` atomically (the parent directory is created).
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(FILE), self.to_json().as_bytes())
    }
}

/// What the person is told, and what decides how a pane starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not in a Flatpak: there is no sandbox to leave, so the group does not exist and nothing changes. It has no word.
    NotApplicable,
    /// In a Flatpak with the permission, and the person has not turned it on: panes start in the sandbox.
    Off,
    /// In a Flatpak with the permission, and the person turned it on: panes start on the host.
    On,
    /// In a Flatpak that was started without the permission: panes can only start in the sandbox, whatever the choice is.
    Unavailable,
}

impl State {
    /// The word of the wire: `off`, `on` or `unavailable`. [`State::NotApplicable`] has none, because the page draws nothing
    /// for it.
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            State::NotApplicable => None,
            State::Off => Some("off"),
            State::On => Some("on"),
            State::Unavailable => Some("unavailable"),
        }
    }

    /// The state for a word of the wire; `None` for any other text (another case, a blank, `not_applicable`).
    pub fn from_word(word: &str) -> Option<State> {
        match word {
            "off" => Some(State::Off),
            "on" => Some(State::On),
            "unavailable" => Some(State::Unavailable),
            _ => None,
        }
    }

    /// Whether a new pane starts on the host through `flatpak-spawn --host`. Only [`State::On`] does.
    pub fn spawns_on_host(self) -> bool {
        self == State::On
    }
}

/// The state of the host shell. Outside a Flatpak it is [`State::NotApplicable`]; in a Flatpak without the permission it is
/// [`State::Unavailable`] whatever the person chose, so `On` is reachable only with the permission present.
pub fn state_of(in_flatpak: bool, permission: bool, enabled: bool) -> State {
    match (in_flatpak, permission, enabled) {
        (false, _, _) => State::NotApplicable,
        (true, false, _) => State::Unavailable,
        (true, true, true) => State::On,
        (true, true, false) => State::Off,
    }
}

/// Whether the choice may be changed in `state`, and the sentence that says why not.
pub fn can_set(state: State) -> Result<(), &'static str> {
    match state {
        State::NotApplicable => Err("Host shell exists only inside a Flatpak."),
        State::Unavailable => Err("This Flatpak has no permission to run commands on the host."),
        State::Off | State::On => Ok(()),
    }
}

/// Whether the text of a `/.flatpak-info` says that this Flatpak may talk to `org.freedesktop.Flatpak` on the session bus:
/// the section `[Session Bus Policy]` holds that name with the value `talk` or `own` (`own` implies `talk`).
///
/// Fail closed: the answer is `false` for an empty text, for text above [`MAX_INFO_BYTES`], for a NUL byte, for any line
/// anywhere in the text that is not blank, a comment, a `[Section]` or a `key=value`, for the name in another section, for a commented line, for
/// the name as the prefix or the suffix of another name, and for any value but `talk` and `own` (`none` among them). The
/// first line of the section that names the bus name decides. A section name and a value match exactly and with the case
/// given here. The format is the INI form of the key file of the desktop world: `#` and `;` start a comment line and there
/// are no escapes. No regular expression and no dependency.
pub fn permission_in(info: &str) -> bool {
    if info.is_empty() || info.len() as u64 > MAX_INFO_BYTES || info.contains('\0') {
        return false;
    }
    let mut in_section = false;
    // The first line of the section that names the bus name decides, but the whole text is read: a structure error
    // anywhere makes the text untrustworthy.
    let mut decided: Option<bool> = None;
    for raw in info.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else { return false };
            in_section = name == BUS_SECTION;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { return false };
        if in_section && decided.is_none() && key.trim() == BUS_NAME {
            decided = Some(matches!(value.trim(), "talk" | "own"));
        }
    }
    decided.unwrap_or(false)
}

//! `local_network.json`: whether this computer has been asked about the Local Network permission of macOS, and the rule
//! that decides when the system may be made to ask.
//!
//! macOS 15 and later let the person allow or deny, per app, the access to computers on the local network. There is no API
//! to ask for it and none to read it: the system shows its own question the first time the app touches the local network,
//! and the only way to read the answer is to touch the local network again. So reading the state is itself something that
//! can raise the question, and holzWave shows its own notice first. This file holds the three facts the core owns:
//!
//! * the **choice** of the person, kept in `local_network.json` ([`LocalNetwork`]): `unasked` (the notice was not shown
//!   yet), `declined` (the person said `Not now`) or `requested` (the person said `Allow`);
//! * the **reading** a probe made of the system ([`Reading`]), and the **state** shown to the person ([`State`]);
//! * the rule that joins them: nothing is probed unless the choice is `requested` ([`state_of`]), and a denial is shown only
//!   by a status read, never by the call that asked ([`state_after_request`]).
//!
//! The file is machine-local and is never synced: `settings.json` travels to every computer, and a second Mac must not
//! inherit "already asked", or its first status read would raise the system question with no notice. A damaged or hostile
//! file costs the choice and never the start: [`LocalNetwork::load`] has no error case. The probe itself lives in the
//! shell and in the `holzwave-local-network` bridge; this file has no network code, no unsafe code and no dependency.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::layout::LoadStatus;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic};

/// The file name inside the app data directory.
pub const FILE: &str = "local_network.json";
/// The format version that is written.
pub const VERSION: u32 = 1;
/// The largest `local_network.json` that is read. The file is a few dozen bytes.
pub const MAX_BYTES: u64 = 4096;

/// What the person decided on this computer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    /// The notice was not answered yet. The notice is due on a system that has the setting.
    #[default]
    Unasked,
    /// The person chose `Not now` (or pressed Escape, or clicked outside). Nothing asks again by itself.
    Declined,
    /// The person chose `Allow`: holzWave may touch the local network, which makes the system show its own question.
    Requested,
}

impl Choice {
    /// The word in the file.
    pub fn as_str(self) -> &'static str {
        match self {
            Choice::Unasked => "unasked",
            Choice::Declined => "declined",
            Choice::Requested => "requested",
        }
    }

    /// The choice for a word of the file; `None` for any other text (another case, a blank, an unknown word).
    pub fn from_word(word: &str) -> Option<Choice> {
        match word {
            "unasked" => Some(Choice::Unasked),
            "declined" => Some(Choice::Declined),
            "requested" => Some(Choice::Requested),
            _ => None,
        }
    }

    /// The choice after the person answered the notice. `Allow` always gives `Requested`. `Not now` gives `Declined` only
    /// from `Unasked`: a `Requested` stays `Requested`, because the system has been asked already and cannot be un-asked.
    pub fn answered(self, allow: bool) -> Choice {
        match (allow, self) {
            (true, _) => Choice::Requested,
            (false, Choice::Unasked) => Choice::Declined,
            (false, other) => other,
        }
    }
}

/// The content of `local_network.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalNetwork {
    pub version: u32,
    pub choice: Choice,
}

impl Default for LocalNetwork {
    fn default() -> Self {
        LocalNetwork { version: VERSION, choice: Choice::Unasked }
    }
}

impl LocalNetwork {
    /// Reads the document. There is no error case: text that is not JSON or not an object is `Reset`; a missing, mistyped
    /// or unknown `choice` costs the choice (it reads `unasked`) and the result is `Repaired`. Unknown keys are ignored.
    pub fn from_json(text: &str) -> (LocalNetwork, LoadStatus) {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => return (LocalNetwork::default(), LoadStatus::Reset(format!("not a local network file: {e}"))),
        };
        let Value::Object(object) = value else {
            return (
                LocalNetwork::default(),
                LoadStatus::Reset("not a local network file: the top level is not an object".into()),
            );
        };
        let mut result = LocalNetwork::default();
        let mut notes = Vec::new();
        match object.get("version") {
            None => {}
            Some(version) => match version.as_u64().and_then(|v| u32::try_from(v).ok()) {
                Some(number) => result.version = number,
                None => notes.push("the version is not a number and was set to the current one".to_string()),
            },
        }
        match object.get("choice") {
            Some(Value::String(word)) => match Choice::from_word(word) {
                Some(choice) => result.choice = choice,
                None => notes.push("the choice is not one of unasked, declined and requested and was reset".to_string()),
            },
            Some(_) => notes.push("the choice is not text and was reset".to_string()),
            None => notes.push("the file has no choice and it was set to unasked".to_string()),
        }
        let status = if notes.is_empty() { LoadStatus::Clean } else { LoadStatus::Repaired(notes) };
        (result, status)
    }

    /// The document as pretty printed JSON with a trailing newline.
    pub fn to_json(&self) -> String {
        // By hand: the key order is fixed (version first) and the words are plain ASCII that needs no escaping.
        format!("{{\n  \"version\": {},\n  \"choice\": \"{}\"\n}}\n", self.version, self.choice.as_str())
    }

    /// Reads `<dir>/local_network.json`.
    ///
    /// There is no error case. A missing file is the default and `Clean`. A file that cannot be used (oversized, not UTF-8,
    /// not JSON, an unknown word) gives `unasked`, and its bytes are copied to `local_network.json.bad` first so that the
    /// next save does not destroy them; an oversized file is moved there without being read. A valid file is never written.
    pub fn load(dir: &Path) -> (LocalNetwork, LoadStatus) {
        let path = dir.join(FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (LocalNetwork::default(), LoadStatus::Clean),
            Err(e) => {
                let mut reason = format!("cannot read {FILE}: {e}");
                if e.kind() == io::ErrorKind::InvalidData {
                    match fs::rename(&path, bad_path(&path)) {
                        Ok(()) => reason.push_str("; the file was moved to local_network.json.bad"),
                        Err(e) => reason.push_str(&format!("; and it could not be kept: {e}")),
                    }
                }
                return (LocalNetwork::default(), LoadStatus::Reset(reason));
            }
        };
        let (value, status) = match String::from_utf8(bytes) {
            Ok(text) => LocalNetwork::from_json(&text),
            Err(_) => (LocalNetwork::default(), LoadStatus::Reset(format!("{FILE} is not UTF-8 text"))),
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

    /// Writes `<dir>/local_network.json` atomically (the parent directory is created).
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic(&dir.join(FILE), self.to_json().as_bytes())
    }
}

/// What one probe of the system saw. The shell maps the reading of the bridge crate to this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Allowed,
    Denied,
    Unknown,
}

/// What the person is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The system was not asked: the choice is not `requested`, so nothing touched the network.
    NotAsked,
    Allowed,
    Denied,
    /// The probe could not tell: no network, a failed browse, or the system question is still open.
    Unknown,
}

impl State {
    /// The word of the wire: `not_asked`, `allowed`, `denied` or `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            State::NotAsked => "not_asked",
            State::Allowed => "allowed",
            State::Denied => "denied",
            State::Unknown => "unknown",
        }
    }
}

impl From<Reading> for State {
    fn from(reading: Reading) -> State {
        match reading {
            Reading::Allowed => State::Allowed,
            Reading::Denied => State::Denied,
            Reading::Unknown => State::Unknown,
        }
    }
}

/// The state for a status read. `probe` touches the local network, which can make the system show its question, so it
/// runs only when the choice is `requested`; with `unasked` or `declined` the answer is `NotAsked` and `probe` is never
/// called.
pub fn state_of(choice: Choice, probe: impl FnOnce() -> Reading) -> State {
    match choice {
        Choice::Requested => probe().into(),
        Choice::Unasked | Choice::Declined => State::NotAsked,
    }
}

/// The state right after the person pressed `Allow` and one probe ran. A denial is never reported here: the system may
/// refuse a request before the person has answered its question (TN3179), so `Denied` becomes `Unknown`, and only a later
/// status read can say `denied`.
pub fn state_after_request(reading: Reading) -> State {
    match reading {
        Reading::Allowed => State::Allowed,
        Reading::Denied | Reading::Unknown => State::Unknown,
    }
}

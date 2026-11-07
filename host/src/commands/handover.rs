//! The plain functions of the two commands that hand one file over between the phone's document dialog and the shell's
//! folder: `handover_in` and `handover_out`.
//!
//! A host with no document dialog (the desktop, the server, a test) or with no private folder for the shell answers
//! `unavailable`, an outcome the page reads like any other. Every other answer is one of four fixed kinds: `done`,
//! `cancelled`, `too_large` and `failed`. A failure never carries text: a provider's words or a path would reach the page.
//! The only text that does is a file name, and it is the cleaned one (see [`crate::handover::clean_name`]).
//!
//! Both commands wait for the person in the system's dialog, so their rows run off the main thread.

use serde::Serialize;

use crate::handover::{self, Outcome};
use crate::state::AppCore;

/// What a hand-over came to, as the page reads it. Tagged by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoverOutcomeMsg {
    /// The file crossed. `name` is the cleaned name of the chosen file and `shown` the text for the place it has now
    /// (`~/handover/report-2.pdf` when `report.pdf` was taken); `renamed` says a number was added.
    Done { name: String, size: u64, shown: String, renamed: bool },
    /// The person left the dialog; nothing changed.
    Cancelled,
    /// The file is larger than the limit; the size is there when it is known.
    TooLarge { name: String, size: Option<u64> },
    /// Anything else, with no text.
    Failed,
    /// This build has no document dialog.
    Unavailable,
}

impl From<Outcome> for HandoverOutcomeMsg {
    fn from(outcome: Outcome) -> HandoverOutcomeMsg {
        match outcome {
            Outcome::Done { name, size, shown, renamed } => HandoverOutcomeMsg::Done { name, size, shown, renamed },
            Outcome::Cancelled => HandoverOutcomeMsg::Cancelled,
            Outcome::TooLarge { name, size } => HandoverOutcomeMsg::TooLarge { name, size },
            Outcome::Failed => HandoverOutcomeMsg::Failed,
        }
    }
}

/// Copy one file the person picks in the system's dialog into `~/handover/` of the shell's folder.
pub fn handover_in(core: &AppCore) -> HandoverOutcomeMsg {
    let host = core.host();
    match (host.private_dirs(), host.file_picker()) {
        (Some(dirs), Some(picker)) => handover::import(&dirs.home, picker.as_ref()).into(),
        _ => HandoverOutcomeMsg::Unavailable,
    }
}

/// Copy the file at `path` (from the shell's folder) to a place the person names in the system's dialog. A path that is
/// not a regular file inside the folder answers `failed` and the dialog is never shown.
pub fn handover_out(core: &AppCore, path: String) -> HandoverOutcomeMsg {
    let host = core.host();
    match (host.private_dirs(), host.file_picker()) {
        (Some(dirs), Some(picker)) => handover::export(&dirs.home, &path, picker.as_ref()).into(),
        _ => HandoverOutcomeMsg::Unavailable,
    }
}

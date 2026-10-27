//! The plain functions of the commands that work on one pane by its id: the keyboard, the pointer, the selection, the
//! search and copy mode. Pane ids are global, so none of them needs a window.

use holzwave_core::layout::PaneId;

use super::wire::PasteOutcomeMsg;
use crate::state::AppCore;

/// The selected text, `null` when nothing is selected. Waits for the engine's answer.
pub fn selection_text(core: &AppCore, pane: PaneId) -> Result<Option<String>, String> {
    core.selection_text(pane)
}

/// Paste clipboard text. Waits for the engine's answer.
pub fn paste(core: &AppCore, pane: PaneId, text: String, allow_unsafe: bool) -> Result<PasteOutcomeMsg, String> {
    core.paste(pane, text, allow_unsafe).map(PasteOutcomeMsg::from)
}

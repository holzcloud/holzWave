//! The plain functions of the commands that work on one pane by its id: the keyboard, the pointer, the selection, the
//! search and copy mode. Pane ids are global, so none of them needs a window.

use holzwave_core::engine::{DomKey, Mods};
use holzwave_core::layout::PaneId;
use holzwave_core::wire::SearchMsg;

use super::wire::{
    CopyOpMsg, CopyStateMsg, EditActionMsg, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg, SearchOpMsg, SelectOpMsg,
};
use crate::state::AppCore;

/// Start the shell of a pane if it has none yet. Waits for the actor's startup handshake.
pub fn start_pane(
    core: &AppCore,
    pane: PaneId,
    cols: u32,
    rows: u32,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
) -> Result<(), String> {
    core.start_pane(pane, cols, rows, cell_w.unwrap_or(0), cell_h.unwrap_or(0))
}

/// A key press or release as the page saw it (`mods` is SHIFT 1, ALT 2, CTRL 4, SUPER 8). The engine encodes it.
#[allow(clippy::too_many_arguments)]
pub fn key(
    core: &AppCore,
    pane: PaneId,
    code: String,
    key: String,
    mods: u32,
    repeat: bool,
    release: bool,
    composing: bool,
) -> Result<(), String> {
    let dom = DomKey { code, key, mods: Mods::from_bits((mods & 0xFF) as u8), repeat, release, composing };
    core.key(pane, dom)
}

/// A line-editing step (word or line jump, delete word or line) for the shell's line editor. The page sends the
/// name of the step, never bytes. It asks for this only on the normal screen; a program on the alternate screen
/// gets the key through `key`.
pub fn send_edit(core: &AppCore, pane: PaneId, action: EditActionMsg) -> Result<(), String> {
    core.edit(pane, action.into())
}

/// Committed text (an IME commit, a dead-key result) as input.
pub fn text(core: &AppCore, pane: PaneId, text: String) -> Result<(), String> {
    core.text(pane, text)
}

/// Paste clipboard text. Waits for the engine's answer.
pub fn paste(core: &AppCore, pane: PaneId, text: String, allow_unsafe: bool) -> Result<PasteOutcomeMsg, String> {
    core.paste(pane, text, allow_unsafe).map(PasteOutcomeMsg::from)
}

/// Tell the program of a pane that something that is not text was pasted (an image on the clipboard): an empty bracketed
/// paste when the program turned on bracketed paste, nothing otherwise. The bytes are fixed in the core, so no argument
/// carries content. Waits for the engine's answer.
pub fn paste_cue(core: &AppCore, pane: PaneId) -> Result<PasteOutcomeMsg, String> {
    core.paste_cue(pane).map(PasteOutcomeMsg::from)
}

pub fn resize(
    core: &AppCore,
    pane: PaneId,
    cols: u32,
    rows: u32,
    cell_w: Option<u32>,
    cell_h: Option<u32>,
) -> Result<(), String> {
    core.resize(pane, cols, rows, cell_w.unwrap_or(0), cell_h.unwrap_or(0))
}

pub fn scroll(core: &AppCore, pane: PaneId, rows: i32) -> Result<(), String> {
    core.scroll(pane, rows)
}

pub fn scroll_to_bottom(core: &AppCore, pane: PaneId) -> Result<(), String> {
    core.scroll_to_bottom(pane)
}

/// Make the next frame of the pane a full one (the page reconnected or saw a gap in the sequence numbers).
pub fn request_full(core: &AppCore, pane: PaneId) -> Result<(), String> {
    core.request_full(pane)
}

/// One piece of an image's pixels as raw bytes: four big-endian `u32` (width, height, total length in bytes, offset) and
/// then at most 512 KiB of straight RGBA from `offset` on. Fails with `no such image` when the pane, the image or that
/// generation is gone, so a page never draws the pixels of another picture than the one it asked for.
pub fn image_data(core: &AppCore, pane: PaneId, image: u32, generation: u64, offset: u64) -> Result<Vec<u8>, String> {
    core.image_data(pane, image, generation, offset)
}

/// Whether pictures may animate: the page's reduced-motion fact, for every pane now and the ones that start later.
pub fn set_image_animation(core: &AppCore, enabled: bool) {
    core.set_image_animation(enabled);
}

pub fn select(core: &AppCore, pane: PaneId, op: SelectOpMsg) -> Result<(), String> {
    core.select(pane, op.into())
}

/// The selected text, `null` when nothing is selected. Waits for the engine's answer.
pub fn selection_text(core: &AppCore, pane: PaneId) -> Result<Option<String>, String> {
    core.selection_text(pane)
}

/// Change the search and answer with its state. Waits for the engine's answer.
pub fn search(core: &AppCore, pane: PaneId, op: SearchOpMsg) -> Result<SearchMsg, String> {
    let state = core.search(pane, op.into())?;
    Ok(SearchMsg { total: state.total, current: state.current })
}

/// Run a copy-mode operation and answer with its state. Waits for the engine's answer.
pub fn copy_mode(core: &AppCore, pane: PaneId, op: CopyOpMsg) -> Result<CopyStateMsg, String> {
    core.copy_mode(pane, op.into()).map(CopyStateMsg::from)
}

/// A pointer press, release or motion at `x`, `y` device pixels from the origin of the pane's grid; `mods` is
/// SHIFT 1, ALT 2, CTRL 4, SUPER 8. Hot path: bounds the numbers and pushes to the pane's queue, nothing else, so
/// it is synchronous and never waits.
pub fn mouse(
    core: &AppCore,
    pane: PaneId,
    action: MouseActionMsg,
    button: MouseButtonMsg,
    x: f64,
    y: f64,
    mods: u32,
) {
    core.mouse(pane, action.into(), button.into(), x, y, mods);
}

/// A wheel event of `rows` rows (negative towards older output) at `x`, `y`. Hot path like `mouse`.
pub fn wheel(core: &AppCore, pane: PaneId, rows: i32, x: f64, y: f64, mods: u32) {
    core.wheel(pane, rows, x, y, mods);
}

/// What `copy_selection` answers in the browser build: `false`, whatever the pane has selected. The desktop copies the
/// core's own selection to the system clipboard of the computer the app runs on; a server has no clipboard of the
/// person, so the page copies with the browser's clipboard (from `selection_text`) and this command copies nothing.
pub fn copy_selection_web(_core: &AppCore, _pane: PaneId) -> Result<bool, String> {
    Ok(false)
}

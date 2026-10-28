//! The commands the webview can call: thin wrappers over [`AppCore`] (D-19). The terminal, layout, window, pointer,
//! settings, connection, block, file, history, sync and local network commands are rows of the host's command table
//! (`holzwave_host::commands::table`) and their wrappers are written from it in `desktop_table.rs`; the three of them
//! that need the window shell (`copy_selection`, `clipboard_text`, `window_ready`) and the AI and integration commands
//! are written out here.
//!
//! Two conventions hold for every command, and `state.rs` has a test that reads this file and the table to keep them:
//!
//! - **snake_case argument names.** The command macro of the pinned tauri-macros turns `allow_unsafe` into
//!   `allowUnsafe` for the page unless `rename_all = "snake_case"` says otherwise, which would make `allow_unsafe`,
//!   `cell_w` and `cell_h` silently disappear. Every command carries the rename.
//! - **the calling window.** A command that works on a layout takes `window: WebviewWindow`, which Tauri fills in
//!   with the window that made the call (the page does not send it, so it is no argument of the protocol), and
//!   answers with that window's view. A command that names a pane by id (`key`, `resize`, ...) needs no window: pane
//!   ids are global. The label of a window is the key of its layout; an unknown one is the error `no such window`.
//! - **no waiting on the main thread.** A command without `async` runs on the main thread (RESEARCH Pitfall 10), so
//!   those only lock, mutate and push to a queue. A command that waits for the engine thread's answer (up to two
//!   seconds) or for a shell to start is `async`, which runs it off the main thread.
//!
//! The wire op types (`holzwave_host::commands::wire`) are internally tagged with `kind` and every variant, motion and
//! mode name is snake_case, so the page sends `{kind: "begin", x, y, mode: "word"}` and `{kind: "move", motion:
//! "line_start"}`.

use std::collections::BTreeMap;
use std::sync::Arc;

use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::ai::service::{AskView, ContextState, OfferView};
use holzwave_core::ai::types::ContextKind;
use holzwave_core::integrate::service::{ProfilesView, RunAnswer, SelectionView, ToolView};
use holzwave_core::integrate::ticket::Card;
use holzwave_core::integrate::Selection;
use holzwave_core::layout::PaneId;
use serde_json::Value;
use tauri::{State, WebviewWindow};

use holzwave_host::ai_host::{AiFault, ConversationList, ProfileInputMsg, RemovalView, TicketView};
use crate::clipboard;
use holzwave_host::integrate_host::{ConfirmAnswer, ContextAnswer, IntegrationFault, LaunchAnswer};
use holzwave_host::state::AppCore;

// The wire types of the table's commands live in the host crate. The protocol golden and the tests of the shell import
// them from here.
#[allow(unused_imports)]
pub use holzwave_host::commands::wire::{
    CopyMotionMsg, CopyOpMsg, CopyStateMsg, EditActionMsg, GrantAnswer, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg,
    SearchOpMsg, SelectModeMsg, SelectOpMsg,
};

// ---- the wrappers that need the window shell ---------------------------------------------------------------------

/// Copy the core's own selection of a pane to the system clipboard: `true` when text was written, `false` when the pane
/// has no selection or an empty one. The text comes from the core and never from the page, so this command is not a
/// clipboard write primitive: a script in the page can ask for the selection to be copied, but never choose what is
/// copied. Waits for the engine's answer.
#[tauri::command(async, rename_all = "snake_case")]
pub fn copy_selection(core: State<Arc<AppCore>>, pane: PaneId) -> Result<bool, String> {
    match core.selection_text(pane)? {
        Some(text) if !text.is_empty() => {
            clipboard::write_text(&text)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The text on the system clipboard (the empty string when it holds none), for the page's paste fallback. The page calls it
/// only after the person's own paste chord, when the web view's `paste` event did not come, and the text then goes through
/// the one `paste` path (sanitised, bracketed when asked, confirmed when it spans lines). An error means this platform has
/// no native clipboard access and the page reads the web view's clipboard instead. Waits for the system.
#[tauri::command(async, rename_all = "snake_case")]
pub fn clipboard_text() -> Result<String, String> {
    clipboard::read_text()
}

/// The page of the calling window has applied its first layout: it is told the last title and directory of its
/// panes, and a window that was created hidden is shown and focused. The first half is the table's function; showing
/// the window is the shell's.
#[tauri::command(async, rename_all = "snake_case")]
pub fn window_ready(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<(), String> {
    holzwave_host::commands::layout::window_ready(&core, window.label())?;
    if !window.is_visible().map_err(|e| e.to_string())? {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---- the AI chat -------------------------------------------------------------------------------------------------
//
// A refusal of these commands reaches the page as the object `{code, reason, text}` (`AiFault`), not as a string. The
// events of a turn arrive as the event `ai` for every window; the page routes them by conversation.

/// Send the typed `text` to the chat `conversation`, as a message of the user in `pane`. The answer is the ticket of
/// the turn, at once; the reply arrives as `ai` events, and anything that goes wrong later arrives as the end of the
/// turn. `profile` and `model` may be null or empty: the chat then uses the provider and model it is pinned to. Only
/// the typed text leaves: the command has no argument that could carry terminal content or a decision. A pane that is
/// not open is refused as `no_such_pane`.
///
/// `offer` and `picked` are both given or both left out. With both, the message carries the items of an offer that
/// `ai_context_preview` made for this chat, this pane and this route: the page names the offer and the numbers of the
/// items it picked, never their text. With one of them only, the call is refused as `context` / `bad_pick` and nothing
/// is stored.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_send(
    core: State<Arc<AppCore>>,
    conversation: String,
    text: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    offer: Option<String>,
    picked: Option<Vec<u32>>,
) -> Result<TicketView, AiFault> {
    core.ai_send_with(&conversation, text, pane, profile, model, offer, picked)
}

/// What `pane` offers for the chat and where it would go: the profile and model of the call (the chat's own when
/// they are null or empty), the host and where it is (`on_device`, `loopback`, `lan` or `cloud`), whether the pane
/// must be shared first (`required`), whether it is (`shared`), the kinds the pane offers now and whether the chat
/// file keeps the text of a context. Nothing is read from the pane, and there is no text in the answer. This looks up
/// the name of a host once, so call it for an act of the user and not on a mount. A refusal is the fault object with
/// the code `context`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_state(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
) -> Result<ContextState, AiFault> {
    core.ai_context_state(&conversation, pane, profile, model)
}

/// Share `pane` for the chat on this route (`shared` true), or end the share (false). A share is the permission to
/// read the pane into an offer; nothing is read and nothing is attached to a later message. A change of provider,
/// model or address, and the end of the chat or of the pane, end the share. The answer is the state, as for
/// `ai_context_state`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_share(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    shared: bool,
) -> Result<ContextState, AiFault> {
    core.ai_context_share(&conversation, pane, profile, model, shared)
}

/// Read the `kinds` (`selection`, `screen`, `last_command`, `file`, `preview`) of a shared `pane` into an offer. The
/// answer carries the id of the offer, where it would go, and for each item its number, kind, label, size and what the
/// cleaner removed: labels, sizes and findings, never text. The chat holds one offer at a time and it lives five
/// minutes. A refusal is the fault object with the code `context` and one of its stable reasons.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_context_preview(
    core: State<Arc<AppCore>>,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    kinds: Vec<ContextKind>,
) -> Result<OfferView, AiFault> {
    core.ai_context_preview(&conversation, pane, profile, model, kinds)
}

/// A fresh chat id. No file is made until the first message is sent.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_new(core: State<Arc<AppCore>>) -> Result<String, AiFault> {
    core.ai().conversation_new()
}

/// The saved chats, newest first, and one note for each file that could not be listed.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_list(core: State<Arc<AppCore>>) -> Result<ConversationList, AiFault> {
    core.ai().conversation_list()
}

/// Delete a chat and stop its turn. Answers whether a file was removed.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_conversation_delete(core: State<Arc<AppCore>>, conversation: String) -> Result<bool, AiFault> {
    core.ai().conversation_delete(&conversation)
}

/// Stop the turn of a chat; it keeps the text it had. Answers whether the chat had a turn.
#[tauri::command(rename_all = "snake_case")]
pub fn ai_cancel(core: State<Arc<AppCore>>, conversation: String) -> Result<bool, AiFault> {
    core.ai().cancel(&conversation)
}

/// The person's verdict on one approval card: the chat, the call, `allow` or `deny`, and the hash of the card they were
/// shown. It answers nothing; what follows arrives as `ai` events (the result of the call, or the end of the turn). A
/// refusal is the fault object with the code `tool` and the stable code of the error as the reason (`hash_mismatch`,
/// `unknown_call`, `not_pending`, ...), or `invalid_decision` for a verdict that is neither word. A hash that does not
/// match leaves the card waiting.
///
/// This is the one command that decides a call. It exists for the webview only: the local socket of `wsh` has no
/// counterpart, and nothing else reaches the approval table.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_decide(
    core: State<Arc<AppCore>>,
    conversation: String,
    call_id: String,
    decision: String,
    hash: String,
) -> Result<(), AiFault> {
    core.ai_decide(&conversation, &call_id, &decision, &hash)
}

/// The person's press on `Undo` for one finished write: the chat and the call. It answers nothing. The core puts the old text
/// of the file back only while the file still holds exactly what the assistant wrote, and outside the deny list; a refusal
/// is the fault object with the code `undo` and a stable reason (`unknown_change`, `created`, `file_changed`, `denied`,
/// `unreadable`) and a fixed sentence.
///
/// This command exists for the webview only and is not a verdict on a tool call: it never reaches the approval table, and it
/// carries two ids and no text.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_undo(core: State<Arc<AppCore>>, conversation: String, call_id: String) -> Result<(), AiFault> {
    core.ai_undo(&conversation, &call_id)
}

/// The requests of `wsh ai` that wait for an answer in the calling window, oldest first: a page that loaded after the
/// request was made (a reload, a window that opened late) asks for the cards it missed. It reads an in-memory table and
/// touches no keychain, file or network, and it holds no text but the prompt each card shows.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_ask_pending(core: State<Arc<AppCore>>, window: WebviewWindow) -> Result<Vec<AskView>, AiFault> {
    Ok(core.ai_ask_pending(window.label()))
}

/// The person's press on a card of `wsh ai`: the request, `send` or `refuse`, and the seal the card carried. It answers
/// nothing; the answer of the model streams back to the program that asked. A refusal is the fault object with the code
/// `ask` and a stable reason (`unknown_ask`, `seal_mismatch`, `bad_verdict`) and a fixed sentence.
///
/// This command exists for the webview only: it releases one waiting request, for its own seal, and the local socket of
/// `wsh` has no way to reach it.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_ask_answer(core: State<Arc<AppCore>>, ask: String, verdict: String, seal: String) -> Result<(), AiFault> {
    core.ai_ask_answer(&ask, &verdict, &seal)
}

/// Make a profile or change one. Answers its id. The input has no member for a key.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_profile_save(core: State<Arc<AppCore>>, input: ProfileInputMsg) -> Result<String, AiFault> {
    core.ai().profile_save(input)
}

/// Remove a profile and its keys, and stop the turns that use it.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_profile_remove(core: State<Arc<AppCore>>, profile: String) -> Result<RemovalView, AiFault> {
    core.ai().profile_remove(&profile)
}

/// Remove a key from the system keychain and stop the turns that use the profile. Answers nothing.
#[tauri::command(async, rename_all = "snake_case")]
pub fn ai_key_clear(core: State<Arc<AppCore>>, profile: String, slot: KeySlot) -> Result<(), AiFault> {
    core.ai().key_clear(&profile, slot)
}

// ---- the integrations ----------------------------------------------------------------------------------------------
//
// The page sends facts (a tool, a verb, a pane, names, a selection) and the core builds everything else: no command takes
// a program, an argument list or a shell string. A refusal reaches the page as the object `{code, reason, text}`
// (`IntegrationFault`). Nothing of this exists on the local socket of `wsh`.

/// One read of a verb that only reads and shows a view, for `pane` (or for the tool's own selection when `pane` is null).
/// `args` is an object of strings; the core checks each value by the kind of the verb's argument. The answer is the view
/// model, the selection the read used and the time it was read; a refusal is the fault object. It starts the tool, so it
/// is async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_run(
    core: State<Arc<AppCore>>,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
) -> Result<RunAnswer, IntegrationFault> {
    core.integration_run(&tool, &verb, pane, &args)
}

/// The selection of a pane (or the pick of the tool, which new panes start from, when `pane` is null): the context, the
/// namespace, the subscription, the section, the node and the profile, as far as the tool takes them. The core checks it
/// against the tool and its profile and answers it as it will be used (the pins of the profile filled in) with the name of
/// the production profile, or `null`. A selection the tool refuses is the fault object and changes nothing. One
/// `integration` event follows a selection that was accepted.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_select(
    core: State<Arc<AppCore>>,
    tool: String,
    pane: Option<PaneId>,
    selection: Selection,
) -> Result<SelectionView, IntegrationFault> {
    core.integration_select(&tool, pane, &selection)
}

/// What the tools are set to and which panes have a selection: `{rows, panes}`. `rows` has one entry for each tool (its
/// state, and what is selected for `pane` or for the tool), `panes` one for each pane with an integration selection (an
/// integration block, or the terminal a launch opened), with the status of its tool when the pane holds the tool's last
/// words. With `refresh` the tools are looked for again and their default verbs are read, which starts them, so it is async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_context(core: State<Arc<AppCore>>, pane: Option<PaneId>, refresh: bool) -> ContextAnswer {
    core.integration_context(pane, refresh)
}

/// The card for a verb that changes something or opens a session: the ticket, the tool, the verb, what it does to the
/// world, the program's name and every argument spelled out, the target, the guard line, every field the hash covers and
/// the hash. Nothing is started. A verb that only reads is refused (it needs no card). The page shows the card as it is
/// and answers it with `integration_confirm` or closes it with `integration_cancel`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_prepare(
    core: State<Arc<AppCore>>,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
) -> Result<Card, IntegrationFault> {
    core.integration_prepare(&tool, &verb, pane, &args)
}

/// The person's press on a card: the ticket and the hash of the card they were shown. It is the one integration command
/// that decides, and the one that names a ticket and a hash together; it exists for the webview only, and the local socket
/// of `wsh` has no counterpart. A verb that changes a file, a cluster or an account runs once (`kind` `ran`, with the
/// tool's own first line of output as `note`); an exec verb opens a pane (`kind` `launched`, with `pane`, its name as
/// `note` and the new layout as `view`) that the page starts. A ticket that is unknown, old, answered with another hash,
/// or no longer what the card showed is the fault object with the code `ticket`, and nothing runs.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_confirm(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    ticket: String,
    hash: String,
) -> Result<ConfirmAnswer, IntegrationFault> {
    core.integration_confirm(window.label(), &ticket, &hash)
}

/// The person closed a card without answering it. The ticket is dropped; a ticket that is not there is no problem.
#[tauri::command(rename_all = "snake_case")]
pub fn integration_cancel(core: State<Arc<AppCore>>, ticket: String) -> Result<(), IntegrationFault> {
    core.integration_cancel(&ticket);
    Ok(())
}

/// A terminal for a verb that only reads and opens one (logs, kernel messages, a shell): a pane in a tab or beside the
/// focused one (`placement` is `tab`, `right` or `down`), with the launch registered before the page starts it. The answer
/// is the new `pane` and the layout it is in, as `block_open` answers. A verb that changes something or opens a session
/// with the person's credentials is refused here and needs a card. It may read the tool once for the shell's overlay, so it
/// is async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_launch(
    core: State<Arc<AppCore>>,
    window: WebviewWindow,
    tool: String,
    verb: String,
    pane: Option<PaneId>,
    args: BTreeMap<String, String>,
    placement: String,
) -> Result<LaunchAnswer, IntegrationFault> {
    core.integration_launch(window.label(), &tool, &verb, pane, &args, &placement)
}

/// Makes `path` the program of `tool`. The person chooses which `kubectl` runs and not which program: the path must be
/// absolute, its file name must be one of the tool's own names, and the file must be one the core finds usable (an
/// executable file that no one but the person's group can replace). Answers the four tools again; a path that does not
/// pass is the fault object. It asks the tool for its version, so it is async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_binary_set(
    core: State<Arc<AppCore>>,
    tool: String,
    path: String,
) -> Result<Vec<ToolView>, IntegrationFault> {
    core.integration_binary_set(&tool, &path)
}

/// Forgets the program the person chose for `tool`; the search of the core applies again. Answers the four tools again.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_binary_clear(core: State<Arc<AppCore>>, tool: String) -> Result<Vec<ToolView>, IntegrationFault> {
    core.integration_binary_clear(&tool)
}

/// The profiles and the notes about the files that could not be used. A profile lists its secret variables by name, each
/// with whether the keychain has a value (`has_value`): a yes or a no, never the value. It asks the keychain, so it is
/// async.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_profiles(core: State<Arc<AppCore>>) -> ProfilesView {
    core.integration_profiles()
}

/// Adds a profile or replaces the one with the same id. `profile` has the shape of the profiles file: `secret_env` is a
/// list of names and `env` an object of plain values; the rules of the loader decide. A profile that does not pass is the
/// fault object with the code `profile`, naming the profile and the field and never a value. Answers the profiles again.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_profile_save(core: State<Arc<AppCore>>, profile: Value) -> Result<ProfilesView, IntegrationFault> {
    core.integration_profile_save(profile)
}

/// Removes a profile and the values of its secret variables from the keychain. Answers the profiles again.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_profile_remove(core: State<Arc<AppCore>>, id: String) -> Result<ProfilesView, IntegrationFault> {
    core.integration_profile_remove(&id)
}

/// Stores the value of one secret variable of a profile in the system keychain. It is the one integration command that
/// takes a value, and it answers nothing: a value goes in and never comes out. The name must be one the profile declares in
/// `secret_env` and the value 1 to 1024 bytes; anything else is the fault object with the code `profile`.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_secret_set(
    core: State<Arc<AppCore>>,
    profile: String,
    name: String,
    value: String,
) -> Result<(), IntegrationFault> {
    core.integration_secret_set(&profile, &name, value)
}

/// Removes the value of one secret variable of a profile from the keychain. Answers nothing.
#[tauri::command(async, rename_all = "snake_case")]
pub fn integration_secret_clear(core: State<Arc<AppCore>>, profile: String, name: String) -> Result<(), IntegrationFault> {
    core.integration_secret_clear(&profile, &name)
}

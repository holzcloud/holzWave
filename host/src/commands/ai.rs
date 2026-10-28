//! The plain functions of the commands of the assistant (the AI chat). A refusal of these commands is the object
//! `{code, reason, text}` ([`AiFault`]) and not a string; the events of a turn go to every window as the event `ai`, and
//! the page routes them by conversation. Keys live in the store the host gives the core: a key goes in with `ai_key_set`
//! and never comes out, and a store that is locked says `secrets are locked`.

use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::ai::service::events::ChatState;
use holzwave_core::ai::service::{AskView, ContextState, OfferView};
use holzwave_core::ai::types::ContextKind;
use holzwave_core::layout::PaneId;

use crate::ai_host::{AiFault, AiProfiles, ConversationList, ProfileInputMsg, RemovalView, TicketView};
use crate::state::AppCore;

/// The state of a chat right now: what a page that saw every `ai` event of it holds. The page asks for it when it
/// opens a chat and when it misses an event.
pub fn ai_turn_snapshot(core: &AppCore, conversation: String) -> Result<ChatState, AiFault> {
    core.ai().snapshot(&conversation)
}

/// The profiles with the state of each key (never a key). This reads the keychain, so the settings dialog calls it
/// when it opens and not before.
pub fn ai_profiles(core: &AppCore) -> AiProfiles {
    core.ai().profiles()
}

/// Save a key in the secret store: it is written and never read back. A profile's address is part of the saved record, so
/// a key is used only for the address it was saved for. Answers nothing.
pub fn ai_key_set(core: &AppCore, profile: String, slot: KeySlot, api_key: String) -> Result<(), AiFault> {
    core.ai().key_set(&profile, slot, api_key)
}

/// Whether a usable key is saved for the profile. Answers a flag, never the key.
pub fn ai_has_key(core: &AppCore, profile: String, slot: KeySlot) -> Result<bool, AiFault> {
    core.ai().has_key(&profile, slot)
}

/// Start a turn of the assistant in a chat. It answers the ticket of the turn, at once; the reply arrives as `ai` events,
/// and anything that goes wrong later arrives as the end of the turn. `profile` and `model` may be null or empty: the chat
/// then uses the provider and model it is pinned to. Only the typed text leaves: the command has no argument that could
/// carry terminal content or a decision. A pane that is not open is refused as `no_such_pane`.
///
/// `offer` and `picked` are both given or both left out. With both, the message carries the items of an offer that
/// `ai_context_preview` made for this chat, this pane and this route: the page names the offer and the numbers of the
/// items it picked, never their text. With one of them only, the call is refused as `context` / `bad_pick` and nothing is
/// stored. The text may be as long as the largest argument (8 MiB), and the core bounds it again.
#[allow(clippy::too_many_arguments)]
pub fn ai_send(
    core: &AppCore,
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

/// What `pane` offers for the chat and where it would go: the profile and model of the call (the chat's own when they are
/// null or empty), the host and where it is (`on_device`, `loopback`, `lan` or `cloud`), whether the pane must be shared
/// first (`required`), whether it is (`shared`), the kinds the pane offers now and whether the chat file keeps the text of
/// a context. Nothing is read from the pane, and there is no text in the answer. This looks up the name of a host once, so
/// call it for an act of the user and not on a mount. A refusal is the fault object with the code `context`.
pub fn ai_context_state(
    core: &AppCore,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
) -> Result<ContextState, AiFault> {
    core.ai_context_state(&conversation, pane, profile, model)
}

/// Share `pane` for the chat on this route (`shared` true), or end the share (false). A share is the permission to read
/// the pane into an offer; nothing is read and nothing is attached to a later message. A change of provider, model or
/// address, and the end of the chat or of the pane, end the share. The answer is the state, as for `ai_context_state`.
pub fn ai_context_share(
    core: &AppCore,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    shared: bool,
) -> Result<ContextState, AiFault> {
    core.ai_context_share(&conversation, pane, profile, model, shared)
}

/// Read the `kinds` (`selection`, `screen`, `last_command`, `file`, `preview`) of a shared `pane` into an offer. The answer
/// carries the id of the offer, where it would go, and for each item its number, kind, label, size and what the cleaner
/// removed: labels, sizes and findings, never text. The chat holds one offer at a time and it lives five minutes. A
/// refusal is the fault object with the code `context` and one of its stable reasons.
pub fn ai_context_preview(
    core: &AppCore,
    conversation: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
    kinds: Vec<ContextKind>,
) -> Result<OfferView, AiFault> {
    core.ai_context_preview(&conversation, pane, profile, model, kinds)
}

/// A fresh chat id. No file is made until the first message is sent.
pub fn ai_conversation_new(core: &AppCore) -> Result<String, AiFault> {
    core.ai().conversation_new()
}

/// The saved chats, newest first, and one note for each file that could not be listed.
pub fn ai_conversation_list(core: &AppCore) -> Result<ConversationList, AiFault> {
    core.ai().conversation_list()
}

/// Delete a chat and stop its turn. Answers whether a file was removed.
pub fn ai_conversation_delete(core: &AppCore, conversation: String) -> Result<bool, AiFault> {
    core.ai().conversation_delete(&conversation)
}

/// Stop the turn of a chat; it keeps the text it had. Answers whether the chat had a turn.
pub fn ai_cancel(core: &AppCore, conversation: String) -> Result<bool, AiFault> {
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
pub fn ai_decide(core: &AppCore, conversation: String, call_id: String, decision: String, hash: String) -> Result<(), AiFault> {
    core.ai_decide(&conversation, &call_id, &decision, &hash)
}

/// The person's press on `Undo` for one finished write: the chat and the call. It answers nothing. The core puts the old
/// text of the file back only while the file still holds exactly what the assistant wrote, and outside the deny list; a
/// refusal is the fault object with the code `undo` and a stable reason (`unknown_change`, `created`, `file_changed`,
/// `denied`, `unreadable`) and a fixed sentence.
///
/// This command exists for the webview only and is not a verdict on a tool call: it never reaches the approval table, and
/// it carries two ids and no text.
pub fn ai_undo(core: &AppCore, conversation: String, call_id: String) -> Result<(), AiFault> {
    core.ai_undo(&conversation, &call_id)
}

/// The requests of `wsh ai` that wait for an answer in the calling window, oldest first: a page that loaded after the
/// request was made (a reload, a window that opened late) asks for the cards it missed. It reads an in-memory table and
/// touches no keychain, file or network, and it holds no text but the prompt each card shows.
pub fn ai_ask_pending(core: &AppCore, window: &str) -> Result<Vec<AskView>, AiFault> {
    Ok(core.ai_ask_pending(window))
}

/// The person's press on a card of `wsh ai`: the request, `send` or `refuse`, and the seal the card carried. It answers
/// nothing; the answer of the model streams back to the program that asked. A refusal is the fault object with the code
/// `ask` and a stable reason (`unknown_ask`, `seal_mismatch`, `bad_verdict`) and a fixed sentence.
///
/// This command exists for the webview only: it releases one waiting request, for its own seal, and the local socket of
/// `wsh` has no way to reach it.
pub fn ai_ask_answer(core: &AppCore, ask: String, verdict: String, seal: String) -> Result<(), AiFault> {
    core.ai_ask_answer(&ask, &verdict, &seal)
}

/// Make a profile or change one. Answers its id. The input has no member for a key.
pub fn ai_profile_save(core: &AppCore, input: ProfileInputMsg) -> Result<String, AiFault> {
    core.ai().profile_save(input)
}

/// Remove a profile and its keys, and stop the turns that use it.
pub fn ai_profile_remove(core: &AppCore, profile: String) -> Result<RemovalView, AiFault> {
    core.ai().profile_remove(&profile)
}

/// Remove a key from the secret store and stop the turns that use the profile. Answers nothing.
pub fn ai_key_clear(core: &AppCore, profile: String, slot: KeySlot) -> Result<(), AiFault> {
    core.ai().key_clear(&profile, slot)
}

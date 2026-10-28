//! The plain functions of the commands of the assistant (the AI chat). A refusal of these commands is the object
//! `{code, reason, text}` ([`AiFault`]) and not a string; the events of a turn go to every window as the event `ai`, and
//! the page routes them by conversation. Keys live in the store the host gives the core: a key goes in with `ai_key_set`
//! and never comes out, and a store that is locked says `secrets are locked`.

use holzwave_core::ai::profiles::KeySlot;
use holzwave_core::ai::service::events::ChatState;

use crate::ai_host::{AiFault, AiProfiles};
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

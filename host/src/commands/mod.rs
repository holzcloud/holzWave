//! The commands of the app, written once.
//!
//! [`table`] is the one list of every command the page can call. The desktop shell expands it into the window system's
//! command wrappers and the server expands it into [`registry`], so a command cannot exist for one way only. The plain
//! functions that do the work are in one module for each theme ([`terminal`], [`layout`], [`settings`], [`connections`],
//! [`blocks`], [`files`], [`history_sync`], [`ai`], [`integrations`], [`vault`]); the wire types of their arguments and answers are
//! in [`wire`] or in the module that owns them. Nothing here knows a window shell: a plain function takes the core by
//! reference, the label of the calling window where the command works on a layout, and the arguments the page sends.

pub mod ai;
pub mod blocks;
pub mod connections;
pub mod files;
pub mod history_sync;
pub mod integrations;
pub mod layout;
pub mod registry;
pub mod settings;
pub mod table;
pub mod terminal;
pub mod vault;
pub mod wire;

/// Every name a row of the table writes: the plain function modules, the caps and the types of the arguments and
/// answers. Both expansions of the table (the registry here and the desktop wrappers in the app) import this one list,
/// so a row spells a type the same way on both sides and the recorded type text of a row is the text of the row.
pub mod prelude {
    pub use super::ai;
    pub use super::blocks;
    pub use super::connections;
    pub use super::files;
    pub use super::history_sync;
    pub use super::integrations;
    pub use super::layout;
    pub use super::registry::{CAP_DEFAULT, CAP_INPUT, CAP_LARGE, CAP_PASTE};
    pub use super::settings;
    pub use super::terminal;
    pub use super::vault;
    pub use super::vault::VaultStatusMsg;
    pub use super::wire::{
        CopyOpMsg, CopyStateMsg, EditActionMsg, GrantAnswer, GrantWhy, MouseActionMsg, MouseButtonMsg, PasteOutcomeMsg,
        SearchOpMsg, SelectOpMsg,
    };
    pub use crate::ai_host::{AiFault, AiProfiles, ConversationList, ProfileInputMsg, RemovalView, TicketView};
    pub use crate::integrate_host::{ConfirmAnswer, ContextAnswer, IntegrationFault, LaunchAnswer};
    pub use crate::state::{AppInfo, Released, SettingsView, View};
    pub use crate::windows::DropKind;
    pub use holzwave_core::ai::profiles::KeySlot;
    pub use holzwave_core::ai::service::events::ChatState;
    pub use holzwave_core::ai::service::{AskView, ContextState, OfferView};
    pub use holzwave_core::ai::types::ContextKind;
    pub use holzwave_core::blocks::{BlockKind, BlockView, Opened, Saved};
    pub use holzwave_core::connections::{ConnView, ProfileView, SecretChange};
    pub use holzwave_core::files::{Entry, Listing};
    pub use holzwave_core::integrate::service::{ProfilesView, RunAnswer, SelectionView, ToolView};
    pub use holzwave_core::integrate::ticket::Card;
    pub use holzwave_core::integrate::Selection;
    pub use holzwave_core::layout::{Edge, PaneId, SplitId};
    pub use holzwave_core::profiles::Profile;
    pub use holzwave_core::wire::SearchMsg;
    pub use serde_json::Value;
    pub use std::collections::BTreeMap;
}

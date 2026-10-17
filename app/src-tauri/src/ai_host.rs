//! The shell's side of the AI service: the sink that turns service events into one webview event, the types of the
//! answers the commands give, the faults, and the calls the commands make.
//!
//! The service itself lives in the core and owns every thread: a send returns at once, a turn streams on a thread of
//! its own, and its events reach the page through [`ShellAiSink`] as the event `ai`, for every window (the page routes
//! by conversation, so a reload or a window that was torn off keeps working). This file holds no tool host, no context
//! source and no decision of any kind; a key is written to the service and never read back.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use holzwave_core::ai::profiles::{KeyError, KeySlot, KeyStatus, KeyVault, Preset, ProfileInput, ProfileView, Removal};
use holzwave_core::ai::gate::OfferId;
use holzwave_core::ai::provider::Provider;
use holzwave_core::ai::redact::SourceText;
use holzwave_core::ai::service::events::{AiEvent, ChatState};
use holzwave_core::ai::service::{
    AiService, AiSink, AskFault, Asks, ContextRefusal, ContextSource, ContextState, Fetched, OfferView, ReadRefusal, SendRequest,
    ServiceError, TurnTicket,
};
use holzwave_core::ai::store::{ConversationId, ConversationSummary, JsonDirStore, StoreError};
use holzwave_core::ai::tools::{Access, DenyPolicy, FsKind, Style, ToolError};
use holzwave_core::ai::types::{ApiKey, ContextKind};
use holzwave_core::ai::wire::Wire;
use holzwave_core::blocks::{Block, BlockKind, LOCAL};
use holzwave_core::Error as CoreError;
use holzwave_core::files::{FileSource, Kind as FileKind};
use holzwave_core::layout::PaneId;
use holzwave_core::preview::{Content, SNIFF, classify};
use holzwave_core::secrets::KeychainStore;
use holzwave_core::settings::Settings;
use holzwave_core::textread::TextRegion;
use serde::{Deserialize, Serialize, Serializer};

use crate::ai_ask::ShellAskNotify;
use crate::ai_tools::{CoreTargets, ShellTools};
use crate::ai_write::UndoFault;
use crate::state::{AppCore, Out, Sink, Target};

/// The folder of the data directory that holds one file per chat.
pub const CHATS_DIR: &str = "chats";

/// How long the end of the app waits for running turns to stop. A turn that is cancelled ends within a quarter of a
/// second and keeps the text it had; the bound is for one that is stuck.
const SHUTDOWN_BOUND: Duration = Duration::from_secs(1);

/// What the host is built from besides the data directory: the keychain the profile keys live in, and a clock for the
/// tests. Nothing is read from either until a call needs it.
pub struct AiParts {
    /// The keychain. A vault without a store refuses every key and every profile that needs one.
    pub vault: KeyVault,
    /// A clock for the saved chats (Unix seconds); `None` is the system clock.
    pub clock: Option<fn() -> i64>,
    /// Places besides the data directory that no tool may touch: the configuration directory and the folder of the
    /// local socket. Empty unless [`AiParts::protecting`] names some.
    pub protected: Vec<PathBuf>,
}

impl AiParts {
    /// The platform keychain and the system clock.
    pub fn system() -> AiParts {
        AiParts { vault: KeyVault::new(Arc::new(KeychainStore)), clock: None, protected: Vec::new() }
    }

    /// The same parts with `dirs` added to the places no tool may touch. The deny list of the service, of the tools
    /// and of the source of shared content are all built from the one list.
    #[must_use]
    pub fn protecting(mut self, dirs: Vec<PathBuf>) -> AiParts {
        self.protected.extend(dirs);
        self
    }
}

// ---- the event ------------------------------------------------------------------------------------------------

/// One event of a chat turn, as the page receives it. It serialises as the event alone (`conversation`, `turn`, `seq`,
/// `kind`, then the fields of the kind); the name `ai` travels separately. `Debug` is the event's own, which prints
/// kinds and sizes and never the text.
#[derive(Clone)]
pub struct AiPayload(Arc<AiEvent>);

impl AiPayload {
    /// Wraps an event of the service.
    pub fn new(event: AiEvent) -> AiPayload {
        AiPayload(Arc::new(event))
    }
}

impl fmt::Debug for AiPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

/// Equal when it is the same event or when both serialise to the same value (an event that cannot be serialised is
/// equal to nothing but itself).
impl PartialEq for AiPayload {
    fn eq(&self, other: &AiPayload) -> bool {
        if Arc::ptr_eq(&self.0, &other.0) {
            return true;
        }
        match (serde_json::to_value(&*self.0), serde_json::to_value(&*other.0)) {
            (Ok(mine), Ok(theirs)) => mine == theirs,
            _ => false,
        }
    }
}

impl Serialize for AiPayload {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// Hands every event of the service to the sink of the shell, for every window. It takes no lock, waits for nothing
/// and never calls into [`AppCore`]: the service calls it from its turn threads.
struct ShellAiSink {
    sink: Arc<dyn Sink>,
}

impl AiSink for ShellAiSink {
    fn emit(&self, event: &AiEvent) {
        self.sink.send(Target::All, Out::Ai(AiPayload::new(event.clone())));
    }
}

// ---- refusals and answers -------------------------------------------------------------------------------------

/// Why a command was refused, as the page receives it: `{code, reason, text}`. `code` is the stable name of the
/// service error or one of the two the shell adds; `reason` is the lower-case name a dialog can act on, or `null`;
/// `text` is a fixed English phrase. None of the three holds typed text, a path, an address or a library message.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AiFault {
    pub code: &'static str,
    pub reason: Option<&'static str>,
    pub text: String,
}

impl AiFault {
    /// The conversation id is not one the store accepts.
    pub fn invalid_conversation() -> AiFault {
        AiFault { code: "invalid_conversation", reason: None, text: "that chat id is not valid".to_owned() }
    }

    /// The pane the chat acts for is not open.
    pub fn no_such_pane() -> AiFault {
        AiFault { code: "no_such_pane", reason: None, text: "that pane is not open".to_owned() }
    }

    /// An offer without picked items, or picked items without an offer.
    pub fn bad_pick() -> AiFault {
        context_fault(ContextRefusal::BadPick)
    }

    /// The verdict of a card is neither of the two the page may send.
    pub fn invalid_decision() -> AiFault {
        let text = "the decision must be allow or deny".to_owned();
        AiFault { code: "tool", reason: Some("invalid_decision"), text }
    }

    /// The table's refusal of a verdict or a call: the stable code of the error and its fixed sentence.
    pub fn from_tool(error: &ToolError) -> AiFault {
        AiFault { code: "tool", reason: Some(error.code()), text: error.to_string() }
    }

    /// The refusal of an answer to a card of `wsh ai`: the code `ask`, the stable name of the reason and its fixed
    /// sentence. The page acts on `unknown_ask`, `seal_mismatch` and `bad_verdict`.
    pub(crate) fn from_ask(fault: &AskFault) -> AiFault {
        AiFault { code: "ask", reason: Some(fault.reason()), text: fault.to_string() }
    }

    /// A change that could not be taken back: the code `undo`, the stable name of the reason and its fixed sentence.
    pub(crate) fn from_undo(fault: &UndoFault) -> AiFault {
        AiFault { code: "undo", reason: Some(fault.reason()), text: fault.to_string() }
    }

    /// A profile id that no profile has.
    pub fn unknown_profile() -> AiFault {
        AiFault { code: "profile", reason: Some("unknown_profile"), text: "there is no such profile".to_owned() }
    }

    /// The service's refusal, with the name of its reason. The matches have no wildcard arm, so a variant that the
    /// core adds later does not compile here until it has a name.
    pub fn from_service(error: &ServiceError) -> AiFault {
        let reason = match error {
            ServiceError::Profile(profile) => Some(profile.reason()),
            ServiceError::Context(refusal) => Some(refusal.reason()),
            ServiceError::Key(key) => Some(match key {
                KeyError::NoSecureStore => "no_secure_store",
                KeyError::Store => "store",
                KeyError::Invalid(_) => "invalid",
                KeyError::UnknownProfile => "unknown_profile",
                KeyError::NoSuchSlot => "no_such_slot",
            }),
            ServiceError::Store(store) => Some(match store {
                StoreError::InvalidId => "invalid_id",
                StoreError::Invalid(_) => "invalid",
                StoreError::TooLarge { .. } => "too_large",
                StoreError::Full { .. } => "full",
                StoreError::Damaged { .. } => "damaged",
                StoreError::Newer { .. } => "newer",
                StoreError::Io(_) => "io",
            }),
            ServiceError::EmptyText
            | ServiceError::TextTooLong
            | ServiceError::Busy
            | ServiceError::TooManyTurns
            | ServiceError::ShuttingDown
            | ServiceError::NoProfile
            | ServiceError::ContextTooMany
            | ServiceError::ContextDuplicate
            | ServiceError::Spawn
            | ServiceError::ConfigWrite
            | ServiceError::IdTaken => None,
        };
        AiFault { code: error.code(), reason, text: error.to_string() }
    }
}

/// What a send answers at once: which turn will carry the message. Its events carry the same numbers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TicketView {
    pub conversation: String,
    pub turn: u64,
}

impl From<TurnTicket> for TicketView {
    fn from(ticket: TurnTicket) -> TicketView {
        TicketView { conversation: ticket.conversation.as_str().to_owned(), turn: ticket.turn }
    }
}

/// The chats in the store, newest first, and one line for each file that could not be listed. The lines never quote a
/// chat.
#[derive(Debug, Serialize)]
pub struct ConversationList {
    pub chats: Vec<ConversationSummary>,
    pub notes: Vec<String>,
}

/// The profiles as the settings dialog shows them: a status for each key and never a key, and what loading `ai.json`
/// had to say.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AiProfiles {
    pub profiles: Vec<ProfileView>,
    pub notes: Vec<String>,
}

/// What removing a profile did: whether the profile is gone, and whether no key of it is left in the keychain. The
/// members are private so that no public member of the host carries a key's name; they are read on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RemovalView {
    removed: bool,
    key_deleted: bool,
}

impl From<Removal> for RemovalView {
    fn from(removal: Removal) -> RemovalView {
        RemovalView { removed: removal.removed, key_deleted: removal.key_deleted }
    }
}

/// What the settings dialog sends to make or change a profile. It has no member for a key: a key goes through
/// `ai_key_set` and is never read back, and an object with any other member is refused. Only `name` and `preset` are
/// required. `Debug` prints the preset and sizes, never the name or the address.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileInputMsg {
    /// The profile to change; absent or null makes a new one with an id made from the name.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub preset: Preset,
    /// The wire; absent or null keeps the wire of the profile, or the default of the preset for a new one.
    #[serde(default)]
    pub wire: Option<Wire>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub allow_lan_http: bool,
    #[serde(default)]
    pub acknowledge_gateway: bool,
}

impl ProfileInputMsg {
    /// The core's input. It starts from the core's own constructor and assigns the public members, so a member that
    /// the core adds later keeps its default.
    pub fn into_input(self) -> ProfileInput {
        let mut input = ProfileInput::new(self.name, self.preset);
        input.id = self.id;
        input.wire = self.wire;
        input.base_url = self.base_url;
        input.model = self.model;
        input.allow_lan_http = self.allow_lan_http;
        input.acknowledge_gateway = self.acknowledge_gateway;
        input
    }
}

impl fmt::Debug for ProfileInputMsg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileInputMsg")
            .field("preset", &self.preset)
            .field("has_id", &self.id.is_some())
            .field("name_bytes", &self.name.len())
            .field("base_url_bytes", &self.base_url.len())
            .field("model_bytes", &self.model.len())
            .finish_non_exhaustive()
    }
}

// ---- the host -------------------------------------------------------------------------------------------------

/// The AI service inside the shell. The commands call it through [`AppCore::ai`]; it adds the parsing of the ids and
/// the naming of the refusals, and decides nothing.
pub struct AiHost {
    service: AiService,
    /// The source of shared content, kept so the core can hand it its weak reference once the core exists.
    context: Arc<ShellContext>,
    /// The host of the agent's tools, kept for the same reason.
    tools: Arc<ShellTools>,
    /// The desk of the requests that shells make: the sink of the service, so it sees every event first.
    asks: Arc<Asks>,
    /// Tells the window of a pane that a request waits; bound to the core once it exists.
    notify: Arc<ShellAskNotify>,
}

impl AiHost {
    /// Builds the service over the chats folder and `ai.json` of `dir`. The data directory is the first place the
    /// agent may never touch, then the places `parts` names. Nothing else happens here: no thread, no socket, no
    /// keychain call.
    pub fn new(dir: &Path, sink: Arc<dyn Sink>, parts: AiParts) -> AiHost {
        let dirs = protected_dirs(dir, &parts.protected);
        let context = Arc::new(ShellContext::new(&dirs));
        let tools = Arc::new(ShellTools::new(DenyPolicy::new(dirs.clone())));
        // The desk wraps the sink of the page: events of a request from a shell stay with the desk, the others go on.
        let notify = Arc::new(ShellAskNotify::new(sink.clone()));
        let asks = Asks::new(Arc::new(ShellAiSink { sink }), notify.clone());
        let mut builder =
            AiService::builder(Arc::new(JsonDirStore::new(dir.join(CHATS_DIR))), asks.clone(), dir)
                .vault(parts.vault)
                .deny_policy(DenyPolicy::new(dirs))
                .tool_host(tools.clone())
                .context_source(context.clone());
        if let Some(clock) = parts.clock {
            builder = builder.clock(clock);
        }
        AiHost { service: builder.build(), context, tools, asks, notify }
    }

    /// Gives the source of shared content and the host of the tools the core they read panes and files through. Held
    /// weakly and set once; a call after the first changes nothing.
    pub fn bind(&self, core: Weak<AppCore>) {
        self.notify.bind(core.clone());
        self.context.bind(core.clone());
        self.tools.bind(Arc::new(CoreTargets::new(core)));
    }

    /// The service, for the calls the shell makes on it directly.
    pub fn service(&self) -> &AiService {
        &self.service
    }

    /// The desk of the requests that shells make.
    pub(crate) fn asks(&self) -> &Arc<Asks> {
        &self.asks
    }

    /// The host of the agent's tools, which keeps what a person may still take back.
    pub(crate) fn tools(&self) -> &ShellTools {
        &self.tools
    }

    /// Registers a provider that does not come from a profile (the on-device model).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn register_provider(&self, id: &str, provider: Arc<dyn Provider>) -> Result<(), AiFault> {
        self.service.register_provider(id, provider).map_err(|e| AiFault::from_service(&e))
    }

    /// Starts a turn for the typed `text` and returns at once. A blank `profile` or `model` is not given: the chat's
    /// own pin applies. Only the typed text leaves; the request has no context.
    pub fn send(
        &self,
        conversation: &str,
        text: String,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
    ) -> Result<TicketView, AiFault> {
        let request = request_of(conversation, text, pane, profile, model)?;
        self.service.send(request).map(TicketView::from).map_err(|e| AiFault::from_service(&e))
    }

    /// [`AiHost::send`] with an offer that was previewed for this chat, this pane and this route: the page names the
    /// offer and the numbers of the items it picked, never their text. The service spends the offer, and a refusal of
    /// it is the fault `context`.
    pub fn send_with_offer(
        &self,
        conversation: &str,
        text: String,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        offer: String,
        picked: Vec<u32>,
    ) -> Result<TicketView, AiFault> {
        let offer = OfferId::parse(&offer).ok_or_else(|| context_fault(ContextRefusal::StaleOffer))?;
        let request = request_of(conversation, text, pane, profile, model)?.offer(offer, picked);
        self.service.send(request).map(TicketView::from).map_err(|e| AiFault::from_service(&e))
    }

    /// What `pane` can offer for this chat and where it would go: no text. A blank `profile` or `model` is not given.
    /// This classifies the destination (a name lookup for a host name) and blocks, so call it from a worker thread.
    pub fn context_state(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
    ) -> Result<ContextState, AiFault> {
        let id = parse_conversation(conversation)?;
        let (profile, model) = (given(profile), given(model));
        self.service
            .context_state(&id, pane, profile.as_deref(), model.as_deref())
            .map_err(|e| AiFault::from_service(&e))
    }

    /// Shares `pane` for the chat on this route, or ends the share. Nothing is read.
    pub fn context_share(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        shared: bool,
    ) -> Result<ContextState, AiFault> {
        let id = parse_conversation(conversation)?;
        let (profile, model) = (given(profile), given(model));
        self.service
            .context_share(&id, pane, profile.as_deref(), model.as_deref(), shared)
            .map_err(|e| AiFault::from_service(&e))
    }

    /// Reads the asked kinds from the shared `pane` and offers them: labels, sizes and findings, never text.
    pub fn context_preview(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        kinds: Vec<ContextKind>,
    ) -> Result<OfferView, AiFault> {
        let id = parse_conversation(conversation)?;
        let (profile, model) = (given(profile), given(model));
        self.service
            .context_preview(&id, pane, profile.as_deref(), model.as_deref(), &kinds)
            .map_err(|e| AiFault::from_service(&e))
    }

    /// Tells the service that the pane of a chat closed: the shares of the chat end. An id that is not a chat id is
    /// ignored.
    pub fn chat_closed(&self, conversation: &str) {
        if let Ok(id) = ConversationId::parse(conversation) {
            self.service.chat_closed(&id);
        }
    }

    /// The state of a chat at this moment: what a page that saw every event holds.
    pub fn snapshot(&self, conversation: &str) -> Result<ChatState, AiFault> {
        let id = parse_conversation(conversation)?;
        self.service.snapshot(&id).map_err(|e| AiFault::from_service(&e))
    }

    /// A fresh chat id. No file is made before the first message.
    pub fn conversation_new(&self) -> Result<String, AiFault> {
        self.service.new_conversation().map(|id| id.as_str().to_owned()).map_err(|e| AiFault::from_service(&e))
    }

    /// The chats in the store, newest first.
    pub fn conversation_list(&self) -> Result<ConversationList, AiFault> {
        let listing = self.service.list_conversations().map_err(|e| AiFault::from_service(&e))?;
        Ok(ConversationList { chats: listing.chats, notes: listing.notes })
    }

    /// Deletes a chat and stops its turn. Answers whether a file was removed.
    pub fn conversation_delete(&self, conversation: &str) -> Result<bool, AiFault> {
        let id = parse_conversation(conversation)?;
        let removed = self.service.delete_conversation(&id).map_err(|e| AiFault::from_service(&e))?;
        // What the chat could still take back goes with it.
        self.tools.forget_conversation(id.as_str());
        Ok(removed)
    }

    /// Stops the turn of a chat. Answers whether the chat had a turn.
    pub fn cancel(&self, conversation: &str) -> Result<bool, AiFault> {
        let id = parse_conversation(conversation)?;
        Ok(self.service.cancel(&id))
    }

    /// The profiles with the status of their keys. This reads the keychain, so call it when the dialog opens.
    pub fn profiles(&self) -> AiProfiles {
        AiProfiles { profiles: self.service.profiles(), notes: self.service.config_notes() }
    }

    /// Makes a profile or changes one. Answers its id.
    pub fn profile_save(&self, input: ProfileInputMsg) -> Result<String, AiFault> {
        self.service.save_profile(input.into_input()).map_err(|e| AiFault::from_service(&e))
    }

    /// Removes a profile and its keys, and stops the turns that use it.
    pub fn profile_remove(&self, profile: &str) -> Result<RemovalView, AiFault> {
        self.service.remove_profile(profile).map(RemovalView::from).map_err(|e| AiFault::from_service(&e))
    }

    /// Saves a key in the keychain. The string becomes the core's key type as the first act, so no copy of the text
    /// stays here, and nothing is answered but whether it worked: no call of the shell returns a key.
    pub fn key_set(&self, profile: &str, slot: KeySlot, key: String) -> Result<(), AiFault> {
        let key = ApiKey::new(key);
        self.service.set_key(profile, slot, key).map_err(|e| AiFault::from_service(&e))
    }

    /// Removes a key from the keychain and stops the turns that use the profile.
    pub fn key_clear(&self, profile: &str, slot: KeySlot) -> Result<(), AiFault> {
        self.service.clear_key(profile, slot).map_err(|e| AiFault::from_service(&e))
    }

    /// Whether a usable key is saved in `slot` of `profile`: the status of the slot is the present one. The status is
    /// read once for every profile, which reads the keychain.
    pub fn has_key(&self, profile: &str, slot: KeySlot) -> Result<bool, AiFault> {
        let views = self.service.profiles();
        let view = views.iter().find(|view| view.id == profile).ok_or_else(AiFault::unknown_profile)?;
        let status = match slot {
            KeySlot::Provider => Some(view.key),
            KeySlot::Gateway => view.gateway_key,
        };
        Ok(status == Some(KeyStatus::Present))
    }

    /// Tells the service the two settings it reads (`ai_local_only`, `ai_persist_context`). Call it at start and on
    /// every change of the settings; the effect (turns it stopped) is not needed by the shell.
    pub fn apply_settings(&self, settings: &Settings) {
        let _ = self.service.apply_settings(settings);
    }

    /// Tells the service that a pane left the layout: the calls that wait for a decision in turns that act for it are
    /// voided. Answers how many.
    pub fn pane_closed(&self, pane: PaneId) -> usize {
        self.service.pane_closed(pane)
    }

    /// Ends the running turns (they keep the text they had) and waits for them for at most [`SHUTDOWN_BOUND`]. A send
    /// after this is refused. Safe to call more than once.
    pub fn shutdown(&self) {
        // The requests that wait for a person end first: nobody can answer them any more.
        self.asks.shutdown();
        let left = self.service.shutdown(SHUTDOWN_BOUND);
        if left > 0 {
            eprintln!("holzwave: {left} assistant turn(s) did not end within the shutdown bound");
        }
    }
}

/// The places the agent may never touch: the data directory first, then each of `extra` once. The deny policy of the
/// service, the one of the tools and the one the source of shared content reads files through are built from this one
/// list, and it is the one place to add a directory.
fn protected_dirs(dir: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    for place in extra {
        if !dirs.contains(place) {
            dirs.push(place.clone());
        }
    }
    dirs
}

/// The refusal of a context call as the page receives it.
fn context_fault(refusal: ContextRefusal) -> AiFault {
    AiFault::from_service(&ServiceError::Context(refusal))
}

/// The request of a text send: the chat, the typed text, the pane, and the profile and model when they are given.
fn request_of(
    conversation: &str,
    text: String,
    pane: PaneId,
    profile: Option<String>,
    model: Option<String>,
) -> Result<SendRequest, AiFault> {
    let mut request = SendRequest::new(parse_conversation(conversation)?, text, pane);
    if let Some(profile) = given(profile) {
        request = request.profile(profile);
    }
    if let Some(model) = given(model) {
        request = request.model(model);
    }
    Ok(request)
}

/// The id of a chat, parsed at the boundary: nothing else is ever used as a file name.
fn parse_conversation(text: &str) -> Result<ConversationId, AiFault> {
    ConversationId::parse(text).map_err(|_| AiFault::invalid_conversation())
}

/// `value` unless it is absent or only white space.
fn given(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
}

// ---- what a pane or a file gives ------------------------------------------------------------------------------

/// The source of shared content: the text a terminal pane shows, and the file a preview pane shows.
///
/// A terminal region is read from the rendered grid on the engine thread of the pane, and the cells the engine drew
/// hidden travel as byte ranges, so the gate removes and reports them. A file is read as saved on disk, only after the
/// deny list allows its path. Nothing is cleaned here: the gate is the one cleaner. `Debug` prints nothing of the
/// content.
struct ShellContext {
    core: OnceLock<Weak<AppCore>>,
    deny: DenyPolicy,
}

impl fmt::Debug for ShellContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellContext").finish_non_exhaustive()
    }
}

/// A file as the source read it: what it is, its name and its first bytes.
struct FileRead {
    kind: ContextKind,
    name: String,
    bytes: Vec<u8>,
}

impl ShellContext {
    fn new(protected: &[PathBuf]) -> ShellContext {
        ShellContext { core: OnceLock::new(), deny: DenyPolicy::new(protected.to_vec()) }
    }

    fn bind(&self, core: Weak<AppCore>) {
        let _ = self.core.set(core);
    }

    fn core(&self) -> Option<Arc<AppCore>> {
        self.core.get().and_then(Weak::upgrade)
    }

    /// The path of the file of `block` when the deny list lets it be read: the resolved path for this computer, the
    /// path as spelled for a saved connection. Nothing is opened before this answers.
    fn readable_path(&self, block: &Block) -> Result<String, ReadRefusal> {
        if block.source == LOCAL {
            let local = Path::new(&block.path);
            let resolved = self.deny.check_local(local, Access::Read).map_err(|_| ReadRefusal::Denied)?;
            return resolved.to_str().map(str::to_owned).ok_or(ReadRefusal::Denied);
        }
        self.deny.check(Style::Posix, FsKind::Remote, &block.path, Access::Read).map_err(|_| ReadRefusal::Denied)?;
        Ok(block.path.clone())
    }

    /// Reads at most `limit` bytes of the file a preview pane shows and says what it is: Markdown is a `Preview`, other
    /// text a `File`, anything else is not text.
    fn read_file(&self, block: &Block, source: &dyn FileSource, limit: usize) -> Result<FileRead, ReadRefusal> {
        let path = self.readable_path(block)?;
        let entry = source.stat(&path).map_err(|_| ReadRefusal::Unreadable)?;
        if entry.kind != FileKind::File {
            return Err(ReadRefusal::NotText);
        }
        let bytes = source.read_range(&path, 0, limit as u64).map_err(|_| ReadRefusal::Unreadable)?;
        let kind = match classify(&entry.name, &bytes[..bytes.len().min(SNIFF)]).content {
            Content::Markdown => ContextKind::Preview,
            Content::Text => ContextKind::File,
            _ => return Err(ReadRefusal::NotText),
        };
        Ok(FileRead { kind, name: entry.name, bytes })
    }

    fn read_terminal(
        &self,
        core: &AppCore,
        pane: PaneId,
        kind: ContextKind,
        max_bytes: usize,
    ) -> Result<Option<Fetched>, ReadRefusal> {
        if core.context_block(pane).is_some() {
            return Err(ReadRefusal::Unsupported);
        }
        let Some(handle) = core.context_pane(pane) else {
            return Err(if core.pane_is_open(pane) { ReadRefusal::Unsupported } else { ReadRefusal::PaneGone });
        };
        let (region, what) = match kind {
            ContextKind::Selection => (TextRegion::Selection, "Selection"),
            ContextKind::Screen => (TextRegion::Screen, "Screen"),
            ContextKind::LastCommand => (TextRegion::LastOutput, "Last command output"),
            ContextKind::File | ContextKind::Preview => return Err(ReadRefusal::Unsupported),
        };
        let read = handle.read_text(region, max_bytes).map_err(|error| refusal_of(&error))?;
        Ok(read.map(|read| {
            let cut = read.truncated;
            Fetched::new(format!("{what} of pane {pane}"), SourceText::with_hidden(read.text, read.hidden), cut)
        }))
    }

    fn read_block(
        &self,
        core: &AppCore,
        pane: PaneId,
        kind: ContextKind,
        max_bytes: usize,
    ) -> Result<Option<Fetched>, ReadRefusal> {
        let (block, source) = core.context_block(pane).ok_or(ReadRefusal::Unsupported)?;
        if block.kind != BlockKind::Preview {
            return Err(ReadRefusal::Unsupported);
        }
        // One byte more than the bound tells whether the file was cut.
        let file = self.read_file(&block, source.as_ref(), max_bytes.saturating_add(1))?;
        if file.kind != kind {
            return Err(ReadRefusal::Unsupported);
        }
        let cut = file.bytes.len() > max_bytes;
        let mut text = String::from_utf8_lossy(&file.bytes).into_owned();
        if text.len() > max_bytes {
            let mut at = max_bytes;
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            text.truncate(at);
        }
        let label = if block.source == LOCAL { file.name } else { format!("{} on {}", file.name, source.label()) };
        Ok(Some(Fetched::new(label, SourceText::plain(text), cut)))
    }
}

/// The reason a pane could not be read, from the engine's own words: a closed pane, one that did not answer in time, or
/// anything else. The library's message is dropped.
fn refusal_of(error: &CoreError) -> ReadRefusal {
    match error {
        CoreError::Pty(message) if message == "the pane is closed" => ReadRefusal::PaneGone,
        CoreError::Pty(message) if message == "pane busy" => ReadRefusal::Busy,
        _ => ReadRefusal::Unreadable,
    }
}

impl ContextSource for ShellContext {
    fn kinds(&self, pane: PaneId) -> Vec<ContextKind> {
        let Some(core) = self.core() else { return Vec::new() };
        if let Some((block, source)) = core.context_block(pane) {
            if block.kind != BlockKind::Preview {
                return Vec::new();
            }
            return match self.read_file(&block, source.as_ref(), SNIFF) {
                Ok(file) => vec![file.kind],
                Err(_) => Vec::new(),
            };
        }
        if core.context_pane(pane).is_some() {
            return vec![ContextKind::Selection, ContextKind::Screen, ContextKind::LastCommand];
        }
        Vec::new()
    }

    fn read(&self, pane: PaneId, kind: ContextKind, max_bytes: usize) -> Result<Option<Fetched>, ReadRefusal> {
        let core = self.core().ok_or(ReadRefusal::PaneGone)?;
        match kind {
            ContextKind::Selection | ContextKind::Screen | ContextKind::LastCommand => {
                self.read_terminal(&core, pane, kind, max_bytes)
            }
            ContextKind::File | ContextKind::Preview => self.read_block(&core, pane, kind, max_bytes),
        }
    }
}

impl AppCore {
    /// [`AiHost::send`] for a chat that acts for `pane`. The page names the pane, and a pane that is not open is
    /// refused before anything is stored or started: an approval that waits for a pane that does not exist would wait
    /// for its timeout.
    pub fn ai_send(
        &self,
        conversation: &str,
        text: String,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
    ) -> Result<TicketView, AiFault> {
        self.open_pane(pane)?;
        self.ai().send(conversation, text, pane, profile, model)
    }

    /// The send of the command: the typed text alone when `offer` and `picked` are both left out, with the items of an
    /// offer when both are given, and the fault `context` / `bad_pick` when only one is, before anything is stored.
    pub fn ai_send_with(
        &self,
        conversation: &str,
        text: String,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        offer: Option<String>,
        picked: Option<Vec<u32>>,
    ) -> Result<TicketView, AiFault> {
        match (offer, picked) {
            (None, None) => self.ai_send(conversation, text, pane, profile, model),
            (Some(offer), Some(picked)) => {
                self.ai_send_with_context(conversation, text, pane, profile, model, offer, picked)
            }
            _ => Err(AiFault::bad_pick()),
        }
    }

    /// [`AppCore::ai_send`] with an offer: the page sends the offer id and the numbers of the picked items. The pane
    /// is the one whose content was read and the one the chat acts for.
    pub fn ai_send_with_context(
        &self,
        conversation: &str,
        text: String,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        offer: String,
        picked: Vec<u32>,
    ) -> Result<TicketView, AiFault> {
        self.open_pane(pane)?;
        self.ai().send_with_offer(conversation, text, pane, profile, model, offer, picked)
    }

    /// [`AiHost::context_state`] for a pane that is open.
    pub fn ai_context_state(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
    ) -> Result<ContextState, AiFault> {
        self.open_pane(pane)?;
        self.ai().context_state(conversation, pane, profile, model)
    }

    /// [`AiHost::context_share`] for a pane that is open.
    pub fn ai_context_share(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        shared: bool,
    ) -> Result<ContextState, AiFault> {
        self.open_pane(pane)?;
        self.ai().context_share(conversation, pane, profile, model, shared)
    }

    /// [`AiHost::context_preview`] for a pane that is open.
    pub fn ai_context_preview(
        &self,
        conversation: &str,
        pane: PaneId,
        profile: Option<String>,
        model: Option<String>,
        kinds: Vec<ContextKind>,
    ) -> Result<OfferView, AiFault> {
        self.open_pane(pane)?;
        self.ai().context_preview(conversation, pane, profile, model, kinds)
    }

    /// A pane that is not open is refused before anything is stored, read or looked up.
    fn open_pane(&self, pane: PaneId) -> Result<(), AiFault> {
        if self.pane_is_open(pane) { Ok(()) } else { Err(AiFault::no_such_pane()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use holzwave_core::ai::provider::AiError;
    use holzwave_core::ai::scripted::{Script, ScriptedProvider};
    use holzwave_core::ai::service::events::ApplyResult;
    use holzwave_core::ai::types::{Capabilities, Locality, Part};
    use holzwave_core::layout::MAIN_WINDOW;
    use holzwave_core::secrets::MemoryStore;
    use holzwave_core::settings::SETTINGS_FILE;
    use serde_json::{Value, json};

    use crate::state::ShellSpec;
    use crate::windows::{CursorSpot, Place};

    /// The text a user types in these tests; it appears in test code only, so a hit anywhere else is a leak.
    const TEXT_CANARY: &str = "zq-host-text-8d3b27f1c4";

    /// The key a user saves in these tests; it appears in test code only, so a hit anywhere else is a leak.
    const KEY_CANARY: &str = "zq-host-key-5a91c3e7d0";

    /// A scratch data directory, removed at the end of the test. Declared before the core, so the core is dropped
    /// first and a late write cannot bring the directory back.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-ai-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Scratch(path)
        }

        fn chats(&self) -> PathBuf {
            self.0.join(CHATS_DIR)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Keeps every event the shell sends, with its target, in order.
    #[derive(Default)]
    struct AiRecorder {
        log: Mutex<Vec<(Target, Out)>>,
    }

    impl Sink for AiRecorder {
        fn send(&self, target: Target, out: Out) {
            self.log.lock().expect("log").push((target, out));
        }

        fn close_window(&self, _label: &str) {}

        fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
            Ok(())
        }

        fn cursor_spot(&self, _source: &str) -> CursorSpot {
            CursorSpot::Unknown
        }
    }

    impl AiRecorder {
        /// Every event, whatever it was.
        fn all(&self) -> Vec<(Target, Out)> {
            self.log.lock().expect("log").clone()
        }

        /// The `ai` events with their targets.
        fn ai_events(&self) -> Vec<(Target, AiPayload)> {
            self.all()
                .into_iter()
                .filter_map(|(target, out)| match out {
                    Out::Ai(payload) => Some((target, payload)),
                    _ => None,
                })
                .collect()
        }

        /// Whether a turn of `chat` has ended.
        fn ended(&self, chat: &str) -> bool {
            self.saw(chat, "turn_ended")
        }

        /// How many turns of `chat` have ended.
        fn ended_turns(&self, chat: &str) -> usize {
            self.ai_events().iter().filter(|(_, p)| p.0.conversation == chat && p.0.kind.name() == "turn_ended").count()
        }

        /// Whether an event of this kind was sent for `chat`.
        fn saw(&self, chat: &str, kind: &str) -> bool {
            self.ai_events().iter().any(|(_, p)| p.0.conversation == chat && p.0.kind.name() == kind)
        }

        /// The last `turn_ended` event of `chat`, as the page receives it.
        fn end_of(&self, chat: &str) -> Option<Value> {
            self.ai_events()
                .iter()
                .rev()
                .find(|(_, p)| p.0.conversation == chat && p.0.kind.name() == "turn_ended")
                .map(|(_, p)| json_of(&*p.0))
        }
    }

    fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if f() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn fixed_clock() -> i64 {
        1_800_000_000
    }

    /// A core over `dir` with the recording sink, no pane started, and the fixed clock.
    fn core_with(dir: &Path, vault: KeyVault) -> (Arc<AppCore>, Arc<AiRecorder>) {
        let rec = Arc::new(AiRecorder::default());
        let core = AppCore::new_with_ai(
            dir.to_path_buf(),
            rec.clone(),
            ShellSpec::new("/bin/cat", &[]),
            AiParts { vault, clock: Some(fixed_clock), protected: Vec::new() },
        );
        (core, rec)
    }

    fn scripted(id: &str, locality: Locality) -> ScriptedProvider {
        ScriptedProvider::new(id).with_capabilities(Capabilities::conservative(locality))
    }

    fn json_of<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("serialises")
    }

    /// The pane that has the focus in the main window.
    fn pane_of(core: &AppCore) -> PaneId {
        core.view_in(MAIN_WINDOW).expect("main").focus
    }

    /// Registers `provider` under `id` and keeps a handle to read what it was asked.
    fn register(core: &AppCore, id: &str, provider: ScriptedProvider) -> Arc<ScriptedProvider> {
        let provider = Arc::new(provider);
        core.ai().register_provider(id, provider.clone()).expect("registered");
        provider
    }

    /// A turn that streams one piece of text and then goes quiet for thirty seconds, or until it is cancelled.
    fn stalling() -> Script {
        Script::new().text("partial").stall(Duration::from_secs(30))
    }

    /// Every way a fault can be printed or sent, as one text.
    fn printed(fault: &AiFault) -> String {
        format!("{} {fault:?} {fault:#?}", json_of(fault))
    }

    fn profile_msg(name: &str, preset: Preset) -> ProfileInputMsg {
        ProfileInputMsg {
            id: None,
            name: name.to_owned(),
            preset,
            wire: None,
            base_url: String::new(),
            model: String::new(),
            allow_lan_http: false,
            acknowledge_gateway: false,
        }
    }

    // ---- the tracer -------------------------------------------------------------------------------------------

    #[test]
    fn a_message_sent_through_the_app_core_reaches_the_webview_as_ai_events_and_a_snapshot_matches() {
        let scratch = Scratch::new("tracer");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = Arc::new(
            scripted("scripted", Locality::OnDevice)
                .with_turn(Script::new().reasoning("thinking").text("Hello there").usage(11, 5)),
        );
        core.ai().register_provider("scripted", provider.clone()).expect("registered");
        let chat = ConversationId::generate().expect("an id").as_str().to_owned();
        let pane = core.view_in(MAIN_WINDOW).expect("main").focus;

        let started = Instant::now();
        let ticket =
            core.ai_send(&chat, format!("hello {TEXT_CANARY}"), pane, Some("scripted".to_owned()), None).expect("sent");
        assert!(started.elapsed() < Duration::from_secs(1), "a send returns at once");
        assert_eq!(ticket.conversation, chat);
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");

        let events = rec.ai_events();
        assert!(!events.is_empty());
        for (target, payload) in &events {
            assert_eq!(*target, Target::All, "every ai event is for every window");
            assert_eq!(payload.0.conversation, chat);
            assert_eq!(payload.0.turn, ticket.turn);
        }
        let seqs: Vec<u64> = events.iter().map(|(_, p)| p.0.seq).collect();
        assert_eq!(seqs, (1..=events.len() as u64).collect::<Vec<_>>(), "seq runs from 1 without a hole");
        let kinds: Vec<&str> = events.iter().map(|(_, p)| p.0.kind.name()).collect();
        assert_eq!(kinds, ["turn_started", "reasoning", "delta", "usage", "message", "turn_ended"]);

        // The page reducer: every event applies, and the state equals the snapshot the command answers.
        let mut folded = ChatState::new(&chat);
        for (_, payload) in &events {
            assert_eq!(folded.apply(&payload.0), ApplyResult::Applied);
        }
        let snapshot = core.ai().snapshot(&chat).expect("a snapshot");
        assert_eq!(json_of(&folded), json_of(&snapshot), "the fold of the events is the snapshot");
        assert!(!snapshot.running);
        assert_eq!(snapshot.messages.len(), 2);
        assert!(snapshot.partial.text.is_empty() && snapshot.partial.reasoning.is_empty());

        // Only the typed text left, and no context.
        let requests = provider.requests();
        assert_eq!(requests.len(), 1, "one send is one request");
        assert!(requests[0].context.is_empty(), "no context travels with a typed message");
        let typed = requests[0].messages.iter().flat_map(|m| &m.parts).any(|part| match part {
            Part::Text { text } => text.contains(TEXT_CANARY),
            _ => false,
        });
        assert!(typed, "the typed text is in the request");
        assert!(scratch.chats().join(format!("{chat}.json")).is_file(), "the chat was saved");

        // The first event on the wire: the payload alone, in the documented key order.
        let first = serde_json::to_string(&rec.all().into_iter().next().expect("an event").1).expect("serialises");
        let head =
            format!("{{\"conversation\":\"{chat}\",\"turn\":{},\"seq\":1,\"kind\":\"turn_started\"", ticket.turn);
        assert!(first.starts_with(&head), "{first}");
    }

    #[test]
    fn an_ai_event_is_the_payload_alone_under_the_name_ai_and_never_prints_its_text() {
        use holzwave_core::ai::service::events::AiEventKind;
        let event = |seq: u64| AiEvent {
            conversation: "c".to_owned(),
            turn: 1,
            seq,
            kind: AiEventKind::Delta { text: TEXT_CANARY.to_owned() },
        };
        let out = Out::Ai(AiPayload::new(event(2)));

        assert_eq!(out.name(), "ai");
        assert_eq!(
            serde_json::to_string(&out).expect("serialises"),
            format!("{{\"conversation\":\"c\",\"turn\":1,\"seq\":2,\"kind\":\"delta\",\"text\":\"{TEXT_CANARY}\"}}")
        );
        for printed in [format!("{out:?}"), format!("{out:#?}")] {
            assert!(!printed.contains(TEXT_CANARY), "a Debug line shows the text: {printed}");
        }
        assert_eq!(AiPayload::new(event(2)), AiPayload::new(event(2)), "equal events are equal payloads");
        assert_ne!(AiPayload::new(event(2)), AiPayload::new(event(3)), "payloads that differ in seq are not equal");
    }

    #[test]
    fn a_send_for_a_pane_that_is_not_open_is_refused_before_anything_is_stored() {
        let scratch = Scratch::new("no-pane");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = Arc::new(scripted("scripted", Locality::OnDevice).with_turn(Script::reply("never sent")));
        core.ai().register_provider("scripted", provider.clone()).expect("registered");
        let chat = ConversationId::generate().expect("an id").as_str().to_owned();

        let fault = core
            .ai_send(&chat, format!("hello {TEXT_CANARY}"), 999_999_999, Some("scripted".to_owned()), None)
            .expect_err("refused");

        assert_eq!(fault.code, "no_such_pane");
        assert_eq!(fault.reason, None);
        assert_eq!(fault.text, "that pane is not open");
        assert!(rec.ai_events().is_empty(), "no event");
        assert_eq!(provider.calls(), 0, "the provider was not called");
        assert!(!scratch.chats().exists(), "nothing was stored");
    }

    // ---- conversations ----------------------------------------------------------------------------------------

    #[test]
    fn conversations_are_created_listed_and_deleted_and_a_bad_id_is_refused() {
        let scratch = Scratch::new("chats");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider =
            register(&core, "scripted", scripted("scripted", Locality::OnDevice).with_turn(Script::reply("ok")));
        let pane = pane_of(&core);
        let ai = core.ai();

        // An id that is not a chat id never reaches the store, whatever the call.
        let long = "a".repeat(65);
        for bad in ["", "../etc/passwd", "a/b", long.as_str()] {
            let sent = core.ai_send(bad, "hello".to_owned(), pane, Some("scripted".to_owned()), None);
            for fault in [ai.conversation_delete(bad).err(), ai.cancel(bad).err(), ai.snapshot(bad).err(), sent.err()] {
                let fault = fault.expect("refused");
                assert_eq!((fault.code, fault.reason), ("invalid_conversation", None), "{bad:?}");
                assert_eq!(fault.text, "that chat id is not valid");
            }
        }
        assert!(!scratch.chats().exists(), "a refused id touched the disk");
        assert_eq!(provider.calls(), 0);
        assert!(rec.ai_events().is_empty());

        // A new id is a valid id, differs from the last one, and makes no file.
        let first = ai.conversation_new().expect("an id");
        let second = ai.conversation_new().expect("another id");
        assert_ne!(first, second);
        assert!(ConversationId::parse(&first).is_ok() && ConversationId::parse(&second).is_ok());
        assert!(!scratch.chats().exists(), "a new id makes no file");

        // A chat that was never saved is an empty state, and has no turn to cancel.
        let empty = ai.snapshot(&first).expect("an empty state");
        assert_eq!((empty.turn, empty.running, empty.messages.len()), (0, false, 0));
        assert_eq!(ai.cancel(&first), Ok(false));

        // After one send the list holds exactly that chat.
        core.ai_send(&first, "hello".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&first)));
        let list = ai.conversation_list().expect("a list");
        assert_eq!(list.chats.len(), 1);
        assert_eq!(list.chats[0].id.as_str(), first);
        assert!(!list.chats[0].title.is_empty());
        assert!(list.notes.is_empty());

        // Delete removes the file once.
        let file = scratch.chats().join(format!("{first}.json"));
        assert!(file.is_file());
        assert_eq!(ai.conversation_delete(&first), Ok(true));
        assert!(ai.conversation_list().expect("a list").chats.is_empty());
        assert!(!file.exists());
        assert_eq!(ai.conversation_delete(&first), Ok(false));
    }

    // ---- profiles and keys ------------------------------------------------------------------------------------

    /// `ai.json` of the scratch directory, when there is one, is JSON and holds no key.
    fn assert_ai_json_clean(scratch: &Scratch) {
        if let Ok(text) = std::fs::read_to_string(scratch.0.join("ai.json")) {
            serde_json::from_str::<Value>(&text).expect("ai.json is JSON");
            assert!(!text.contains(KEY_CANARY), "ai.json holds the key");
        }
    }

    #[test]
    fn profiles_and_keys_go_through_the_calls_and_no_answer_holds_a_key() {
        let scratch = Scratch::new("keys");
        let (core, rec) = core_with(&scratch.0, KeyVault::new(Arc::new(MemoryStore::default())));
        let ai = core.ai();
        let mut faults: Vec<AiFault> = Vec::new();
        let mut answers: Vec<String> = Vec::new();

        let id = ai.profile_save(profile_msg("Claude", Preset::Anthropic)).expect("saved");
        let listed = ai.profiles();
        answers.push(json_of(&listed).to_string());
        assert_eq!(listed.profiles.len(), 1);
        assert_eq!(listed.profiles[0].id, id);
        assert_eq!(listed.profiles[0].key, KeyStatus::NotSet);
        assert!(listed.notes.is_empty());
        assert_eq!(ai.has_key(&id, KeySlot::Provider), Ok(false));
        assert_ai_json_clean(&scratch);

        // A key that is too long is refused and nothing is stored.
        let fault = ai.key_set(&id, KeySlot::Provider, KEY_CANARY.repeat(200)).expect_err("too long");
        assert_eq!(fault.code, "key");
        faults.push(fault);
        assert_eq!(ai.has_key(&id, KeySlot::Provider), Ok(false));

        // A key is written and the calls only say that it is there.
        ai.key_set(&id, KeySlot::Provider, KEY_CANARY.to_owned()).expect("stored");
        assert_eq!(ai.has_key(&id, KeySlot::Provider), Ok(true));
        let listed = ai.profiles();
        answers.push(json_of(&listed).to_string());
        assert_eq!(listed.profiles[0].key, KeyStatus::Present);
        assert_ai_json_clean(&scratch);

        // A slot the kind of provider does not have, and a profile that does not exist.
        let fault = ai.key_set(&id, KeySlot::Gateway, KEY_CANARY.to_owned()).expect_err("no such slot");
        assert_eq!((fault.code, fault.reason), ("key", Some("no_such_slot")));
        faults.push(fault);
        let fault = ai.key_set("nobody", KeySlot::Provider, KEY_CANARY.to_owned()).expect_err("unknown");
        assert_eq!((fault.code, fault.reason), ("key", Some("unknown_profile")));
        faults.push(fault);
        let fault = ai.has_key("nobody", KeySlot::Provider).expect_err("unknown");
        assert_eq!((fault.code, fault.reason), ("profile", Some("unknown_profile")));
        faults.push(fault);

        // Clearing makes the key go; removing the profile removes it.
        ai.key_clear(&id, KeySlot::Provider).expect("cleared");
        assert_eq!(ai.has_key(&id, KeySlot::Provider), Ok(false));
        ai.key_set(&id, KeySlot::Provider, KEY_CANARY.to_owned()).expect("stored again");
        let removal = ai.profile_remove(&id).expect("removed");
        assert_eq!(removal, RemovalView { removed: true, key_deleted: true });
        assert!(ai.profiles().profiles.is_empty());
        assert_ai_json_clean(&scratch);

        // A refused profile is a reason the dialog can act on.
        let mut bad = profile_msg("x", Preset::Anthropic);
        bad.name = String::new();
        let fault = ai.profile_save(bad).expect_err("no name");
        assert_eq!((fault.code, fault.reason), ("profile", Some("name")));
        faults.push(fault);

        // Without a keychain a key is refused by name, and nothing else changes.
        let other = Scratch::new("keys-none");
        let (bare, _) = core_with(&other.0, KeyVault::unavailable());
        let id = bare.ai().profile_save(profile_msg("Claude", Preset::Anthropic)).expect("saved");
        let fault = bare.ai().key_set(&id, KeySlot::Provider, KEY_CANARY.to_owned()).expect_err("no keychain");
        assert_eq!((fault.code, fault.reason), ("key", Some("no_secure_store")));
        faults.push(fault);
        assert_ai_json_clean(&other);

        for fault in &faults {
            assert!(!printed(fault).contains(KEY_CANARY), "a fault shows the key: {fault:?}");
        }
        for answer in &answers {
            assert!(!answer.contains(KEY_CANARY), "an answer shows the key");
        }
        assert!(!format!("{:?}", rec.all()).contains(KEY_CANARY), "an event shows the key");

        // The input has no member for a key, and prints no name or address.
        assert!(
            serde_json::from_value::<ProfileInputMsg>(json!({"name": "x", "preset": "anthropic", "extra": 1})).is_err()
        );
        assert!(
            serde_json::from_value::<ProfileInputMsg>(json!({"name": "x", "preset": "anthropic", "api_key": "k"}))
                .is_err()
        );
        let mut msg = profile_msg("NameMarker-93", Preset::Custom);
        msg.base_url = "AddressMarker-77".to_owned();
        let text = format!("{msg:?} {msg:#?}");
        assert!(!text.contains("NameMarker-93") && !text.contains("AddressMarker-77"), "{text}");
    }

    // ---- refusals ---------------------------------------------------------------------------------------------

    #[test]
    fn a_refusal_reaches_the_page_as_a_code_a_reason_and_a_fixed_text() {
        let scratch = Scratch::new("faults");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(&core, "slow", scripted("slow", Locality::OnDevice).with_turn(stalling()));
        let pane = pane_of(&core);
        let chat = core.ai().conversation_new().expect("an id");
        let mut faults = Vec::new();

        let fault = core.ai_send(&chat, "  \n\t".to_owned(), pane, Some("slow".to_owned()), None).expect_err("empty");
        assert_eq!((fault.code, fault.reason), ("empty_text", None));
        assert_eq!(fault.text, ServiceError::EmptyText.to_string());
        faults.push(fault);

        let long = format!("{TEXT_CANARY}{}", "x".repeat(131_073 - TEXT_CANARY.len()));
        assert_eq!(long.len(), 131_073);
        let fault = core.ai_send(&chat, long, pane, Some("slow".to_owned()), None).expect_err("too long");
        assert_eq!(fault.code, "text_too_long");
        assert!(!printed(&fault).contains(TEXT_CANARY), "a fault shows the text");
        faults.push(fault);

        let fault = core.ai_send(&chat, "hello".to_owned(), pane, None, None).expect_err("no provider");
        assert_eq!(fault.code, "no_profile");
        faults.push(fault);

        // A chat runs one turn at a time, and a cancel ends it.
        core.ai_send(&chat, "go".to_owned(), pane, Some("slow".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(5), || rec.saw(&chat, "delta")), "the turn is running");
        let fault = core.ai_send(&chat, "again".to_owned(), pane, Some("slow".to_owned()), None).expect_err("busy");
        assert_eq!(fault.code, "busy");
        faults.push(fault);
        assert_eq!(core.ai().cancel(&chat), Ok(true));
        assert!(wait_for(Duration::from_secs(1), || rec.ended(&chat)), "a cancel ends the turn");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], "cancelled");

        for fault in &faults {
            let value = json_of(fault);
            let mut members: Vec<&str> = value.as_object().expect("an object").keys().map(String::as_str).collect();
            members.sort_unstable();
            assert_eq!(members, ["code", "reason", "text"]);
            assert!(!fault.text.is_empty());
        }
    }

    // ---- settings ---------------------------------------------------------------------------------------------

    #[test]
    fn local_only_reaches_the_service_at_start_on_a_change_and_on_a_reload() {
        // (a) A settings file that says local only is honoured before the first command can run.
        {
            let scratch = Scratch::new("local-start");
            std::fs::write(scratch.0.join(SETTINGS_FILE), r#"{"ai_local_only": true}"#).expect("settings.json");
            let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
            let cloud = register(&core, "cloud", scripted("cloud", Locality::Cloud).with_turn(Script::reply("never")));
            let device =
                register(&core, "device", scripted("device", Locality::OnDevice).with_turn(Script::reply("fine")));
            let pane = pane_of(&core);

            let one = core.ai().conversation_new().expect("an id");
            core.ai_send(&one, "hello".to_owned(), pane, Some("cloud".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || rec.ended(&one)));
            let end = rec.end_of(&one).expect("an end");
            assert_eq!(end["end"], "failed");
            assert_eq!(end["error"]["class"], "policy");
            assert_eq!(cloud.calls(), 0, "the cloud provider was not called");

            let two = core.ai().conversation_new().expect("an id");
            core.ai_send(&two, "hello".to_owned(), pane, Some("device".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || rec.ended(&two)));
            assert_eq!(rec.end_of(&two).expect("an end")["end"], "done");
            assert_eq!(device.calls(), 1);
        }

        // (b) A change stops the turn that leaves the device and not the one that stays.
        {
            let scratch = Scratch::new("local-change");
            let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
            let cloud = register(
                &core,
                "cloud",
                scripted("cloud", Locality::Cloud).with_turn(stalling()).with_turn(Script::reply("again")),
            );
            register(&core, "device", scripted("device", Locality::OnDevice).with_turn(stalling()));
            let pane = pane_of(&core);

            let away = core.ai().conversation_new().expect("an id");
            let here = core.ai().conversation_new().expect("an id");
            core.ai_send(&away, "hello".to_owned(), pane, Some("cloud".to_owned()), None).expect("sent");
            core.ai_send(&here, "hello".to_owned(), pane, Some("device".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || rec.saw(&away, "delta") && rec.saw(&here, "delta")));

            core.set_setting("ai_local_only", json!(true)).expect("saved");
            assert!(wait_for(Duration::from_secs(1), || rec.ended(&away)), "the cloud turn is stopped");
            assert_eq!(rec.end_of(&away).expect("an end")["end"], "cancelled");
            std::thread::sleep(Duration::from_millis(300));
            assert!(!rec.ended(&here), "the on-device turn goes on");

            core.reset_setting("ai_local_only").expect("reset");
            let again = core.ai().conversation_new().expect("an id");
            core.ai_send(&again, "hello".to_owned(), pane, Some("cloud".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || rec.ended(&again)));
            assert_eq!(rec.end_of(&again).expect("an end")["end"], "done");
            assert_eq!(cloud.calls(), 2, "the cloud provider is reached again");
            core.ai().cancel(&here).expect("a valid id");
        }

        // (c) A hand edit of the file is picked up by a reload.
        {
            let scratch = Scratch::new("local-reload");
            let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
            register(&core, "cloud", scripted("cloud", Locality::Cloud).with_turn(stalling()));
            let pane = pane_of(&core);
            let chat = core.ai().conversation_new().expect("an id");
            core.ai_send(&chat, "hello".to_owned(), pane, Some("cloud".to_owned()), None).expect("sent");
            assert!(wait_for(Duration::from_secs(5), || rec.saw(&chat, "delta")));

            std::fs::write(scratch.0.join(SETTINGS_FILE), r#"{"ai_local_only": true}"#).expect("settings.json");
            core.reload_settings();

            assert!(wait_for(Duration::from_secs(1), || rec.ended(&chat)), "the cloud turn is stopped");
            assert_eq!(rec.end_of(&chat).expect("an end")["end"], "cancelled");
        }
    }

    // ---- the end of the app and closed panes -----------------------------------------------------------------

    #[test]
    fn ending_the_app_ends_running_turns_and_cancel_stops_one() {
        let scratch = Scratch::new("exit");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(&core, "slow", scripted("slow", Locality::OnDevice).with_turn(stalling()).with_turn(stalling()));
        let pane = pane_of(&core);

        // A cancel stops one turn and says so.
        let one = core.ai().conversation_new().expect("an id");
        core.ai_send(&one, "hello".to_owned(), pane, Some("slow".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(5), || rec.saw(&one, "delta")));
        assert_eq!(core.ai().cancel(&one), Ok(true));
        assert!(wait_for(Duration::from_secs(1), || rec.ended(&one)));
        assert_eq!(rec.end_of(&one).expect("an end")["end"], "cancelled");
        assert_eq!(core.ai().cancel(&one), Ok(false), "the turn is over");

        // The end of the app stops the rest, and a send after it is refused.
        let two = core.ai().conversation_new().expect("an id");
        core.ai_send(&two, "hello".to_owned(), pane, Some("slow".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(5), || rec.saw(&two, "delta")));
        core.shutdown();
        assert!(wait_for(Duration::from_secs(2), || rec.ended(&two)), "shutdown ends the turn");
        assert_eq!(rec.end_of(&two).expect("an end")["end"], "cancelled");
        assert_eq!(core.ai().service().active_turns(), 0);
        let three = core.ai().conversation_new().expect("an id");
        let fault = core.ai_send(&three, "late".to_owned(), pane, Some("slow".to_owned()), None).expect_err("refused");
        assert_eq!(fault.code, "shutting_down");
    }

    /// The text of the function whose signature holds `signature`: the lines after it up to the closing brace at
    /// its own indentation.
    fn body_of(source: &str, signature: &str) -> String {
        let lines: Vec<&str> = source.lines().collect();
        let start =
            lines.iter().position(|line| line.contains(signature)).unwrap_or_else(|| panic!("{signature} is gone"));
        let indent = lines[start].len() - lines[start].trim_start().len();
        let mut body = String::new();
        for line in &lines[start + 1..] {
            let here = line.len() - line.trim_start().len();
            if here == indent && line.trim_start().starts_with('}') {
                return body;
            }
            body.push_str(line);
            body.push('\n');
        }
        panic!("{signature} is not closed");
    }

    #[test]
    fn a_pane_that_leaves_the_layout_is_told_to_the_service_by_every_path() {
        // The core is told in one place, and every path that takes a pane out of a layout goes through it.
        let source = include_str!("state.rs");
        assert!(body_of(source, "fn forget_pane(").contains("self.ai.pane_closed(pane)"));
        for path in ["fn close_pane_in(", "fn end_panes(", "fn window_closed(", "fn pane_gone("] {
            assert!(body_of(source, path).contains("self.forget_pane("), "{path} does not forget the pane");
        }
        // The settings and the end of the app reach the service too.
        for place in ["fn new_with_ai(", "fn set_setting(", "fn reload_settings("] {
            assert!(body_of(source, place).contains("apply_settings"), "{place} does not apply the settings");
        }
        assert!(body_of(source, "pub fn shutdown(").contains("self.ai.shutdown()"));
        assert!(!body_of(source, "fn reset_setting(").contains("apply_settings"), "reset goes through set_setting");
        assert!(body_of(source, "fn reset_setting(").contains("self.set_setting("));

        // Closing a pane of two still works, and the pane is not open any more.
        let scratch = Scratch::new("panes");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let view = core.split_in(MAIN_WINDOW, true).expect("split");
        let ids: Vec<PaneId> = view.panes.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| core.pane_is_open(*id)));
        let closing = ids[1];
        assert!(core.close_pane_in(MAIN_WINDOW, closing).is_ok());
        assert!(!core.pane_is_open(closing));
        assert!(core.pane_is_open(ids[0]));
        assert!(rec.all().iter().any(|(_, out)| matches!(out, Out::Layout(_))), "the layout was announced");
        let fault =
            core.ai_send(&core.ai().conversation_new().expect("an id"), "hello".to_owned(), closing, None, None);
        assert_eq!(fault.expect_err("refused").code, "no_such_pane");
    }

    // ---- the recorded run -------------------------------------------------------------------------------------

    /// What stands for the real, random chat id in the recorded run.
    const GOLDEN_CHAT: &str = "chat-golden";

    /// `value` with every string that is `real` replaced by the fixed chat id.
    fn anonymise(value: &mut Value, real: &str) {
        match value {
            Value::String(text) if text == real => *text = GOLDEN_CHAT.to_owned(),
            Value::Array(items) => items.iter_mut().for_each(|item| anonymise(item, real)),
            Value::Object(members) => members.values_mut().for_each(|member| anonymise(member, real)),
            _ => {}
        }
    }

    /// Every member name of every object in `value`.
    fn member_names(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Array(items) => items.iter().for_each(|item| member_names(item, out)),
            Value::Object(members) => {
                for (name, member) in members {
                    out.push(name.clone());
                    member_names(member, out);
                }
            }
            _ => {}
        }
    }

    /// Plays three turns on the scripted provider (one that finishes with reasoning and usage, one that fails, one
    /// that is cancelled while it streams) and records every `ai` event, the state after each, and the snapshots
    /// taken live. Nothing reads a clock and nothing sleeps for a fixed time.
    fn record_run() -> Value {
        let scratch = Scratch::new("stream");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(
            &core,
            "scripted",
            scripted("scripted", Locality::OnDevice)
                .with_turn(Script::new().reasoning("Checking the question.").text("Hello there.").usage(11, 5))
                .with_turn(Script::new().fail(AiError::Overloaded))
                .with_turn(stalling()),
        );
        let chat = core.ai().conversation_new().expect("an id");
        let pane = pane_of(&core);
        let send =
            |text: &str| core.ai_send(&chat, text.to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        let snapshot = || json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        let last_index = || rec.ai_events().len() - 1;
        let wait = Duration::from_secs(5);
        let mut snapshots: Vec<(usize, Value)> = Vec::new();

        send("What is a pty?");
        assert!(wait_for(wait, || rec.ended_turns(&chat) == 1));
        snapshots.push((last_index(), snapshot()));

        send("And a tty?");
        assert!(wait_for(wait, || rec.ended_turns(&chat) == 2));
        snapshots.push((last_index(), snapshot()));

        let third = send("Explain a pipe.");
        let streaming = || rec.ai_events().iter().any(|(_, p)| p.0.turn == third.turn && p.0.kind.name() == "delta");
        assert!(wait_for(wait, streaming));
        snapshots.push((last_index(), snapshot()));
        assert_eq!(core.ai().cancel(&chat), Ok(true));
        assert!(wait_for(wait, || rec.ended_turns(&chat) == 3));
        snapshots.push((last_index(), snapshot()));

        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut state = ChatState::new(&chat);
        let mut states: Vec<Value> = Vec::new();
        for payload in &events {
            assert_eq!(state.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
            states.push(json_of(&state));
        }

        // Three turns, each counting from 1 without a hole, in the kinds the script makes.
        let mut turns: Vec<u64> = events.iter().map(|p| p.0.turn).collect();
        turns.dedup();
        assert_eq!(turns.len(), 3, "three different turn numbers");
        let of_turn = |turn: u64| events.iter().filter(move |p| p.0.turn == turn);
        for turn in &turns {
            let seqs: Vec<u64> = of_turn(*turn).map(|p| p.0.seq).collect();
            assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "turn {turn}");
        }
        let kinds = |turn: u64| of_turn(turn).map(|p| p.0.kind.name()).collect::<Vec<_>>();
        assert_eq!(kinds(turns[0]), ["turn_started", "reasoning", "delta", "usage", "message", "turn_ended"]);
        assert_eq!(kinds(turns[1]), ["turn_started", "turn_ended"]);
        assert_eq!(kinds(turns[2]), ["turn_started", "delta", "message", "turn_ended"]);
        let end_of = |turn: u64| json_of(&*of_turn(turn).next_back().expect("an event").0);
        assert_eq!(end_of(turns[0])["end"], "done");
        assert_eq!(end_of(turns[1])["end"], "failed");
        assert!(end_of(turns[1])["error"]["class"].is_string() && end_of(turns[1])["error"]["text"].is_string());
        assert_eq!(end_of(turns[2])["end"], "cancelled");
        let cut = of_turn(turns[2]).find(|p| p.0.kind.name() == "message").expect("the partial answer is kept");
        assert_eq!(json_of(&*cut.0)["message"]["status"], "cancelled");

        // Every snapshot is the state the events fold to at that point, and the one in the middle of a turn runs.
        for (after, live) in &snapshots {
            assert_eq!(live, &states[*after], "the snapshot after event {after}");
        }
        assert_eq!(snapshots[2].1["running"], true);

        let mut document = json!({
            "conversation": chat,
            "events": events.iter().map(|payload| json_of(&*payload.0)).collect::<Vec<_>>(),
            "states": states,
            "snapshots": snapshots.iter().map(|(after, live)| json!({"after": after, "state": live})).collect::<Vec<_>>(),
        });
        anonymise(&mut document, &chat);
        document
    }

    fn stream_golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/ai_stream.json")
    }

    fn update_fixtures() -> bool {
        std::env::var("UPDATE_FIXTURES").is_ok_and(|value| value == "1")
    }

    #[test]
    fn the_scripted_run_is_stable_and_matches_the_ai_stream_golden() {
        let first = record_run();
        let second = record_run();
        assert_eq!(first, second, "two runs of the same script record the same stream");

        // The page gets views: no provider-native turn, no stored context, no key, and none of the canaries.
        let mut names = Vec::new();
        member_names(&first, &mut names);
        for forbidden in ["native", "stored", "api_key", "key"] {
            assert!(!names.iter().any(|name| name == forbidden), "the stream has a member named {forbidden}");
        }
        let text = first.to_string();
        assert!(!text.contains(TEXT_CANARY) && !text.contains(KEY_CANARY));

        let mut expected = serde_json::to_string_pretty(&first).expect("json");
        expected.push('\n');
        let path = stream_golden_path();
        if update_fixtures() {
            std::fs::write(&path, &expected).expect("write ai_stream.json");
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
        assert!(
            on_disk == expected,
            "the recorded run differs from ai_stream.json; review it and run with UPDATE_FIXTURES=1"
        );
    }

    // ---- shared content: what a pane or a file gives ---------------------------------------------------------

    use holzwave_core::ai::service::{CUT_NOTE, READ_BYTES};
    use holzwave_core::blocks::BlockKind;
    use holzwave_core::layout::PaneId as Pane;
    use holzwave_core::engine::{SelectMode, SelectOp};

    /// The text a pane shows in these tests; it appears in test code only.
    const SHELL_TEXT: &str = "zq-shell-text-52c1e8";
    /// Text drawn with the conceal attribute.
    const SHELL_HIDDEN: &str = "zq-shell-hidden-b41";
    /// What a secret file holds.
    const FILE_SECRET: &str = "zq-envfile-9d03";

    /// A core whose panes run `script` through `/bin/sh -c`, over `dir`, with the keychain unavailable.
    fn core_with_script(dir: &Path, script: &str) -> (Arc<AppCore>, Arc<AiRecorder>) {
        let rec = Arc::new(AiRecorder::default());
        let core = AppCore::new_with_ai(
            dir.to_path_buf(),
            rec.clone(),
            ShellSpec::new("/bin/sh", &["-c", script]),
            AiParts { vault: KeyVault::unavailable(), clock: Some(fixed_clock), protected: Vec::new() },
        );
        (core, rec)
    }

    /// A core with a running pane that shows the output of `script`, and a scripted on-device provider named
    /// `scripted` that answers one turn.
    fn core_showing(dir: &Path, script: &str) -> (Arc<AppCore>, Arc<AiRecorder>, Pane, Arc<ScriptedProvider>) {
        let (core, rec) = core_with_script(dir, script);
        let mut double = scripted("scripted", Locality::OnDevice);
        for _ in 0..4 {
            double = double.with_turn(Script::reply("ok"));
        }
        let provider = register(&core, "scripted", double);
        let pane = pane_of(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        (core, rec, pane, provider)
    }

    fn new_chat() -> String {
        ConversationId::generate().expect("an id").as_str().to_owned()
    }

    fn share(core: &AppCore, chat: &str, pane: Pane) -> ContextState {
        core.ai_context_share(chat, pane, Some("scripted".to_owned()), None, true).expect("a share")
    }

    /// Previews `kinds` until an item of at least `min_bytes` shows up: the program that draws the pane needs a moment.
    fn preview_until(core: &AppCore, chat: &str, pane: Pane, kinds: &[ContextKind], min_bytes: u64) -> OfferView {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = core.ai_context_preview(chat, pane, Some("scripted".to_owned()), None, kinds.to_vec());
            if let Ok(view) = &answer {
                if view.items.iter().map(|item| item.bytes).sum::<u64>() >= min_bytes {
                    return view.clone();
                }
            }
            assert!(Instant::now() < deadline, "no preview of {kinds:?} with {min_bytes} bytes: {answer:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Sends with the offer and the first item, and waits for the end of the turn.
    fn send_offer(core: &AppCore, rec: &AiRecorder, chat: &str, pane: Pane, view: &OfferView, picked: &[u32]) {
        let before = rec.ended_turns(chat);
        core.ai_send_with_context(
            chat,
            format!("explain {TEXT_CANARY}"),
            pane,
            Some("scripted".to_owned()),
            None,
            view.offer.clone(),
            picked.to_vec(),
        )
        .expect("the send is accepted");
        assert!(wait_for(Duration::from_secs(5), || rec.ended_turns(chat) > before), "the turn ends");
    }

    /// The envelope of the single approved context of request number `index` that `provider` saw.
    fn envelope_sent(provider: &ScriptedProvider, index: usize) -> String {
        let requests = provider.requests();
        assert!(requests.len() > index, "the provider saw {} requests", requests.len());
        assert_eq!(requests[index].context.len(), 1, "one approved context travels");
        requests[index].context[0].envelope().to_owned()
    }

    fn everything_serialised(rec: &AiRecorder, views: &[Value]) -> String {
        let events: Vec<Value> = rec.ai_events().iter().map(|(_, payload)| json_of(payload)).collect();
        format!("{}{}", Value::Array(events), Value::Array(views.to_vec()))
    }

    #[test]
    fn a_live_pane_screen_is_offered_read_and_only_the_approved_bytes_reach_the_provider() {
        let scratch = Scratch::new("ctx-live");
        let hidden = format!("printf '\\033[8m{SHELL_HIDDEN}\\033[0m after\\r\\n'");
        let script = format!("printf '{SHELL_TEXT}\\r\\n'; {hidden}; sleep 30");
        let (core, rec, pane, provider) = core_showing(&scratch.0, &script);
        let chat = new_chat();

        // What the pane offers, before anything is read.
        let state = core.ai_context_state(&chat, pane, Some("scripted".to_owned()), None).expect("a state");
        let shown = json_of(&state);
        assert_eq!(shown["kinds"], json!(["selection", "screen", "last_command"]));
        assert_eq!(shown["required"], json!(true));
        assert_eq!(shown["shared"], json!(false));
        assert_eq!(shown["locality"], json!("on_device"));

        // A pane that is not shared is not read.
        let refused = core
            .ai_context_preview(&chat, pane, Some("scripted".to_owned()), None, vec![ContextKind::Screen])
            .expect_err("not shared");
        assert_eq!((refused.code, refused.reason), ("context", Some("not_shared")));

        // Shared: the screen is read from the grid, and the hidden cells are reported.
        assert_eq!(json_of(&share(&core, &chat, pane))["shared"], json!(true));
        let view = preview_until(&core, &chat, pane, &[ContextKind::Screen], 20);
        assert_eq!(view.items.len(), 1);
        assert_eq!(view.items[0].label, format!("Screen of pane {pane}"));
        assert!(view.items[0].findings.iter().any(|finding| finding.rule == "hidden_text"), "{view:?}");

        send_offer(&core, &rec, &chat, pane, &view, &[1]);
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));
        assert_eq!(provider.requests().len(), 1, "one send is one request");
        let envelope = envelope_sent(&provider, 0);
        assert!(envelope.contains(SHELL_TEXT) && envelope.contains(" after"), "{envelope}");
        assert!(!envelope.contains(SHELL_HIDDEN), "the hidden text reached the envelope");

        // The turn carries a note with the label and the size, and no view holds text.
        let started = rec
            .ai_events()
            .into_iter()
            .find(|(_, payload)| payload.0.kind.name() == "turn_started")
            .map(|(_, payload)| json_of(&payload))
            .expect("turn_started");
        let parts = started["message"]["parts"].as_array().expect("parts");
        let note = parts.iter().find(|part| part["type"] == "context").expect("a context part");
        assert_eq!(note["items"][0]["label"], json!(format!("Screen of pane {pane}")));
        assert_eq!(note["items"][0]["bytes"], json!(view.items[0].bytes));
        let wire = everything_serialised(&rec, &[json_of(&state), json_of(&view)]);
        for text in [SHELL_TEXT, " after", SHELL_HIDDEN] {
            assert!(!wire.contains(text), "{text:?} is in an event or a view");
        }
        assert!(wire.contains(&format!("Screen of pane {pane}")));
    }

    #[test]
    fn a_selection_a_preview_file_and_the_deny_list() {
        let scratch = Scratch::new("ctx-sel");
        let files = Scratch::new("ctx-files");
        let (core, rec, pane, provider) = core_showing(&scratch.0, "printf 'find me\\r\\nother'; sleep 30");
        let chat = new_chat();

        // A selection made through the core is read as selected, and nothing else.
        share(&core, &chat, pane);
        preview_until(&core, &chat, pane, &[ContextKind::Screen], 10);
        core.select(pane, SelectOp::Begin { x: 0, y: 0, mode: SelectMode::Cell }).expect("select");
        core.select(pane, SelectOp::Extend { x: 3, y: 0 }).expect("extend");
        let view = preview_until(&core, &chat, pane, &[ContextKind::Selection], 4);
        send_offer(&core, &rec, &chat, pane, &view, &[1]);
        let envelope = envelope_sent(&provider, 0);
        assert!(envelope.contains(">\nfind\n</holzwave-item>"), "{envelope}");

        // The files: a Markdown file offers a preview, another text file a file, anything else nothing.
        let long: String = "a line of text\n".repeat(READ_BYTES / 10);
        let paths: Vec<(&str, PathBuf)> = [
            ("notes.md", "# Title\nsome notes\n".to_owned()),
            ("plain.txt", "plain words\n".to_owned()),
            ("data.bin", "ab\0cd".to_owned()),
            (".env", format!("TOKEN={FILE_SECRET}\n")),
            ("big.txt", long),
        ]
        .into_iter()
        .map(|(name, text)| {
            let path = files.0.join(name);
            std::fs::write(&path, text).expect("a file");
            (name, path)
        })
        .collect();
        let inside = scratch.0.join("inside.txt");
        std::fs::write(&inside, format!("data {FILE_SECRET}\n")).expect("a file in the data directory");
        let open = |path: &Path| -> Pane {
            core.block_open_in(MAIN_WINDOW, "right", BlockKind::Preview, "local", path.to_str().expect("utf-8"))
                .expect("a preview pane")
                .focus
        };
        let panes: Vec<(&str, Pane)> = paths.iter().map(|(name, path)| (*name, open(path))).collect();
        let inside_pane = open(&inside);
        let folder = core
            .block_open_in(MAIN_WINDOW, "right", BlockKind::Files, "local", files.0.to_str().expect("utf-8"))
            .expect("a browser pane")
            .focus;
        let kinds_of = |pane: Pane| {
            let state = core.ai_context_state(&chat, pane, Some("scripted".to_owned()), None).expect("a state");
            json_of(&state)["kinds"].clone()
        };
        let pane_named = |name: &str| panes.iter().find(|(n, _)| *n == name).map(|(_, pane)| *pane).expect("a pane");
        assert_eq!(kinds_of(pane_named("notes.md")), json!(["preview"]));
        assert_eq!(kinds_of(pane_named("plain.txt")), json!(["file"]));
        assert_eq!(kinds_of(pane_named("data.bin")), json!([]));
        assert_eq!(kinds_of(pane_named(".env")), json!([]));
        assert_eq!(kinds_of(inside_pane), json!([]));
        assert_eq!(kinds_of(folder), json!([]));

        // A direct read of the two places the deny list protects is refused before a byte is read.
        let source = &core.ai().context;
        assert_eq!(source.read(pane_named(".env"), ContextKind::File, READ_BYTES).err(), Some(ReadRefusal::Denied));
        assert_eq!(source.read(inside_pane, ContextKind::File, READ_BYTES).err(), Some(ReadRefusal::Denied));
        let env_rule = source.deny.check_local(&paths[3].1, Access::Read).expect_err("denied").rule();
        let dir_rule = source.deny.check_local(&inside, Access::Read).expect_err("denied").rule();
        assert_eq!((env_rule, dir_rule), ("env_file", "protected_dir"));
        let refused = |name: &str, kind: ContextKind| source.read(pane_named(name), kind, READ_BYTES).err();
        assert_eq!(refused("data.bin", ContextKind::File), Some(ReadRefusal::NotText));
        assert_eq!(refused("notes.md", ContextKind::File), Some(ReadRefusal::Unsupported));

        // The label is the file name and the text is the file as saved.
        for (at, (name, kind, text)) in
            [("notes.md", ContextKind::Preview, "some notes"), ("plain.txt", ContextKind::File, "plain words")]
                .into_iter()
                .enumerate()
        {
            let target = pane_named(name);
            core.ai_context_share(&chat, target, Some("scripted".to_owned()), None, true).expect("a share");
            let view = preview_until(&core, &chat, target, &[kind], 5);
            assert_eq!(view.items[0].label, name);
            send_offer(&core, &rec, &chat, target, &view, &[1]);
            let envelope = envelope_sent(&provider, at + 1);
            assert!(envelope.contains(text) && envelope.contains(&format!("label=\"{name}\"")), "{envelope}");
        }

        // A file larger than the read limit is cut at it, and the label says so.
        let big = pane_named("big.txt");
        core.ai_context_share(&chat, big, Some("scripted".to_owned()), None, true).expect("a share");
        let view = preview_until(&core, &chat, big, &[ContextKind::File], 5);
        assert!(view.items[0].label.ends_with(CUT_NOTE), "{}", view.items[0].label);

        // The secret of the protected files is nowhere.
        let wire = everything_serialised(&rec, &[]);
        let requests = format!("{:?}", provider.requests());
        assert!(!wire.contains(FILE_SECRET) && !requests.contains(FILE_SECRET));
    }

    #[test]
    fn a_closed_pane_or_chat_pane_ends_the_share_and_the_context_calls_refuse_a_pane_that_is_not_open() {
        let scratch = Scratch::new("ctx-close");
        let (core, _rec, pane, provider) = core_showing(&scratch.0, "printf 'text on the screen\\r\\n'; sleep 30");
        let chat = new_chat();
        let gone: Pane = 999_999_999;
        let scripted_id = || Some("scripted".to_owned());

        // A pane that is not open is refused first, by every call, with nothing stored and nothing read.
        let faults = [
            core.ai_context_state(&chat, gone, scripted_id(), None).err(),
            core.ai_context_share(&chat, gone, scripted_id(), None, true).err(),
            core.ai_context_preview(&chat, gone, scripted_id(), None, vec![ContextKind::Screen]).err(),
            core.ai_send_with_context(&chat, "x".to_owned(), gone, scripted_id(), None, "0".repeat(32), vec![1]).err(),
        ];
        for fault in faults {
            let fault = fault.expect("refused");
            let shown = (fault.code, fault.reason, fault.text.as_str());
            assert_eq!(shown, ("no_such_pane", None, "that pane is not open"));
        }
        assert_eq!(provider.calls(), 0);
        assert!(!scratch.chats().exists(), "nothing was stored");
        assert!(!core.ai().service().is_shared(&ConversationId::parse(&chat).expect("an id"), gone));

        // A pane that closes ends its share and voids its offer.
        let second = core.split_in(MAIN_WINDOW, false).expect("a split").focus;
        core.start_pane(second, 80, 24, 0, 0).expect("the pane starts");
        let id = ConversationId::parse(&chat).expect("an id");
        share(&core, &chat, second);
        preview_until(&core, &chat, second, &[ContextKind::Screen], 1);
        assert!(core.ai().service().is_shared(&id, second));
        assert_eq!(core.ai().service().gate().pending(), 1);
        core.close_pane_in(MAIN_WINDOW, second).expect("the pane closes");
        assert!(!core.ai().service().is_shared(&id, second));
        assert_eq!(core.ai().service().gate().pending(), 0, "the offer went with the pane");

        // A chat pane that closes ends the shares of its chat on the other panes, and only of its chat.
        let other = new_chat();
        let other_id = ConversationId::parse(&other).expect("an id");
        share(&core, &chat, pane);
        share(&core, &other, pane);
        let chat_pane = core
            .block_open_in(MAIN_WINDOW, "right", BlockKind::Chat, "local", &chat)
            .expect("a chat pane")
            .focus;
        assert!(core.ai().service().is_shared(&id, pane) && core.ai().service().is_shared(&other_id, pane));
        core.close_pane_in(MAIN_WINDOW, chat_pane).expect("the chat pane closes");
        assert!(core.pane_is_open(pane));
        assert!(!core.ai().service().is_shared(&id, pane), "the chat of the closed pane lost its share");
        assert!(core.ai().service().is_shared(&other_id, pane), "another chat keeps its share");

        // The hooks are where the plan says: the block is read, forgotten, the chat told, the pane told last.
        let forget = body_of(include_str!("state.rs"), "fn forget_pane(");
        let at = |needle: &str| forget.find(needle).unwrap_or_else(|| panic!("{needle} is not in forget_pane"));
        assert!(at("block_of") < at("blocks.forget") && at("blocks.forget") < at("chat_closed"));
        assert!(at("chat_closed") < at("self.ai.pane_closed(pane)"));
        assert!(forget.trim_end().ends_with("self.ai.pane_closed(pane);"), "pane_closed is the last statement");
    }

    #[test]
    fn the_last_command_follows_the_engine_verdict() {
        // The engine was shown to bound the output of a command by its marks, so the kind is offered and read.
        let scratch = Scratch::new("ctx-last");
        let mark = |name: &str| format!("printf '\\033]133;{name}\\033\\\\'");
        let marks = [
            mark("A"),
            "printf '$ '".to_owned(),
            mark("B"),
            "printf 'echo hi\\r\\n'".to_owned(),
            mark("C"),
            "printf 'hi\\r\\n'".to_owned(),
            mark("D;0"),
            mark("A"),
            "printf '$ '".to_owned(),
            "sleep 30".to_owned(),
        ]
        .join("; ");
        let (core, rec, pane, provider) = core_showing(&scratch.0, &marks);
        let chat = new_chat();
        let state = core.ai_context_state(&chat, pane, Some("scripted".to_owned()), None).expect("a state");
        assert!(state.kinds.contains(&ContextKind::LastCommand), "{state:?}");
        share(&core, &chat, pane);
        let view = preview_until(&core, &chat, pane, &[ContextKind::LastCommand], 1);
        assert_eq!(view.items[0].label, format!("Last command output of pane {pane}"));
        send_offer(&core, &rec, &chat, pane, &view, &[1]);
        let envelope = envelope_sent(&provider, 0);
        assert!(envelope.contains(">\nhi\n</holzwave-item>"), "{envelope}");
        assert!(!envelope.contains("echo hi"), "the typed command is prompt content, not output");

        // A pane that has written no marks has nothing there: the kind is listed under `empty`.
        let plain = Scratch::new("ctx-plain");
        let (core, _rec, pane, _provider) = core_showing(&plain.0, "printf 'plain output\\r\\n'; sleep 30");
        let chat = new_chat();
        share(&core, &chat, pane);
        let kinds = [ContextKind::LastCommand, ContextKind::Screen];
        let view = loop {
            let view = preview_until(&core, &chat, pane, &kinds, 5);
            if !view.items.is_empty() {
                break view;
            }
        };
        assert_eq!(view.empty, [ContextKind::LastCommand]);
        assert_eq!(view.items[0].kind, ContextKind::Screen);
    }

    #[test]
    fn the_host_has_no_path_to_the_gate_and_the_context_calls_take_ids_only() {
        let whole = include_str!("ai_host.rs");
        let host = whole.split("\n#[cfg(test)]\nmod tests").next().expect("a first part");
        let code: String =
            host.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");

        // Nothing here can reach the gate, mint an approval or decide anything.
        let words = [".gate()", "approve", "ApprovedContext", ".context(", "decide", "Decision", "Allow"];
        for word in words.into_iter().chain(["Target::Window"]) {
            assert!(!code.contains(word), "the host has {word:?}");
        }
        assert_eq!(code.matches(".offer(").count(), 1, "only the send with an offer attaches one");
        assert!(code.contains("read_text") && code.contains("with_hidden") && code.matches("DenyPolicy").count() >= 2);
        assert!(code.contains("LastOutput"), "the last command is read as the last output region");

        // The argument lists: ids, flags and kind names; the only text is the typed text of a send.
        for name in [
            "fn context_state(",
            "fn context_share(",
            "fn context_preview(",
            "fn ai_context_state(",
            "fn ai_context_share(",
            "fn ai_context_preview(",
        ] {
            let at = host.find(name).unwrap_or_else(|| panic!("{name} is gone"));
            let list = &host[at..at + host[at..].find(')').expect("a signature")];
            for forbidden in ["text:", "content:", "envelope:", "body:", "context:"] {
                assert!(!list.contains(forbidden), "{name} takes {forbidden}");
            }
        }
        for name in ["fn send_with_offer(", "fn ai_send_with_context("] {
            let at = host.find(name).unwrap_or_else(|| panic!("{name} is gone"));
            let list = &host[at..at + host[at..].find(')').expect("a signature")];
            assert!(list.contains("text: String,") && list.contains("offer: String,"), "{name}");
            assert_eq!(list.matches("String,").count(), 2, "{name}: the typed text and the offer id are the strings");
            for forbidden in ["content:", "envelope:", "body:", "context:"] {
                assert!(!list.contains(forbidden), "{name} takes {forbidden}");
            }
        }
    }

    #[test]
    fn a_send_names_both_the_offer_and_the_picked_items_or_neither() {
        let scratch = Scratch::new("ctx-ids");
        let (core, rec, pane, provider) = core_showing(&scratch.0, "printf 'on screen\\r\\n'; sleep 30");
        let chat = new_chat();
        let send = |offer: Option<&str>, picked: Option<Vec<u32>>| {
            core.ai_send_with(
                &chat,
                format!("hello {TEXT_CANARY}"),
                pane,
                Some("scripted".to_owned()),
                None,
                offer.map(str::to_owned),
                picked,
            )
        };

        // One of the two alone is refused as a bad pick, and nothing is stored or sent.
        for fault in [send(Some(&"a".repeat(32)), None), send(None, Some(vec![1]))] {
            let fault = fault.expect_err("refused");
            assert_eq!((fault.code, fault.reason), ("context", Some("bad_pick")));
        }
        assert_eq!(provider.calls(), 0);
        assert!(!scratch.chats().exists(), "nothing was stored");

        // Both go to the offer: one nobody made is a stale offer, and one that is not an id is too.
        for offer in ["a".repeat(32), "not an offer".to_owned()] {
            let fault = send(Some(&offer), Some(vec![1])).expect_err("refused");
            assert_eq!((fault.code, fault.reason), ("context", Some("stale_offer")));
        }
        assert!(!scratch.chats().exists(), "nothing was stored");

        // Neither is the text-only send of before.
        send(None, None).expect("a text send");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].context.is_empty(), "no context travels with a typed message");
    }

    // ---- tools ------------------------------------------------------------------------------------------------

    use holzwave_core::ai::types::{Role, StopReason};

    /// What a file the tools read holds; it appears in test code only.
    const TOOL_TEXT: &str = "zq-tool-text-7e21c4";

    /// `<dir>/proj/notes.txt`, holding `alpha beta ` and the canary: the canonical path of the file.
    fn tool_tree(dir: &Path) -> PathBuf {
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).expect("a folder");
        let notes = proj.join("notes.txt");
        std::fs::write(&notes, format!("alpha beta {TOOL_TEXT}\n")).expect("a file");
        notes.canonicalize().expect("a canonical path")
    }

    /// An on-device provider whose model can call tools.
    fn scripted_tools(id: &str) -> ScriptedProvider {
        let capabilities = Capabilities { tools: true, ..Capabilities::conservative(Locality::OnDevice) };
        ScriptedProvider::new(id).with_capabilities(capabilities)
    }

    /// One response of the model: a call of a tool, which ends the response for tool use.
    fn tool_call_turn(call_id: &str, name: &str, args: Value) -> Script {
        Script::new().tool_call(call_id, name, args).stop(StopReason::ToolUse)
    }

    /// One response of the model: a final text.
    fn text_turn(text: &str) -> Script {
        Script::reply(text)
    }

    /// The events of `kind` for `chat` that were sent so far, in order, as the page receives them.
    fn events_of(rec: &AiRecorder, chat: &str, kind: &str) -> Vec<Value> {
        rec.ai_events()
            .iter()
            .filter(|(_, payload)| payload.0.conversation == chat && payload.0.kind.name() == kind)
            .map(|(_, payload)| json_of(payload))
            .collect()
    }

    /// The `nth` (from 0) event of `kind` for `chat`, waited for.
    fn nth_event(rec: &AiRecorder, chat: &str, kind: &str, nth: usize) -> Value {
        assert!(
            wait_for(Duration::from_secs(5), || events_of(rec, chat, kind).len() > nth),
            "no event number {nth} of kind {kind}"
        );
        events_of(rec, chat, kind).remove(nth)
    }

    /// The first event of `kind` for `chat`, waited for.
    fn event_of(rec: &AiRecorder, chat: &str, kind: &str) -> Value {
        nth_event(rec, chat, kind, 0)
    }

    /// The page's verdict on a card.
    fn decision(core: &AppCore, chat: &str, call_id: &str, verdict: &str, hash: &str) -> Result<(), AiFault> {
        core.ai_decide(chat, call_id, verdict, hash)
    }

    /// `state` as a chat that was saved holds it: the output of every tool is the fixed marker, because the store keeps
    /// no tool output as text unless the setting asks for it. A snapshot of a chat that is not running is read from the
    /// store, so it equals the fold of the events only after this.
    fn as_stored(mut state: Value) -> Value {
        if let Some(messages) = state["messages"].as_array_mut() {
            for part in messages.iter_mut().flat_map(|message| message["parts"].as_array_mut().into_iter().flatten()) {
                if part["type"] == "tool_result" {
                    part["content"] = json!(holzwave_core::ai::store::NOT_STORED);
                }
            }
        }
        state
    }

    /// The names of the fields of a card, in order.
    fn payload_names(card: &Value) -> Vec<String> {
        card["payload"]
            .as_array()
            .expect("a payload")
            .iter()
            .map(|field| field["name"].as_str().expect("a name").to_owned())
            .collect()
    }

    #[test]
    fn a_read_call_is_proposed_decided_on_its_hash_and_its_result_reaches_the_model() {
        let tree = Scratch::new("tool-tree");
        let scratch = Scratch::new("tool-trace");
        let notes = tool_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": notes})))
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);

        core.ai_send(&chat, "read my notes".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");

        // The card reaches every window as one flat event.
        let proposed = event_of(&rec, &chat, "tool_proposed");
        let mut members: Vec<&str> = proposed.as_object().expect("an object").keys().map(String::as_str).collect();
        members.sort_unstable();
        assert_eq!(members, ["conversation", "kind", "pending", "seq", "turn"]);
        let card = &proposed["pending"];
        assert_eq!((card["tool"].as_str(), card["effect"].as_str()), (Some("read_file"), Some("read")));
        let header = (card["pane"].as_u64(), card["target"].as_str(), card["remote"].as_bool());
        assert_eq!(header, (Some(pane), Some("This computer"), Some(false)));
        assert_eq!(
            payload_names(card),
            ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]
        );
        let field = |name: &str| {
            let fields = card["payload"].as_array().expect("a payload");
            fields.iter().find(|f| f["name"] == name).map(|f| f["value"].as_str().expect("a value").to_owned())
        };
        assert_eq!(field("resolved_path"), notes.to_str().map(str::to_owned));
        let hash = card["hash"].as_str().expect("a hash").to_owned();
        assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{hash}");
        assert_eq!((card["tainted"].as_bool(), card["taint"].as_array().map(Vec::len)), (Some(false), Some(0)));
        for (target, _) in rec.ai_events() {
            assert_eq!(target, Target::All, "every ai event is for every window");
        }

        // A snapshot taken while the card waits holds the same card.
        let waiting = json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        assert_eq!(waiting["running"], json!(true));
        assert_eq!(waiting["pending"], json!([card]), "the snapshot holds the card the event carried");

        // Nothing runs before a decision.
        std::thread::sleep(Duration::from_millis(300));
        assert!(events_of(&rec, &chat, "tool_result").is_empty(), "a result before the decision");
        assert_eq!(provider.calls(), 1, "the model waits for the result");

        // The page decides on the card's hash: the host reads the file and the result reaches the model.
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["call_id"], json!("call_1"));
        let facts = (result["tool"].as_str(), result["is_error"].as_bool(), result["redactions"].as_u64());
        assert_eq!(facts, (Some("read_file"), Some(false), Some(0)));
        let content = result["content"].as_str().expect("content");
        assert!(content.starts_with("alpha beta ") && content.contains(TOOL_TEXT), "{content}");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));

        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "the second request carries the result");
        let declared: Vec<&str> = requests[1].tools.iter().map(|spec| spec.name.as_str()).collect();
        let offered = ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"];
        assert_eq!(declared, offered, "the four read tools, the two write tools and the command tool, and nothing else");
        let last = requests[1].messages.last().expect("a message");
        assert_eq!(last.role, Role::Tool);
        let returned = last.parts.iter().any(|part| match part {
            Part::ToolResult(result) => result.call_id == "call_1" && result.content.contains(TOOL_TEXT),
            _ => false,
        });
        assert!(returned, "the file text is the result of the call");

        // The events fold to the snapshot, and `seq` has no hole.
        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut folded = ChatState::new(&chat);
        for payload in &events {
            assert_eq!(folded.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
        }
        let seqs: Vec<u64> = events.iter().map(|p| p.0.seq).collect();
        assert_eq!(seqs, (1..=events.len() as u64).collect::<Vec<_>>());
        let idle = core.ai().snapshot(&chat).expect("a snapshot");
        assert_eq!(as_stored(json_of(&folded)), json_of(&idle), "the fold of the events is the snapshot");
        assert!(idle.pending.is_empty());

        // The card is spent.
        let again = decision(&core, &chat, "call_1", "allow", &hash).expect_err("a second decision");
        assert_eq!((again.code, again.reason), ("tool", Some("unknown_call")));
    }

    #[test]
    fn a_wrong_hash_an_odd_verdict_and_a_deny_are_answered_without_the_file_reaching_the_model() {
        let tree = Scratch::new("tool-tree");
        let scratch = Scratch::new("tool-verdict");
        let notes = tool_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": notes})))
                .with_turn(text_turn("Understood.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);
        core.ai_send(&chat, "read my notes".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        let proposed = event_of(&rec, &chat, "tool_proposed");
        let hash = proposed["pending"]["hash"].as_str().expect("a hash").to_owned();

        let refused = [
            (decision(&core, &chat, "call_1", "allow", &"0".repeat(64)), ("tool", Some("hash_mismatch"))),
            (decision(&core, &chat, "call_1", "allow", &hash.to_uppercase()), ("tool", Some("hash_mismatch"))),
            (decision(&core, &chat, "call_1", "maybe", &hash), ("tool", Some("invalid_decision"))),
            (decision(&core, "../x", "call_1", "allow", &hash), ("invalid_conversation", None)),
        ];
        for (answer, (code, reason)) in refused {
            let fault = answer.expect_err("refused");
            assert_eq!((fault.code, fault.reason), (code, reason));
            let shown = printed(&fault);
            assert!(!shown.contains(&hash) && !shown.contains(&"0".repeat(64)), "a fault shows a hash");
        }
        let snapshot = json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        assert_eq!(snapshot["pending"], json!([proposed["pending"]]), "the card stays after every refusal");
        assert_eq!(provider.calls(), 1);

        // A denial is a verdict like the other: the model is told, and the file text never reaches it.
        decision(&core, &chat, "call_1", "deny", &hash).expect("a denial");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["is_error"], json!(true));
        assert_eq!(result["content"], json!("The user denied this action."));
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let sent = serde_json::to_string(&requests[1].messages).expect("serialises");
        assert!(sent.contains("The user denied this action."), "the model is told");
        assert!(!sent.contains(TOOL_TEXT), "the file text reached the model after a denial");
        assert!(!everything_serialised(&rec, &[]).contains(TOOL_TEXT), "the file text reached an event");

        // The command runs on a blocking thread of the async runtime of the app, where a call that classifies a
        // destination is refused. A decision only locks the table, so it answers as it does anywhere else.
        let spent = {
            let (core, chat) = (core.clone(), chat.clone());
            tauri::async_runtime::block_on(async move {
                tauri::async_runtime::spawn_blocking(move || core.ai_decide(&chat, "call_1", "deny", &hash)).await
            })
        };
        let fault = spent.expect("the blocking thread ends").expect_err("a card that was decided");
        assert_eq!((fault.code, fault.reason), ("tool", Some("unknown_call")));
    }

    // ---- the command tool --------------------------------------------------------------------------------------

    /// Text a command prints, and the two halves of a key-shaped value that a command assembles when it runs, so that the
    /// command text itself never holds the key; they appear in test code only.
    const CMD_OUT: &str = "zq-cmd-out-6a1f40";
    const KEY_HEAD: &str = "sk-ant-api03-";
    const KEY_TAIL: &str = "CANARY7f3aQ0123456789abcdefABCDEF";

    /// The canonical folder `<dir>/proj` that `tool_tree` made.
    fn proj_of(notes: &Path) -> PathBuf {
        notes.parent().expect("a folder").to_path_buf()
    }

    /// The `run_command` arguments for `command` in `cwd`.
    fn command_args(command: &str, cwd: &Path) -> Value {
        json!({"command": command, "cwd": cwd})
    }

    /// A core whose live pane runs `exec cat > <dir>/typed.txt`: whatever is typed into the pane lands in that file.
    #[cfg(unix)]
    fn core_with_typed_file(dir: &Path) -> (Arc<AppCore>, Arc<AiRecorder>, PathBuf) {
        let typed = dir.join("typed.txt");
        let (core, rec) = core_with_script(dir, &format!("exec cat > '{}'", typed.display()));
        core.start_pane(pane_of(&core), 80, 24, 0, 0).expect("the pane starts");
        (core, rec, typed)
    }

    #[cfg(unix)]
    #[test]
    fn a_command_call_shows_its_exact_text_is_decided_on_its_hash_and_runs_in_a_separate_process() {
        let tree = Scratch::new("cmd-tree");
        let scratch = Scratch::new("cmd-trace");
        let notes = tool_tree(&tree.0);
        let proj = proj_of(&notes);
        let (core, rec, typed) = core_with_typed_file(&scratch.0);
        let pid_file = scratch.0.join("sh.pid");
        let command = format!(
            "printf '%s\\n' '{CMD_OUT}'; printf '%s%s\\n' '{KEY_HEAD}' '{KEY_TAIL}'; echo $$ > '{}'",
            pid_file.display()
        );
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "run_command", command_args(&command, &proj)))
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);

        core.ai_send(&chat, "run the checks".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");

        // The card: the effect, the machine, every field in order, the folder the command will run in and a hash.
        let proposed = event_of(&rec, &chat, "tool_proposed");
        let card = &proposed["pending"];
        assert_eq!((card["tool"].as_str(), card["effect"].as_str()), (Some("run_command"), Some("exec")));
        let header = (card["pane"].as_u64(), card["target"].as_str(), card["remote"].as_bool());
        assert_eq!(header, (Some(u64::from(pane)), Some("This computer"), Some(false)));
        assert_eq!(
            payload_names(card),
            ["tool", "pane", "target", "remote", "command", "cwd", "timeout_secs", "resolved_cwd"]
        );
        let field = |name: &str| {
            let fields = card["payload"].as_array().expect("a payload");
            fields.iter().find(|f| f["name"] == name).map(|f| f["value"].as_str().expect("a value").to_owned())
        };
        assert_eq!(field("resolved_cwd"), proj.to_str().map(str::to_owned));
        assert!(field("command").is_some_and(|shown| shown.contains(CMD_OUT)), "the command is shown");
        let hash = card["hash"].as_str().expect("a hash").to_owned();
        assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{hash}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(!pid_file.exists(), "the command ran before it was approved");
        assert!(events_of(&rec, &chat, "tool_result").is_empty(), "a result before the decision");

        // The page decides on the card's hash: the host runs exactly that command in a process of its own.
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["call_id"], json!("call_1"));
        assert_eq!((result["tool"].as_str(), result["is_error"].as_bool()), (Some("run_command"), Some(false)));
        let content = result["content"].as_str().expect("content").to_owned();
        assert!(content.starts_with("Exit status: 0."), "{content}");
        assert!(content.contains(CMD_OUT), "{content}");
        assert!(content.contains("[REDACTED:"), "the key-shaped text was not redacted: {content}");
        assert!(!content.contains("CANARY7f3a"), "the key reached the result: {content}");
        let pid: u32 = std::fs::read_to_string(&pid_file).expect("the command wrote its pid").trim().parse().expect("a number");
        assert_ne!(pid, std::process::id(), "the command ran in a process of its own");

        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "the second request carries the result");
        let last = requests[1].messages.last().expect("a message");
        assert_eq!(last.role, Role::Tool);
        let returned = last.parts.iter().any(|part| match part {
            Part::ToolResult(result) => result.call_id == "call_1" && result.content == content && !result.is_error,
            _ => false,
        });
        assert!(returned, "the cleaned text is the result of the call");
        let key = format!("{KEY_HEAD}{KEY_TAIL}");
        for request in &requests {
            let sent = serde_json::to_string(&request.messages).expect("serialises");
            assert!(!sent.contains(&key), "the key reached a request");
        }

        // The events fold to the snapshot, and the key is in no event.
        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut folded = ChatState::new(&chat);
        for payload in &events {
            assert_eq!(folded.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
        }
        let idle = core.ai().snapshot(&chat).expect("a snapshot");
        assert_eq!(as_stored(json_of(&folded)), json_of(&idle), "the fold of the events is the snapshot");
        assert!(!everything_serialised(&rec, &[json_of(&idle)]).contains(&key), "the key reached an event");

        // Nothing was typed into the live pane.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(std::fs::read(&typed).expect("the file the pane writes"), b"", "something was typed into the pane");
    }

    /// The text the core's golden gives a stopped call: the page and the model hear the same sentence.
    fn golden_stopped() -> String {
        let golden: Value = serde_json::from_str(include_str!("../../tests/fixtures/ai_command_card.json")).expect("JSON");
        golden["texts"]["stopped"].as_str().expect("a sentence").to_owned()
    }

    /// Whether the process `pid` is alive; a zombie counts as dead.
    #[cfg(unix)]
    fn alive(pid: u32) -> bool {
        let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().expect("ps runs");
        let state = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        !state.is_empty() && !state.starts_with('Z')
    }

    /// The number a command wrote to `path`, waited for.
    #[cfg(unix)]
    fn pid_of(path: &Path) -> u32 {
        let mut found = None;
        assert!(
            wait_for(Duration::from_secs(5), || {
                found = std::fs::read_to_string(path).ok().and_then(|text| text.trim().parse::<u32>().ok());
                found.is_some()
            }),
            "the command did not write {}",
            path.display()
        );
        found.expect("a pid")
    }

    #[cfg(unix)]
    #[test]
    fn a_stop_ends_a_running_command_and_everything_it_started() {
        let tree = Scratch::new("stop-tree");
        let scratch = Scratch::new("stop-trace");
        let proj = proj_of(&tool_tree(&tree.0));
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let (bg_file, sh_file) = (scratch.0.join("bg.pid"), scratch.0.join("sh.pid"));
        let command = format!("sleep 300 & echo $! > '{}'; echo $$ > '{}'; wait", bg_file.display(), sh_file.display());
        register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "run_command", command_args(&command, &proj)))
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "run the checks");
        let hash = event_of(&rec, &chat, "tool_proposed")["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let (bg, sh) = (pid_of(&bg_file), pid_of(&sh_file));
        assert!(alive(bg) && alive(sh), "the command is running");

        // A stop reaches the running command: the call ends with the loop's sentence for a stopped call, the turn ends
        // `cancelled`, and neither the shell nor its helper is left.
        assert_eq!(core.ai().cancel(&chat), Ok(true));
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!((result["call_id"].as_str(), result["is_error"].as_bool()), (Some("call_1"), Some(true)));
        assert_eq!(result["content"].as_str(), Some(golden_stopped().as_str()));
        assert!(wait_for(Duration::from_secs(5), || !alive(bg) && !alive(sh)), "a process of the command is still alive");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("cancelled"));

        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut folded = ChatState::new(&chat);
        for payload in &events {
            assert_eq!(folded.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
        }
        let idle = core.ai().snapshot(&chat).expect("a snapshot");
        assert_eq!(as_stored(json_of(&folded)), json_of(&idle), "the fold of the events is the snapshot");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_in_a_protected_working_directory_is_refused_before_any_card() {
        let home = Scratch::new("cwall-home");
        let scratch = Scratch::new("cwall-data");
        let (core, rec) = core_protecting(&scratch.0, Vec::new());
        let data = scratch.0.canonicalize().expect("canonical");
        let root = home.0.canonicalize().expect("canonical");
        put(&root, "home/.ssh/config", &format!("secret {SECRET_TEXT}\n"));
        std::fs::create_dir_all(root.join("proj")).expect("a folder");
        std::os::unix::fs::symlink(root.join("home/.ssh"), root.join("proj/link")).expect("a link");
        let marker = root.join("marker");
        let places = [data.clone(), root.join("home/.ssh"), root.join("proj/link")];

        let mut provider = scripted_tools("scripted");
        for (n, place) in places.iter().enumerate() {
            let command = format!("touch '{}'", marker.display());
            provider = provider.with_turn(tool_call_turn(&format!("call_{n}"), "run_command", command_args(&command, place)));
            provider = provider.with_turn(text_turn("noted"));
        }
        let provider = register(&core, "scripted", provider);
        let chat = new_chat();
        let pane = pane_of(&core);
        for n in 0..places.len() {
            say(&core, &rec, &chat, pane, &format!("run number {n}"));
        }
        assert!(events_of(&rec, &chat, "tool_proposed").is_empty(), "a protected folder showed a card");
        let results = events_of(&rec, &chat, "tool_result");
        assert_eq!(results.len(), places.len());
        let refusal = ToolError::Denied("secret_location").to_string();
        for (n, result) in results.iter().enumerate() {
            assert_eq!(result["is_error"], json!(true), "call {n}");
            let text = result["content"].as_str().expect("content");
            assert!(!text.contains(root.to_str().expect("utf-8")) && !text.contains(".ssh"), "call {n}: {text}");
            assert_eq!(text, refusal, "call {n} does not give the fixed refusal");
        }
        assert!(!marker.exists(), "a command ran in a protected folder");
        let snapshot = json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        assert!(!everything_serialised(&rec, &[snapshot]).contains(SECRET_TEXT), "the secret reached an event or a view");
        let sent: String =
            provider.requests().iter().map(|r| serde_json::to_string(&r.messages).expect("serialises")).collect();
        assert!(!sent.contains(SECRET_TEXT), "the secret reached the model");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_command_is_an_error_row_with_its_output_and_the_turn_goes_on() {
        let tree = Scratch::new("fail-tree");
        let scratch = Scratch::new("fail-trace");
        let proj = proj_of(&tool_tree(&tree.0));
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "run_command", command_args("printf 'zq-cmd-err-2b9c73\\n' >&2; exit 3", &proj)))
                .with_turn(text_turn("It failed.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "run it");
        let hash = event_of(&rec, &chat, "tool_proposed")["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["is_error"], json!(true), "{result}");
        let content = result["content"].as_str().expect("content").to_owned();
        assert!(content.starts_with("Exit status: 3."), "{content}");
        assert!(content.contains("Standard error:") && content.contains("zq-cmd-err-2b9c73"), "{content}");

        // The turn goes on to its answer, and the model heard the same text as an error result.
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let heard = requests[1].messages.last().expect("a message").parts.iter().any(|part| match part {
            Part::ToolResult(result) => result.call_id == "call_1" && result.content == content && result.is_error,
            _ => false,
        });
        assert!(heard, "the model hears the whole text as an error result");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_after_untrusted_text_is_tainted_and_runs_only_on_its_own_decision() {
        let tree = Scratch::new("taint-tree");
        let scratch = Scratch::new("taint-trace");
        let notes = tool_tree(&tree.0);
        let proj = proj_of(&notes);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let (ran, denied) = (scratch.0.join("ran.txt"), scratch.0.join("denied.txt"));
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": notes})))
                .with_turn(tool_call_turn("call_2", "run_command", command_args(&format!("touch '{}'", ran.display()), &proj)))
                .with_turn(text_turn("Done."))
                .with_turn(tool_call_turn("call_3", "run_command", command_args(&format!("touch '{}'", denied.display()), &proj)))
                .with_turn(text_turn("Understood.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "read my notes and run the checks");
        let read_hash = event_of(&rec, &chat, "tool_proposed")["pending"]["hash"].as_str().expect("a hash").to_owned();
        assert_eq!(event_of(&rec, &chat, "tool_proposed")["pending"]["tainted"], json!(false));
        decision(&core, &chat, "call_1", "allow", &read_hash).expect("the read is allowed");

        // The command after the read is marked with the label of what was read, and only its own hash decides it.
        let card = nth_event(&rec, &chat, "tool_proposed", 1)["pending"].clone();
        assert_eq!(card["call_id"], json!("call_2"));
        assert_eq!(card["tainted"], json!(true));
        assert_eq!(card["taint"], json!(["output of read_file"]));
        let hash = card["hash"].as_str().expect("a hash").to_owned();
        assert_ne!(hash, read_hash);
        let refused = decision(&core, &chat, "call_2", "allow", &read_hash).expect_err("another card's hash");
        assert_eq!((refused.code, refused.reason), ("tool", Some("hash_mismatch")));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!ran.exists(), "a command ran on the hash of another card");
        decision(&core, &chat, "call_2", "allow", &hash).expect("the command is allowed on its own hash");
        assert!(wait_for(Duration::from_secs(5), || ran.exists()), "the command did not run");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)));

        // A denied command leaves no trace, and the model is told.
        start(&core, &chat, pane_of(&core), "and once more");
        let again = nth_event(&rec, &chat, "tool_proposed", 2)["pending"].clone();
        assert_eq!(again["call_id"], json!("call_3"));
        let hash = again["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_3", "deny", &hash).expect("a denial");
        assert!(wait_for(Duration::from_secs(5), || rec.ended_turns(&chat) == 2));
        assert!(!denied.exists(), "a denied command ran");
        let requests = provider.requests();
        let sent = serde_json::to_string(&requests.last().expect("a request").messages).expect("serialises");
        assert!(sent.contains("The user denied this action."), "the model is told");
    }

    // ---- the walls, the lifecycle and the recorded run --------------------------------------------------------

    use holzwave_core::ai::agent::STOPPED_RESULT;

    /// What the files that no tool may read hold, and what two notes hold; they appear in test code only.
    const SECRET_TEXT: &str = "zq-secret-text-4d18f6";
    const NOTES_A: &str = "zq-notes-a-2e7b90";
    const NOTES_B: &str = "zq-notes-b-9c31d5";

    /// Sends a text for `pane` on the route `scripted` and waits for the end of the turn: for a turn that needs no
    /// decision.
    fn say(core: &AppCore, rec: &AiRecorder, chat: &str, pane: Pane, text: &str) {
        let before = rec.ended_turns(chat);
        core.ai_send(chat, text.to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(10), || rec.ended_turns(chat) > before), "the turn ends");
    }

    /// Sends a text for `pane` and does not wait: the turn is going to stop at a card.
    fn start(core: &AppCore, chat: &str, pane: Pane, text: &str) {
        core.ai_send(chat, text.to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
    }

    /// The `end` of every turn of `chat` that ended, in order.
    fn ends_of(rec: &AiRecorder, chat: &str) -> Vec<String> {
        events_of(rec, chat, "turn_ended").iter().map(|event| event["end"].as_str().unwrap_or("").to_owned()).collect()
    }

    /// Writes `text` to `<dir>/<name>`, making the folders, and returns the canonical path.
    fn put(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a folder");
        std::fs::write(&path, text).expect("a file");
        path.canonicalize().expect("a canonical path")
    }

    /// A core over `data` that also protects `extra`, with the recording sink and the fixed clock.
    fn core_protecting(data: &Path, extra: Vec<PathBuf>) -> (Arc<AppCore>, Arc<AiRecorder>) {
        let rec = Arc::new(AiRecorder::default());
        let parts = AiParts { vault: KeyVault::unavailable(), clock: Some(fixed_clock), protected: Vec::new() };
        let core = AppCore::new_with_ai(
            data.to_path_buf(),
            rec.clone(),
            ShellSpec::new("/bin/cat", &[]),
            parts.protecting(extra),
        );
        (core, rec)
    }

    #[test]
    fn a_tool_cannot_reach_the_agents_own_files_or_the_users_secrets() {
        let home = Scratch::new("wall-home");
        let sockets = Scratch::new("wall-sockets");
        let config = Scratch::new("wall-config");
        let scratch = Scratch::new("wall-data");
        let (core, rec) = core_protecting(&scratch.0, vec![config.0.clone(), sockets.0.clone()]);
        let data = scratch.0.canonicalize().expect("canonical");
        let secret = |dir: &Path, name: &str| put(dir, name, &format!("secret {SECRET_TEXT}\n"));
        let places = [
            secret(&data, "settings.json"),
            secret(&data, "ai.json"),
            secret(&data, "chats/c.json"),
            secret(&config.0, "app.json"),
            secret(&sockets.0, "ipc.sock"),
            secret(&home.0, ".ssh/id_rsa"),
            secret(&home.0, ".aws/credentials"),
            secret(&home.0, "proj/.env"),
        ];
        let notes = put(&home.0, "proj/notes.txt", "alpha beta\n");
        let spelled = home.0.canonicalize().expect("canonical").join("proj/../.ssh/id_rsa");
        let mut calls: Vec<(&str, Value)> = places.iter().map(|path| ("read_file", json!({"path": path}))).collect();
        calls.push(("read_file", json!({"path": spelled})));
        calls.push(("list_dir", json!({"path": data})));
        calls.push(("grep", json!({"pattern": "zq", "path": data})));
        assert_eq!(calls.len(), 11);

        let mut provider = scripted_tools("scripted");
        for (n, (name, args)) in calls.iter().enumerate() {
            provider = provider.with_turn(tool_call_turn(&format!("call_{n}"), name, args.clone()));
            provider = provider.with_turn(text_turn("noted"));
        }
        let control = tool_call_turn("call_c", "read_file", json!({"path": notes}));
        let provider = register(&core, "scripted", provider.with_turn(control).with_turn(text_turn("fine")));
        let chat = new_chat();
        let pane = pane_of(&core);

        for n in 0..calls.len() {
            say(&core, &rec, &chat, pane, &format!("read number {n}"));
        }
        assert!(events_of(&rec, &chat, "tool_proposed").is_empty(), "a protected path showed a card");
        let results = events_of(&rec, &chat, "tool_result");
        assert_eq!(results.len(), calls.len());
        let refusal = ToolError::Denied("secret_location").to_string();
        for (n, result) in results.iter().enumerate() {
            assert_eq!(result["is_error"], json!(true), "call {n}");
            assert_eq!(result["content"], json!(refusal), "call {n} does not give the fixed refusal");
        }
        assert!(ends_of(&rec, &chat).iter().all(|end| end == "done"));

        // A harmless file in the same tree still produces a card; the person denies it.
        start(&core, &chat, pane, "now the notes");
        let card = nth_event(&rec, &chat, "tool_proposed", 0);
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_c", "deny", &hash).expect("a denial");
        assert!(wait_for(Duration::from_secs(10), || rec.ended_turns(&chat) == calls.len() + 1));

        // The secret is in no event, no snapshot and no request.
        let snapshot = json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        let shown = everything_serialised(&rec, &[snapshot]);
        assert!(!shown.contains(SECRET_TEXT), "the secret reached an event or a view");
        let sent: String =
            provider.requests().iter().map(|r| serde_json::to_string(&r.messages).expect("serialises")).collect();
        assert!(!sent.contains(SECRET_TEXT), "the secret reached the model");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_changes_after_the_card_is_refused_when_the_call_runs() {
        let tree = Scratch::new("retarget-tree");
        let scratch = Scratch::new("retarget-data");
        let a = put(&tree.0, "proj/a.txt", &format!("{NOTES_A}\n"));
        let b = put(&tree.0, "proj/b.txt", &format!("{NOTES_B}\n"));
        let link = a.with_file_name("link.txt");
        std::os::unix::fs::symlink(&a, &link).expect("a link");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": link})))
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "read the link");
        let card = event_of(&rec, &chat, "tool_proposed");
        let resolved = card["pending"]["payload"]
            .as_array()
            .and_then(|fields| fields.iter().find(|f| f["name"] == "resolved_path"))
            .map(|f| f["value"].clone());
        assert_eq!(resolved, Some(json!(a)), "the card shows the file the link points to");
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();

        // The link now points to another file; the person's decision still carries the hash of the card.
        std::fs::remove_file(&link).expect("removed");
        std::os::unix::fs::symlink(&b, &link).expect("a new link");
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["is_error"], json!(true));
        assert_eq!(result["content"], json!(ToolError::PayloadChanged.to_string()));
        assert!(wait_for(Duration::from_secs(10), || rec.ended(&chat)));
        assert_eq!(ends_of(&rec, &chat), ["done"]);
        let sent = serde_json::to_string(&provider.requests()[1].messages).expect("serialises");
        assert!(sent.contains("The target changed"), "the model is told");
        let shown = everything_serialised(&rec, &[]);
        for canary in [NOTES_A, NOTES_B] {
            assert!(!shown.contains(canary) && !sent.contains(canary), "{canary} was read");
        }
    }

    #[test]
    fn a_read_taints_the_turn_and_attached_context_taints_it_from_the_first_card() {
        // (a) A read raises the taint of its turn: the card after it says so.
        let tree = Scratch::new("taint-tree");
        let scratch = Scratch::new("taint-data");
        let notes = tool_tree(&tree.0);
        let proj = notes.parent().expect("a folder").to_path_buf();
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(
                    Script::new()
                        .tool_call("call_1", "read_file", json!({"path": notes}))
                        .tool_call("call_2", "list_dir", json!({"path": proj}))
                        .stop(StopReason::ToolUse),
                )
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "read and list");
        let first = nth_event(&rec, &chat, "tool_proposed", 0);
        let marks = (first["pending"]["tainted"].as_bool(), first["pending"]["taint"].clone());
        assert_eq!(marks, (Some(false), json!([])));
        let hash = first["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "allow", &hash).expect("allowed");
        let second = nth_event(&rec, &chat, "tool_proposed", 1);
        assert_eq!(second["pending"]["tainted"], json!(true));
        assert_eq!(second["pending"]["taint"], json!(["output of read_file"]));
        let hash = second["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_2", "deny", &hash).expect("denied");
        assert!(wait_for(Duration::from_secs(10), || rec.ended(&chat)));

        // (b) Context the user attached taints the turn before its first card.
        let other = Scratch::new("taint-ctx");
        let (core, rec) = core_with_script(&other.0, "printf 'on the screen\\r\\n'; sleep 30");
        register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": notes})))
                .with_turn(text_turn("Done.")),
        );
        let pane = pane_of(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        let chat = new_chat();
        share(&core, &chat, pane);
        let view = preview_until(&core, &chat, pane, &[ContextKind::Screen], 10);
        core.ai_send_with_context(
            &chat,
            "explain this".to_owned(),
            pane,
            Some("scripted".to_owned()),
            None,
            view.offer.clone(),
            vec![1],
        )
        .expect("the send is accepted");
        let card = event_of(&rec, &chat, "tool_proposed");
        assert_eq!(card["pending"]["tainted"], json!(true));
        assert_eq!(card["pending"]["taint"], json!(["attached context"]));
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "deny", &hash).expect("denied");
        assert!(wait_for(Duration::from_secs(10), || rec.ended(&chat)));
    }

    #[test]
    fn a_cancel_a_closed_pane_or_a_deleted_chat_voids_the_waiting_card_and_answers_the_call() {
        let tree = Scratch::new("life-tree");
        let scratch = Scratch::new("life-data");
        let notes = tool_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let mut provider = scripted_tools("scripted");
        for n in 0..3 {
            provider = provider.with_turn(tool_call_turn(&format!("call_{n}"), "read_file", json!({"path": notes})));
        }
        register(&core, "scripted", provider);
        let first = pane_of(&core);
        let second = core.split_in(MAIN_WINDOW, false).expect("a split").focus;
        let (by_cancel, by_close, by_delete) = (new_chat(), new_chat(), new_chat());

        // Each chat waits at its card; the cause that ends the turn is different each time.
        let mut hashes = Vec::new();
        let waiting = [(&by_cancel, first), (&by_close, second), (&by_delete, first)];
        for (n, (chat, pane)) in waiting.into_iter().enumerate() {
            start(&core, chat, pane, "read the notes");
            let card = event_of(&rec, chat, "tool_proposed");
            assert_eq!(card["pending"]["call_id"], json!(format!("call_{n}")));
            hashes.push(card["pending"]["hash"].as_str().expect("a hash").to_owned());
        }
        assert_eq!(core.ai().cancel(&by_cancel), Ok(true));
        core.close_pane_in(MAIN_WINDOW, second).expect("the pane closes");
        assert_eq!(core.ai().conversation_delete(&by_delete), Ok(true));

        for (n, chat) in [&by_cancel, &by_close, &by_delete].into_iter().enumerate() {
            assert!(wait_for(Duration::from_secs(1), || rec.ended(chat)), "the turn of chat {n} ends within a second");
            assert_eq!(ends_of(&rec, chat), ["cancelled"], "chat {n}");
            let result = event_of(&rec, chat, "tool_result");
            assert_eq!(result["call_id"], json!(format!("call_{n}")));
            assert_eq!((result["is_error"].clone(), result["content"].clone()), (json!(true), json!(STOPPED_RESULT)));
            assert!(core.ai().service().approvals().pending(chat).is_empty(), "a card outlived chat {n}");
            let late = decision(&core, chat, &format!("call_{n}"), "allow", &hashes[n]).expect_err("a late decision");
            assert_eq!((late.code, late.reason), ("tool", Some("unknown_call")), "chat {n}");
        }
        assert!(!scratch.chats().join(format!("{by_delete}.json")).exists(), "a deleted chat was written back");
    }

    #[test]
    fn a_shared_live_pane_is_read_by_a_tool_and_an_unshared_one_is_refused() {
        let scratch = Scratch::new("live-pane");
        let hidden = format!("printf '\\033[8m{SHELL_HIDDEN}\\033[0m after\\r\\n'");
        let script = format!("printf '{SHELL_TEXT}\\r\\n'; {hidden}; sleep 30");
        let (core, rec) = core_with_script(&scratch.0, &script);
        let pane = pane_of(&core);
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");
        let args = json!({"pane": pane, "rows": 20});
        let mut provider = scripted_tools("scripted");
        for n in 1..=3 {
            provider = provider.with_turn(tool_call_turn(&format!("call_{n}"), "read_pane", args.clone()));
            provider = provider.with_turn(text_turn("noted"));
        }
        register(&core, "scripted", provider);
        let (asked, chat) = (new_chat(), new_chat());
        let refusal = ToolError::NotShared.to_string();

        // Not shared: refused when the call is proposed, no card.
        say(&core, &rec, &asked, pane, "what is on the screen?");
        let first = nth_event(&rec, &asked, "tool_result", 0);
        assert_eq!((first["is_error"].clone(), first["content"].clone()), (json!(true), json!(refusal)));
        assert!(events_of(&rec, &asked, "tool_proposed").is_empty(), "an unshared pane showed a card");

        // Shared in another chat, and the screen shows the output: a card, a decision, the visible text and not the
        // concealed word. A chat that has a tool result in its history taints its later turns, so this one is new.
        share(&core, &chat, pane);
        preview_until(&core, &chat, pane, &[ContextKind::Screen], 20);
        start(&core, &chat, pane, "and now?");
        let card = event_of(&rec, &chat, "tool_proposed");
        assert_eq!(payload_names(&card["pending"]), ["tool", "pane", "target", "remote", "rows"]);
        assert_eq!(card["pending"]["tainted"], json!(false), "nothing was attached to this turn");
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_2", "allow", &hash).expect("allowed");
        let second = nth_event(&rec, &chat, "tool_result", 0);
        let content = second["content"].as_str().expect("content");
        assert!(content.contains(SHELL_TEXT) && content.contains(" after"), "{content}");
        assert!(!content.contains(SHELL_HIDDEN), "the concealed word reached the model");
        assert!(wait_for(Duration::from_secs(10), || rec.ended_turns(&chat) == 1));

        // The share is withdrawn: refused again.
        core.ai_context_share(&chat, pane, Some("scripted".to_owned()), None, false).expect("withdrawn");
        say(&core, &rec, &chat, pane, "once more");
        let third = nth_event(&rec, &chat, "tool_result", 1);
        assert_eq!((third["is_error"].clone(), third["content"].clone()), (json!(true), json!(refusal)));
        assert_eq!(events_of(&rec, &chat, "tool_proposed").len(), 1);
    }

    // ---- the recorded tool run --------------------------------------------------------------------------------

    /// What stands for the scratch folder in the recorded tool run.
    const GOLDEN_ROOT: &str = "/scratch";

    /// `value` with every string that is one of `hashes` replaced by its number as 64 hex digits, and the scratch
    /// folder (in both spellings, the longer one first) replaced by [`GOLDEN_ROOT`] wherever it occurs in a string.
    fn normalise_run(value: &mut Value, hashes: &[String], roots: &[String]) {
        match value {
            Value::String(text) => {
                if let Some(at) = hashes.iter().position(|hash| hash == text) {
                    *text = format!("{:064x}", at + 1);
                    return;
                }
                for root in roots {
                    if text.contains(root.as_str()) {
                        *text = text.replace(root.as_str(), GOLDEN_ROOT);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| normalise_run(item, hashes, roots)),
            Value::Object(members) => {
                let moved = members.get("value").and_then(Value::as_str).is_some_and(|text| {
                    roots.iter().any(|root| text.contains(root.as_str()))
                });
                members.values_mut().for_each(|member| normalise_run(member, hashes, roots));
                // A card field says how long its raw value is, and the length of a path is the machine's.
                if moved && let Some(bytes) = members.get("value").and_then(Value::as_str).map(str::len) {
                    members.insert("bytes".to_owned(), json!(bytes));
                }
            }
            _ => {}
        }
    }

    /// Plays three tool turns on the scripted provider (a read the person allows, a read the person denies, and a read
    /// of a file the deny list protects) and records every `ai` event, the state after each, and the snapshots taken
    /// live: one while a card waits and one after each turn. Nothing reads a clock and nothing sleeps for a fixed time.
    fn record_tool_run() -> Value {
        let tree = Scratch::new("tool-golden-tree");
        let scratch = Scratch::new("tool-golden");
        let notes = put(&tree.0, "proj/notes.txt", "alpha beta \n");
        let env_file = put(&tree.0, "proj/.env", "TOKEN=none\n");
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "read_file", json!({"path": notes})))
                .with_turn(text_turn("The file holds a short note."))
                .with_turn(tool_call_turn("call_2", "read_file", json!({"path": notes})))
                .with_turn(text_turn("Understood, I will not read it."))
                .with_turn(tool_call_turn("call_3", "read_file", json!({"path": env_file})))
                .with_turn(text_turn("That place is protected.")),
        );
        let chat = core.ai().conversation_new().expect("an id");
        let pane = pane_of(&core);
        let snapshot = || json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        let last_index = || rec.ai_events().len() - 1;
        let wait = Duration::from_secs(10);
        let mut snapshots: Vec<(usize, Value)> = Vec::new();
        let mut hashes: Vec<String> = Vec::new();

        start(&core, &chat, pane, "What does the note say?");
        let card = event_of(&rec, &chat, "tool_proposed");
        let proposed_at = rec
            .ai_events()
            .iter()
            .position(|(_, payload)| payload.0.kind.name() == "tool_proposed")
            .expect("the card is in the log");
        assert_eq!(proposed_at, last_index(), "the turn waits at its card");
        snapshots.push((proposed_at, snapshot()));
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        hashes.push(hash.clone());
        decision(&core, &chat, "call_1", "allow", &hash).expect("allowed");
        assert!(wait_for(wait, || rec.ended_turns(&chat) == 1));
        snapshots.push((last_index(), snapshot()));

        start(&core, &chat, pane, "Read it again.");
        let card = nth_event(&rec, &chat, "tool_proposed", 1);
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        hashes.push(hash.clone());
        decision(&core, &chat, "call_2", "deny", &hash).expect("denied");
        assert!(wait_for(wait, || rec.ended_turns(&chat) == 2));
        snapshots.push((last_index(), snapshot()));

        say(&core, &rec, &chat, pane, "Now read the environment file.");
        snapshots.push((last_index(), snapshot()));

        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut state = ChatState::new(&chat);
        let mut states: Vec<Value> = Vec::new();
        for payload in &events {
            assert_eq!(state.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
            states.push(json_of(&state));
        }

        // Three turns, each counting from 1 without a hole; a card is open exactly between its proposal and its result.
        let mut turns: Vec<u64> = events.iter().map(|p| p.0.turn).collect();
        turns.dedup();
        assert_eq!(turns.len(), 3, "three different turn numbers");
        let of_turn = |turn: u64| events.iter().filter(move |p| p.0.turn == turn);
        for turn in &turns {
            let seqs: Vec<u64> = of_turn(*turn).map(|p| p.0.seq).collect();
            assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>(), "turn {turn}");
        }
        let kinds = |turn: u64| of_turn(turn).map(|p| p.0.kind.name()).collect::<Vec<_>>();
        for turn in &turns[..2] {
            let kinds = kinds(*turn);
            let at = |kind: &str| kinds.iter().position(|k| *k == kind);
            let (proposed, result) = (at("tool_proposed"), at("tool_result"));
            assert!(proposed < result && proposed.is_some(), "a card comes before its result: {kinds:?}");
        }
        assert!(!kinds(turns[2]).contains(&"tool_proposed"), "a protected path showed a card");
        assert!(kinds(turns[2]).contains(&"tool_result"));
        let mut open = 0;
        for (event, state) in events.iter().zip(&states) {
            match event.0.kind.name() {
                "tool_proposed" => open += 1,
                "tool_result" if open > 0 => open -= 1,
                _ => {}
            }
            let pending = state["pending"].as_array().map_or(0, Vec::len);
            let kind = event.0.kind.name();
            assert_eq!(pending, if kind == "turn_ended" { 0 } else { open }, "{kind}");
        }
        let results: Vec<Value> = events
            .iter()
            .filter(|p| p.0.kind.name() == "tool_result")
            .map(|p| json_of(&*p.0))
            .collect();
        let flags: Vec<Value> = results.iter().map(|r| r["is_error"].clone()).collect();
        assert_eq!(flags, [json!(false), json!(true), json!(true)]);
        assert!(results[0]["content"].as_str().is_some_and(|c| c.starts_with("alpha beta")));
        assert_eq!(results[1]["content"], json!("The user denied this action."));
        assert_eq!(ends_of(&rec, &chat), ["done", "done", "done"]);

        // The snapshot while a card waits is the fold of the events at that point. A snapshot after a turn is read from
        // the store, which keeps no tool output as text: it equals the fold with the output replaced by its marker.
        assert_eq!(snapshots[0].1, states[snapshots[0].0], "the snapshot at the card");
        assert_eq!(snapshots[0].1["running"], json!(true));
        assert_eq!(snapshots[0].1["pending"].as_array().map(Vec::len), Some(1));
        for (after, live) in &snapshots[1..] {
            assert_eq!(live, &as_stored(states[*after].clone()), "the snapshot after event {after}");
        }

        let mut roots = vec![tree.0.canonicalize().expect("canonical").to_string_lossy().into_owned()];
        roots.push(tree.0.to_string_lossy().into_owned());
        let mut document = json!({
            "conversation": chat,
            "events": events.iter().map(|payload| json_of(&*payload.0)).collect::<Vec<_>>(),
            "states": states,
            "snapshots": snapshots.iter().map(|(at, live)| json!({"after": at, "state": live})).collect::<Vec<_>>(),
        });
        anonymise(&mut document, &chat);
        normalise_run(&mut document, &hashes, &roots);
        document
    }

    fn tool_golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/ai_tool_stream.json")
    }

    #[test]
    fn the_scripted_tool_run_is_stable_and_matches_the_ai_tool_stream_golden() {
        let first = record_tool_run();
        let second = record_tool_run();
        assert_eq!(first, second, "two runs of the same script record the same stream");

        // The page gets views: no provider-native turn, no stored context, no key, no machine path and no canary.
        let mut names = Vec::new();
        member_names(&first, &mut names);
        for forbidden in ["native", "stored", "api_key", "key"] {
            assert!(!names.iter().any(|name| name == forbidden), "the stream has a member named {forbidden}");
        }
        let text = first.to_string();
        for canary in [TEXT_CANARY, KEY_CANARY, TOOL_TEXT, SECRET_TEXT, NOTES_A, NOTES_B] {
            assert!(!text.contains(canary), "{canary} is in the recorded run");
        }
        let temp = std::env::temp_dir();
        let temp = temp.to_string_lossy();
        let machine = text.contains(temp.as_ref()) || text.contains("holzwave-ai-");
        assert!(!machine, "a machine path is in the recorded run");
        assert!(text.contains("/scratch/proj/notes.txt"));

        let mut expected = serde_json::to_string_pretty(&first).expect("json");
        expected.push('\n');
        let path = tool_golden_path();
        if update_fixtures() {
            std::fs::write(&path, &expected).expect("write ai_tool_stream.json");
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing ({e}); run with UPDATE_FIXTURES=1", path.display()));
        assert!(
            on_disk == expected,
            "the recorded run differs from ai_tool_stream.json; review it and run with UPDATE_FIXTURES=1"
        );
    }

    // ---- write tools ------------------------------------------------------------------------------------------

    /// What the file a write replaces holds, and what the model writes in its place; they appear in test code only.
    const OLD_TEXT: &str = "zq-old-text-4d2e91";
    const NEW_TEXT: &str = "zq-new-text-b83f17";

    /// The old text of `proj/todo.txt`: three lines.
    fn old_todo() -> String {
        format!("alpha\nbeta {OLD_TEXT}\ngamma\n")
    }

    /// The new text the model writes over it.
    fn new_todo() -> String {
        format!("alpha\nbeta {NEW_TEXT}\ngamma\n")
    }

    /// `<dir>/proj/todo.txt` with its three lines: the canonical path of the file.
    fn todo_tree(dir: &Path) -> PathBuf {
        let proj = dir.join("proj");
        std::fs::create_dir_all(&proj).expect("a folder");
        let todo = proj.join("todo.txt");
        std::fs::write(&todo, old_todo()).expect("a file");
        todo.canonicalize().expect("a canonical path")
    }

    /// The text of every file below `dir`, joined: what a store keeps.
    fn stored_text(dir: &Path) -> String {
        let mut out = String::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(folder) = stack.pop() {
            for item in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let path = item.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                }
            }
        }
        out
    }

    #[test]
    fn a_write_call_shows_its_diff_is_decided_on_its_hash_and_saves_the_file() {
        let tree = Scratch::new("write-tree");
        let scratch = Scratch::new("write-trace");
        let todo = todo_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "write_file", json!({"path": todo, "content": new_todo()})))
                .with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);

        core.ai_send(&chat, "update my todo list".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");

        let proposed = event_of(&rec, &chat, "tool_proposed");
        let card = &proposed["pending"];
        assert_eq!((card["tool"].as_str(), card["effect"].as_str()), (Some("write_file"), Some("write")));
        assert_eq!(card["target"], json!("This computer"));
        assert_eq!(
            payload_names(card),
            ["tool", "pane", "target", "remote", "path", "content", "resolved_path", "stamp", "base"]
        );
        let field = |name: &str| {
            let fields = card["payload"].as_array().expect("a payload");
            fields.iter().find(|f| f["name"] == name).map(|f| f["value"].as_str().expect("a value").to_owned())
        };
        assert_eq!(field("resolved_path"), todo.to_str().map(str::to_owned));
        assert!(field("base").is_some_and(|base| base.starts_with("sha256:") && base.len() == 71), "{:?}", field("base"));
        let diff = &card["diff"];
        assert_eq!(diff["kind"], json!("replace"));
        assert_eq!(
            diff["rows"],
            json!([
                {"op": "ctx", "old": 1, "new": 1, "text": "alpha"},
                {"op": "del", "old": 2, "new": null, "text": format!("beta {OLD_TEXT}")},
                {"op": "add", "old": null, "new": 2, "text": format!("beta {NEW_TEXT}")},
                {"op": "ctx", "old": 3, "new": 3, "text": "gamma"},
            ])
        );
        let hash = card["hash"].as_str().expect("a hash").to_owned();
        assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{hash}");
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), old_todo(), "nothing is written before the decision");

        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(
            (result["tool"].as_str(), result["is_error"].as_bool()),
            (Some("write_file"), Some(false)),
            "{result}"
        );
        let content = result["content"].as_str().expect("content");
        assert!(content.starts_with("Saved the file."), "{content}");
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), new_todo(), "the file holds the new text");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));

        let requests = provider.requests();
        assert_eq!(requests.len(), 2, "the second request carries the result");
        let last = requests[1].messages.last().expect("a message");
        assert_eq!(last.role, Role::Tool);
        let returned = last.parts.iter().any(|part| match part {
            Part::ToolResult(result) => result.call_id == "call_1" && result.content.starts_with("Saved the file."),
            _ => false,
        });
        assert!(returned, "the result of the call is the outcome");
        let sent = serde_json::to_string(&requests.iter().map(|r| &r.messages).collect::<Vec<_>>()).expect("serialises");
        assert!(!sent.contains(OLD_TEXT), "the old text of the file reached the provider");
        assert!(!stored_text(&scratch.0).contains(OLD_TEXT), "the old text of the file reached a stored chat");

        let events: Vec<AiPayload> = rec.ai_events().into_iter().map(|(_, payload)| payload).collect();
        let mut folded = ChatState::new(&chat);
        for payload in &events {
            assert_eq!(folded.apply(&payload.0), ApplyResult::Applied, "{payload:?}");
        }
        let idle = core.ai().snapshot(&chat).expect("a snapshot");
        assert_eq!(as_stored(json_of(&folded)), json_of(&idle), "the fold of the events is the snapshot");
    }

    #[test]
    fn a_denied_write_leaves_the_file_alone_and_the_model_is_not_told_the_old_text() {
        let tree = Scratch::new("write-tree");
        let scratch = Scratch::new("write-deny");
        let todo = todo_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "write_file", json!({"path": todo, "content": new_todo()})))
                .with_turn(text_turn("Understood.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);
        core.ai_send(&chat, "update my todo list".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        let proposed = event_of(&rec, &chat, "tool_proposed");
        let hash = proposed["pending"]["hash"].as_str().expect("a hash").to_owned();

        decision(&core, &chat, "call_1", "deny", &hash).expect("a denial");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["is_error"], json!(true));
        assert_eq!(result["content"], json!("The user denied this action."));
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), old_todo(), "the file is as it was");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)), "the turn ends");
        assert_eq!(rec.end_of(&chat).expect("an end")["end"], json!("done"));
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        let sent = serde_json::to_string(&requests.iter().map(|r| &r.messages).collect::<Vec<_>>()).expect("serialises");
        assert!(sent.contains("The user denied this action."), "the model is told");
        assert!(!sent.contains(OLD_TEXT), "the old text of the file reached the model after a denial");
        assert!(!stored_text(&scratch.0).contains(OLD_TEXT), "the old text reached a stored chat");
    }

    /// A chat whose model asked for `args` of `tool` over `todo` and is waiting for a decision: the core, its recorder, the
    /// chat, the card's hash and the provider.
    fn waiting_call(
        scratch: &Path,
        tool: &str,
        args: Value,
    ) -> (Arc<AppCore>, Arc<AiRecorder>, String, String, Arc<ScriptedProvider>) {
        let (core, rec) = core_with(scratch, KeyVault::unavailable());
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted").with_turn(tool_call_turn("call_1", tool, args)).with_turn(text_turn("Done.")),
        );
        let chat = new_chat();
        start(&core, &chat, pane_of(&core), "change my file");
        let card = event_of(&rec, &chat, "tool_proposed");
        let hash = card["pending"]["hash"].as_str().expect("a hash").to_owned();
        (core, rec, chat, hash, provider)
    }

    #[test]
    fn an_edit_call_changes_one_match_and_the_diff_shows_it() {
        let tree = Scratch::new("edit-tree");
        let scratch = Scratch::new("edit-trace");
        let todo = todo_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        let missing = json!({"path": todo, "old_text": "no such line", "new_text": "x"});
        let edit = json!({"path": todo, "old_text": format!("beta {OLD_TEXT}"), "new_text": format!("beta {NEW_TEXT}")});
        let provider = register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "edit_file", edit))
                .with_turn(text_turn("Done."))
                .with_turn(tool_call_turn("call_2", "edit_file", missing))
                .with_turn(text_turn("It was not there.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);

        start(&core, &chat, pane, "change my todo list");
        let proposed = event_of(&rec, &chat, "tool_proposed");
        let card = &proposed["pending"];
        assert_eq!((card["tool"].as_str(), card["effect"].as_str()), (Some("edit_file"), Some("write")));
        assert_eq!(card["diff"]["kind"], json!("edit"));
        assert_eq!(
            payload_names(card),
            ["tool", "pane", "target", "remote", "path", "old_text", "new_text", "replace_all", "resolved_path", "stamp", "base"]
        );
        let hash = card["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!(result["is_error"], json!(false), "{result}");
        assert!(result["content"].as_str().is_some_and(|text| text.ends_with("1 line added, 1 removed.")));
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), new_todo(), "one line is replaced, the rest is kept");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)));

        // An edit whose text is not in the file has no card; the model is told with the fixed phrase and goes on.
        start(&core, &chat, pane, "and once more");
        let second = nth_event(&rec, &chat, "tool_result", 1);
        assert_eq!(second["call_id"], json!("call_2"));
        assert_eq!(second["is_error"], json!(true));
        let text = second["content"].as_str().expect("content");
        assert!(text.contains("old_text was not found in the file"), "{text}");
        assert!(!text.contains("no such line") && !text.contains(todo.to_str().expect("utf-8")), "{text}");
        assert!(wait_for(Duration::from_secs(5), || rec.ended_turns(&chat) == 2));
        assert_eq!(events_of(&rec, &chat, "tool_proposed").len(), 1, "the second call never had a card");
        assert_eq!(ends_of(&rec, &chat), ["done", "done"]);
        assert_eq!(provider.calls(), 4);
    }

    #[test]
    fn a_file_changed_after_the_card_is_refused_when_the_call_runs() {
        let changed = ToolError::PayloadChanged.to_string();
        let other = old_todo().replace("old", "oth");
        assert_eq!(other.len(), old_todo().len());

        // (a) Other text of the same length and the same change time.
        let tree = Scratch::new("stale-tree");
        let scratch = Scratch::new("stale-trace");
        let todo = todo_tree(&tree.0);
        let before = std::fs::metadata(&todo).expect("metadata").modified().expect("a time");
        let (core, rec, chat, hash, provider) =
            waiting_call(&scratch.0, "write_file", json!({"path": todo, "content": new_todo()}));
        std::fs::write(&todo, &other).expect("the other program writes");
        std::fs::File::options().write(true).open(&todo).expect("open").set_modified(before).expect("the time is set back");
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!((result["is_error"].clone(), result["content"].clone()), (json!(true), json!(changed)));
        assert_eq!(std::fs::read(&todo).expect("the file"), other.as_bytes(), "the file is as the other program left it");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)));
        let sent = serde_json::to_string(&provider.requests().iter().map(|r| &r.messages).collect::<Vec<_>>()).expect("serialises");
        assert!(!sent.contains(OLD_TEXT), "the old text reached the model");

        // (b) The file is removed while the card waits.
        let tree = Scratch::new("stale-tree");
        let scratch = Scratch::new("stale-trace");
        let todo = todo_tree(&tree.0);
        let (core, rec, chat, hash, _) = waiting_call(&scratch.0, "write_file", json!({"path": todo, "content": new_todo()}));
        std::fs::remove_file(&todo).expect("removed");
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!((result["is_error"].clone(), result["content"].clone()), (json!(true), json!(changed)));
        assert!(!todo.exists(), "nothing was written");

        // (c) A card for a new file, and a file appears where it would go.
        let tree = Scratch::new("stale-tree");
        let scratch = Scratch::new("stale-trace");
        let proj = tree.0.join("proj");
        std::fs::create_dir_all(&proj).expect("a folder");
        let fresh = proj.canonicalize().expect("canonical").join("fresh.txt");
        let (core, rec, chat, hash, _) = waiting_call(&scratch.0, "write_file", json!({"path": fresh, "content": new_todo()}));
        std::fs::write(&fresh, "somebody else\n").expect("the other program writes");
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        let result = event_of(&rec, &chat, "tool_result");
        assert_eq!((result["is_error"].clone(), result["content"].clone()), (json!(true), json!(changed)));
        assert_eq!(std::fs::read_to_string(&fresh).expect("the file"), "somebody else\n");
    }

    #[test]
    fn a_write_to_protected_places_is_refused_before_any_card() {
        let home = Scratch::new("wwall-home");
        let scratch = Scratch::new("wwall-data");
        let (core, rec) = core_protecting(&scratch.0, Vec::new());
        let data = scratch.0.canonicalize().expect("canonical");
        let root = home.0.canonicalize().expect("canonical");
        let ssh = put(&root, "home/.ssh/config", &format!("secret {SECRET_TEXT}\n"));
        let zshrc = put(&root, "home/.zshrc", "export A=1\n");
        let env = put(&root, "proj/.env", "A=1\n");
        std::fs::create_dir_all(root.join("proj/.git/hooks")).expect("a folder");
        let hook = root.join("proj/.git/hooks/pre-commit");
        let mut places = vec![data.join("settings.json"), zshrc.clone(), ssh.clone(), env.clone(), hook.clone()];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("home/.ssh"), root.join("proj/link")).expect("a link");
            places.push(root.join("proj/link/new.txt"));
        }

        let mut provider = scripted_tools("scripted");
        for (n, path) in places.iter().enumerate() {
            provider = provider.with_turn(tool_call_turn(&format!("call_{n}"), "write_file", json!({"path": path, "content": "pwned\n"})));
            provider = provider.with_turn(text_turn("noted"));
        }
        let provider = register(&core, "scripted", provider);
        let chat = new_chat();
        let pane = pane_of(&core);
        for n in 0..places.len() {
            say(&core, &rec, &chat, pane, &format!("write number {n}"));
        }
        assert!(events_of(&rec, &chat, "tool_proposed").is_empty(), "a protected place showed a card");
        let results = events_of(&rec, &chat, "tool_result");
        assert_eq!(results.len(), places.len());
        let refusal = ToolError::Denied("secret_location").to_string();
        for (n, result) in results.iter().enumerate() {
            assert_eq!(result["is_error"], json!(true), "call {n}");
            assert_eq!(result["content"], json!(refusal), "call {n} does not give the fixed refusal");
        }
        assert_eq!(std::fs::read_to_string(&zshrc).expect("the file"), "export A=1\n");
        assert_eq!(std::fs::read_to_string(&env).expect("the file"), "A=1\n");
        assert!(!hook.exists() && !data.join("settings.json").exists() && !root.join("home/.ssh/new.txt").exists());
        assert!(ends_of(&rec, &chat).iter().all(|end| end == "done"));

        let snapshot = json_of(&core.ai().snapshot(&chat).expect("a snapshot"));
        let shown = everything_serialised(&rec, &[snapshot]);
        assert!(!shown.contains(SECRET_TEXT), "the secret reached an event or a view");
        let sent: String =
            provider.requests().iter().map(|r| serde_json::to_string(&r.messages).expect("serialises")).collect();
        assert!(!sent.contains(SECRET_TEXT), "the secret reached the model");
    }

    #[test]
    fn an_undo_through_the_whole_shell_restores_the_file_and_a_second_press_is_unknown() {
        let tree = Scratch::new("undo-tree");
        let scratch = Scratch::new("undo-trace");
        let todo = todo_tree(&tree.0);
        let (core, rec) = core_with(&scratch.0, KeyVault::unavailable());
        register(
            &core,
            "scripted",
            scripted_tools("scripted")
                .with_turn(tool_call_turn("call_1", "write_file", json!({"path": todo, "content": new_todo()})))
                .with_turn(text_turn("Done."))
                .with_turn(tool_call_turn("call_2", "write_file", json!({"path": todo, "content": new_todo()})))
                .with_turn(text_turn("Done again.")),
        );
        let chat = new_chat();
        let pane = pane_of(&core);

        start(&core, &chat, pane, "update my todo list");
        let hash = event_of(&rec, &chat, "tool_proposed")["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_1", "allow", &hash).expect("the decision is accepted");
        assert!(wait_for(Duration::from_secs(5), || rec.ended(&chat)));
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), new_todo());

        // The press puts the old text back exactly; a second press is a change nobody knows any more.
        core.ai_undo(&chat, "call_1").expect("undone");
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), old_todo(), "the old text is back");
        let again = core.ai_undo(&chat, "call_1").expect_err("used once");
        assert_eq!((again.code, again.reason), ("undo", Some("unknown_change")));
        assert_eq!(again.text, "That change can no longer be undone here.");
        let bad = core.ai_undo("../x", "call_1").expect_err("a bad chat id");
        assert_eq!(bad.code, "invalid_conversation");
        assert!(core.ai_undo(&chat, &"x".repeat(129)).is_err());

        // A second write, and then another program changes the file: nothing is restored and the file is left as it was.
        start(&core, &chat, pane, "again");
        let hash = nth_event(&rec, &chat, "tool_proposed", 1)["pending"]["hash"].as_str().expect("a hash").to_owned();
        decision(&core, &chat, "call_2", "allow", &hash).expect("the decision is accepted");
        assert!(wait_for(Duration::from_secs(5), || rec.ended_turns(&chat) == 2));
        let by_hand = format!("{}edited by hand\n", new_todo());
        std::fs::write(&todo, &by_hand).expect("another program writes");
        let refused = core.ai_undo(&chat, "call_2").expect_err("the file changed");
        assert_eq!((refused.code, refused.reason), ("undo", Some("file_changed")));
        assert_eq!(std::fs::read_to_string(&todo).expect("the file"), by_hand, "the file is left as it was");

        // Deleting the chat takes what it could still undo.
        core.ai().conversation_delete(&chat).expect("deleted");
        let gone = core.ai_undo(&chat, "call_2").expect_err("forgotten");
        assert_eq!((gone.code, gone.reason), ("undo", Some("unknown_change")));
    }
}

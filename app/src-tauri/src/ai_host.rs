//! The shell's side of the AI service: the sink that turns service events into one webview event, the types of the
//! answers the commands give, the faults, and the calls the commands make.
//!
//! The service itself lives in the core and owns every thread: a send returns at once, a turn streams on a thread of
//! its own, and its events reach the page through [`ShellAiSink`] as the event `ai`, for every window (the page routes
//! by conversation, so a reload or a window that was torn off keeps working). This file holds no tool host, no context
//! source and no decision of any kind; a key is written to the service and never read back.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use holzwave_core::ai::profiles::{KeyError, KeySlot, KeyStatus, KeyVault, Preset, ProfileInput, ProfileView, Removal};
use holzwave_core::ai::provider::Provider;
use holzwave_core::ai::service::events::{AiEvent, ChatState};
use holzwave_core::ai::service::{AiService, AiSink, SendRequest, ServiceError, TurnTicket};
use holzwave_core::ai::store::{ConversationId, ConversationSummary, JsonDirStore, StoreError};
use holzwave_core::ai::tools::DenyPolicy;
use holzwave_core::ai::types::ApiKey;
use holzwave_core::ai::wire::Wire;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::KeychainStore;
use holzwave_core::settings::Settings;
use serde::{Deserialize, Serialize, Serializer};

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
}

impl AiParts {
    /// The platform keychain and the system clock.
    pub fn system() -> AiParts {
        AiParts { vault: KeyVault::new(Arc::new(KeychainStore)), clock: None }
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

    /// A profile id that no profile has.
    pub fn unknown_profile() -> AiFault {
        AiFault { code: "profile", reason: Some("unknown_profile"), text: "there is no such profile".to_owned() }
    }

    /// The service's refusal, with the name of its reason. The matches have no wildcard arm, so a variant that the
    /// core adds later does not compile here until it has a name.
    pub fn from_service(error: &ServiceError) -> AiFault {
        let reason = match error {
            ServiceError::Profile(profile) => Some(profile.reason()),
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
}

impl AiHost {
    /// Builds the service over the chats folder and `ai.json` of `dir`. The data directory is the first place the
    /// agent may never touch (the tools that follow add the others). Nothing else happens here: no thread, no socket,
    /// no keychain call.
    pub fn new(dir: &Path, sink: Arc<dyn Sink>, parts: AiParts) -> AiHost {
        let mut builder =
            AiService::builder(Arc::new(JsonDirStore::new(dir.join(CHATS_DIR))), Arc::new(ShellAiSink { sink }), dir)
                .vault(parts.vault)
                .deny_policy(DenyPolicy::new(vec![dir.to_path_buf()]));
        if let Some(clock) = parts.clock {
            builder = builder.clock(clock);
        }
        AiHost { service: builder.build() }
    }

    /// The service, for the plans that add a gate, a tool host or a decision to the shell.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn service(&self) -> &AiService {
        &self.service
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
        let mut request = SendRequest::new(parse_conversation(conversation)?, text, pane);
        if let Some(profile) = given(profile) {
            request = request.profile(profile);
        }
        if let Some(model) = given(model) {
            request = request.model(model);
        }
        self.service.send(request).map(TicketView::from).map_err(|e| AiFault::from_service(&e))
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
        self.service.delete_conversation(&id).map_err(|e| AiFault::from_service(&e))
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
        let left = self.service.shutdown(SHUTDOWN_BOUND);
        if left > 0 {
            eprintln!("holzwave: {left} assistant turn(s) did not end within the shutdown bound");
        }
    }
}

/// The id of a chat, parsed at the boundary: nothing else is ever used as a file name.
fn parse_conversation(text: &str) -> Result<ConversationId, AiFault> {
    ConversationId::parse(text).map_err(|_| AiFault::invalid_conversation())
}

/// `value` unless it is absent or only white space.
fn given(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.trim().is_empty())
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
        if !self.pane_is_open(pane) {
            return Err(AiFault::no_such_pane());
        }
        self.ai().send(conversation, text, pane, profile, model)
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
            AiParts { vault, clock: Some(fixed_clock) },
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
}

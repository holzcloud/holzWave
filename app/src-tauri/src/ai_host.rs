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

use holzwave_core::ai::profiles::{KeyError, KeyVault, ProfileError};
use holzwave_core::ai::provider::Provider;
use holzwave_core::ai::service::events::{AiEvent, ChatState};
use holzwave_core::ai::service::{AiService, AiSink, SendRequest, ServiceError, TurnTicket};
use holzwave_core::ai::store::{ConversationId, JsonDirStore, StoreError};
use holzwave_core::ai::tools::DenyPolicy;
use holzwave_core::layout::PaneId;
use holzwave_core::secrets::KeychainStore;
use serde::{Serialize, Serializer};

use crate::state::{AppCore, Out, Sink, Target};

/// The folder of the data directory that holds one file per chat.
pub const CHATS_DIR: &str = "chats";

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

    use holzwave_core::ai::scripted::{Script, ScriptedProvider};
    use holzwave_core::ai::service::events::ApplyResult;
    use holzwave_core::ai::types::{Capabilities, Locality, Part};
    use holzwave_core::layout::MAIN_WINDOW;
    use serde_json::Value;

    use crate::state::ShellSpec;
    use crate::windows::{CursorSpot, Place};

    /// The text a user types in these tests; it appears in test code only, so a hit anywhere else is a leak.
    const TEXT_CANARY: &str = "zq-host-text-8d3b27f1c4";

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
            self.ai_events().iter().any(|(_, p)| p.0.conversation == chat && p.0.kind.name() == "turn_ended")
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
}

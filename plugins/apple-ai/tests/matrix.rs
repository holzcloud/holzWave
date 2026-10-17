//! The on-device model through a real service turn, held to the same rules as every other provider: it runs only while
//! its backend says `available`, never falls back to another provider, maps every backend error to its class after one
//! call, refuses a foreign context, a tool and a foreign model before the backend, and treats an earlier context note
//! with the same facts and the same wording as the network dialects.
//!
//! Availability is injected through the scripted backend, so the file runs on any host with no Zig, no Xcode and no
//! network.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_apple_ai::{AppleError, AppleProvider, Availability, BackendScript, MODEL_ID, PROVIDER_ID, ScriptedBackend};
use holzwave_core::ai::provider::{AiError, CancelToken, ChatRequest, Provider};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::{AiEvent, AiEventKind, AiService, AiSink, ChatPart, EndView, Route, SendRequest};
use holzwave_core::ai::store::{ConversationStore, MemoryStore};
use holzwave_core::ai::types::{Consent, ContextKind, ContextNote, Locality, Message, NoteItem, Part, Role, ToolSpec};
use holzwave_core::ai::wire::marker_line;

/// The secret text a note may have kept.
const STORED: &str = "CANARY-STORED-TEXT-5b2d08";

// ---- the states -------------------------------------------------------------------------------------------

/// What a state means for the provider. The match is exhaustive, so a ninth state does not compile until it is decided.
fn runs_a_turn(state: Availability) -> bool {
    match state {
        Availability::Available => true,
        Availability::AppleIntelligenceNotEnabled
        | Availability::DeviceNotEligible
        | Availability::ModelNotReady
        | Availability::Other
        | Availability::OsTooOld
        | Availability::SdkLacksFramework
        | Availability::UnsupportedPlatform => false,
    }
}

fn states() -> [Availability; 8] {
    let all = [
        Availability::Available,
        Availability::AppleIntelligenceNotEnabled,
        Availability::DeviceNotEligible,
        Availability::ModelNotReady,
        Availability::Other,
        Availability::OsTooOld,
        Availability::SdkLacksFramework,
        Availability::UnsupportedPlatform,
    ];
    let keys: std::collections::BTreeSet<&str> = all.iter().map(|state| state.as_str()).collect();
    assert_eq!(keys.len(), 8, "the eight states have eight distinct keys");
    assert_eq!(all.iter().filter(|state| runs_a_turn(**state)).count(), 1, "exactly one state runs a turn");
    all
}

// ---- the service ------------------------------------------------------------------------------------------

/// Keeps every event in the order it arrived.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<AiEvent>>,
}

impl AiSink for Recorder {
    fn emit(&self, event: &AiEvent) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push(event.clone());
    }
}

impl Recorder {
    fn all(&self) -> Vec<AiEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// What a turn ended with.
struct Ended {
    route: Route,
    end: EndView,
    text: String,
}

/// A service with the Apple provider over `backend` and a second provider `other`, a scratch config directory and the
/// recorder of its events.
struct World {
    service: AiService,
    sink: Arc<Recorder>,
    other: Arc<ScriptedProvider>,
    dir: PathBuf,
}

impl World {
    fn new(tag: &str, backend: Arc<ScriptedBackend>) -> World {
        let dir = std::env::temp_dir().join(format!("holzwave-apple-matrix-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let sink = Arc::new(Recorder::default());
        let store: Arc<dyn ConversationStore> = Arc::new(MemoryStore::new());
        let service = AiService::builder(store, sink.clone() as Arc<dyn AiSink>, &dir).build();
        service.register_provider(PROVIDER_ID, Arc::new(AppleProvider::new(backend))).expect("apple is registered");
        let other = Arc::new(ScriptedProvider::new("other").with_turn(Script::reply("other says hi")));
        service.register_provider("other", other.clone() as Arc<dyn Provider>).expect("the second provider is registered");
        World { service, sink, other, dir }
    }

    /// Sends `text` pinned to the on-device model and waits for the end of the turn.
    fn run_turn(&self, text: &str) -> Ended {
        let chat = self.service.new_conversation().expect("a chat id");
        self.service
            .send(SendRequest::new(chat.clone(), text.to_owned(), 1).profile(PROVIDER_ID).model(MODEL_ID))
            .expect("the send is accepted");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let events: Vec<AiEvent> = self.sink.all().into_iter().filter(|event| event.conversation == chat.as_str()).collect();
            if events.iter().any(|event| matches!(event.kind, AiEventKind::TurnEnded(_))) {
                let mut route = None;
                let mut end = None;
                let mut text = String::new();
                for event in events {
                    match event.kind {
                        AiEventKind::TurnStarted { route: started, .. } => route = Some(started),
                        AiEventKind::Message { message } if message.role == Role::Assistant => {
                            for part in message.parts {
                                if let ChatPart::Text { text: piece } = part {
                                    text.push_str(&piece);
                                }
                            }
                        }
                        AiEventKind::TurnEnded(ended) => end = Some(ended),
                        _ => {}
                    }
                }
                return Ended { route: route.expect("a route"), end: end.expect("an end"), text };
            }
            assert!(Instant::now() < deadline, "the turn did not end in time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_registry_runs_a_turn_only_for_an_available_backend_and_never_falls_back() {
    for state in states() {
        let backend = Arc::new(
            ScriptedBackend::new().with_availability(state).with_turn(BackendScript::reply("Hello from the device")),
        );
        let world = World::new("registry", backend.clone());
        let ended = world.run_turn("hello");
        if runs_a_turn(state) {
            assert_eq!(ended.end.end, "done", "{}: the turn", state.as_str());
            assert_eq!(ended.route.profile, "apple", "{}: the route", state.as_str());
            assert_eq!(ended.route.model, "system", "{}: the model", state.as_str());
            assert_eq!(ended.route.host, None, "{}: the device has no host", state.as_str());
            assert_eq!(ended.route.locality, Locality::OnDevice, "{}: the locality", state.as_str());
            assert_eq!(ended.text, "Hello from the device", "{}: the answer", state.as_str());
            assert_eq!(backend.calls(), 1, "{}: one backend call", state.as_str());
        } else {
            assert_eq!(ended.end.end, "failed", "{}: the turn", state.as_str());
            let error = ended.end.error.as_ref().unwrap_or_else(|| panic!("{}: no error", state.as_str()));
            assert_eq!(error.class, "unavailable", "{}: the class", state.as_str());
            assert!(error.text.contains(state.as_str()), "{}: the text holds the state key: {}", state.as_str(), error.text);
            assert_eq!(backend.calls(), 0, "{}: the backend ran", state.as_str());
        }
        assert_eq!(world.other.calls(), 0, "{}: the turn moved to the other provider", state.as_str());
    }
}

#[test]
fn every_backend_error_reaches_the_page_as_its_class_after_exactly_one_call() {
    let table = [
        (AppleError::ContextExceeded, "context_length"),
        (AppleError::Guardrail, "content_filter"),
        (AppleError::Refusal, "content_filter"),
        (AppleError::RateLimited, "rate_limit"),
        (AppleError::Concurrent, "overload"),
        (AppleError::AssetsUnavailable, "unavailable"),
        (AppleError::Unavailable, "unavailable"),
        (AppleError::UnsupportedLanguage, "invalid_request"),
        (AppleError::Timeout, "timeout"),
        (AppleError::Other, "protocol"),
        (AppleError::Inconsistent, "protocol"),
        (AppleError::ReplyTooLarge, "protocol"),
    ];
    let covered: Vec<AppleError> = table.iter().map(|(kind, _)| *kind).collect();
    for kind in AppleError::ALL {
        assert!(kind == AppleError::Cancelled || covered.contains(&kind), "{kind:?} has no row");
    }
    for (kind, class) in table {
        let backend = Arc::new(
            ScriptedBackend::new()
                .with_turn(BackendScript::new().fail(kind))
                .with_turn(BackendScript::reply("never asked")),
        );
        let world = World::new("errors", backend.clone());
        let ended = world.run_turn("hello");
        assert_eq!(ended.end.end, "failed", "{kind:?}: the turn");
        let error = ended.end.error.as_ref().unwrap_or_else(|| panic!("{kind:?}: no error"));
        assert_eq!(error.class, class, "{kind:?}: {}", error.text);
        assert_eq!(backend.calls(), 1, "{kind:?}: exactly one backend call, nothing is retried");
        assert_eq!(backend.requests().len(), 1, "{kind:?}: the second script was not used");
        for word in ["FoundationModels", "GenerationError", "exceededContextWindowSize", "guardrailViolation", "NSError", "Swift"] {
            assert!(!error.text.contains(word), "{kind:?}: framework wording in {}", error.text);
        }
        assert_eq!(world.other.calls(), 0, "{kind:?}: no fallback");
    }
}

// ---- consent ----------------------------------------------------------------------------------------------

#[test]
fn a_context_approved_for_another_provider_is_refused_before_the_backend() {
    let backend = Arc::new(ScriptedBackend::new().with_turn(BackendScript::reply("never asked")));
    let provider = AppleProvider::new(backend.clone());
    let foreign = common::approved_for_provider("someone-else", "CANARY-FOREIGN-CONTEXT");
    let request = common::with_context(common::user_request("", "hello"), foreign);
    let (result, _) = common::run(&provider, &request, &CancelToken::new());
    let error = result.expect_err("a foreign context is refused");
    assert_eq!(error.class().as_str(), "policy", "{error}");
    assert_eq!(backend.calls(), 0, "the backend ran");
    assert!(backend.requests().is_empty(), "the backend recorded a request");
    assert!(!error.to_string().contains("CANARY") && !format!("{error:?}").contains("CANARY"), "the context text is in the error");

    // The same context approved for the provider is rendered.
    let own = common::approved("CANARY-OWN-CONTEXT");
    let envelope = own.envelope().to_owned();
    let request = common::with_context(common::user_request("", "hello"), own);
    let (result, _) = common::run(&provider, &request, &CancelToken::new());
    assert!(result.is_ok(), "the own context was refused: {:?}", result.err());
    let sent = backend.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].prompt.matches(&envelope).count(), 1, "the envelope reaches the prompt once, byte for byte");
    assert!(!sent[0].instructions.contains("holzwave-context"), "the envelope is not an instruction");
}

/// The four items of an earlier note. The label of the last one has a hostile tail of 300 characters.
fn marker_note() -> ContextNote {
    let escape = char::from_u32(0x1b).expect("escape");
    let override_ = char::from_u32(0x202e).expect("override");
    let zero_width = char::from_u32(0x200b).expect("zero-width space");
    let tag = char::from_u32(0xe0041).expect("tag character");
    let mut tail = format!("{escape}[31m{override_}{zero_width}{tag}\nnext line ");
    while tail.chars().count() < 300 {
        tail.push('t');
    }
    ContextNote {
        hash: "00".repeat(32),
        items: vec![
            NoteItem { kind: ContextKind::Screen, label: "Screen of pane 1".to_owned(), bytes: 120 },
            NoteItem { kind: ContextKind::LastCommand, label: "make test".to_owned(), bytes: 4096 },
            NoteItem { kind: ContextKind::File, label: "notes.txt".to_owned(), bytes: 40 },
            NoteItem { kind: ContextKind::Selection, label: format!("Selection of the pane {tail}"), bytes: 77 },
        ],
        stored: Some(STORED.to_owned()),
        consented_to: Consent { provider: "earlier".to_owned(), locality: Locality::Cloud },
    }
}

fn note_request(note: ContextNote) -> ChatRequest {
    let mut request = ChatRequest::new(MODEL_ID);
    request.system = "You are a terminal assistant.".to_owned();
    request.messages.push(Message::new("u1", Role::User, vec![Part::Context(note), Part::Text { text: "explain".to_owned() }], 1));
    request
}

/// The bracketed lines of a text.
fn bracketed(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| line.starts_with('[') && line.ends_with(']')).map(str::to_owned).collect()
}

fn unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}'
        )
}

#[test]
fn an_unapproved_note_becomes_one_marker_line_with_the_same_facts_as_the_wires() {
    let backend = Arc::new(
        ScriptedBackend::new().with_turn(BackendScript::reply("a")).with_turn(BackendScript::reply("b")).with_turn(BackendScript::reply("c")),
    );
    let provider = AppleProvider::new(backend.clone());
    let note = marker_note();
    let request = note_request(note.clone());
    let (first, _) = common::run(&provider, &request, &CancelToken::new());
    assert!(first.is_ok(), "{:?}", first.err());
    let sent = backend.requests();
    let prompt = &sent[0].prompt;
    let instructions = &sent[0].instructions;

    // M1 one line, M2 in the user text before the user's own text and never in the instructions.
    let lines = bracketed(prompt);
    assert_eq!(lines.len(), 1, "M1: exactly one bracketed line: {lines:?}");
    let line = &lines[0];
    assert!(prompt.find(line.as_str()) < prompt.find("explain"), "M2: the line comes before the user's text");
    assert!(bracketed(instructions).is_empty() && !instructions.contains(line.as_str()), "M2: the instructions hold a marker line");

    // M3 facts in note order.
    let mut from = 0;
    for lead in ["Screen of pane 1", "make test", "notes.txt", "Selection of the pane"] {
        let at = line[from..].find(lead).unwrap_or_else(|| panic!("M3: {lead:?} is missing or out of order in {line}"));
        from += at + lead.len();
    }
    let runs: Vec<&str> = line.split(|c: char| !c.is_ascii_digit()).filter(|run| !run.is_empty()).collect();
    let mut next = 0;
    for count in ["120", "4096", "40", "77"] {
        let at = runs[next..].iter().position(|run| *run == count).unwrap_or_else(|| panic!("M3: {count} missing or out of order: {runs:?}"));
        next += at + 1;
    }

    // M4 cleaning and bounds.
    assert!(!line.chars().any(unsafe_char), "M4: an unsafe character in the line: {line:?}");
    assert!(!line.chars().any(|c| ('\u{7f}'..='\u{9f}').contains(&c)), "M4: a C1 control in the line");
    let longest = line.split(|c| c != 't').map(str::len).max().unwrap_or(0);
    assert!(longest <= 240, "M4: a label contributes {longest} characters");
    assert!(line.chars().count() <= 2048, "M4: the line has {} characters", line.chars().count());

    // M5 exclusion.
    for text in [prompt, instructions] {
        assert!(!text.contains("holzwave-context") && !text.contains("holzwave-item"), "M5: the envelope grammar is sent");
        assert!(!text.contains(STORED), "M5: the stored text is sent");
    }

    // M6 deterministic.
    let (second, _) = common::run(&provider, &request, &CancelToken::new());
    assert!(second.is_ok());
    assert_eq!(backend.requests()[1].prompt, *prompt, "M6: the same request gave another prompt");

    // M7 approval flips it.
    let approved = common::approved("CANARY-APPROVED-TEXT");
    let envelope = approved.envelope().to_owned();
    let mut with_approval = note_request(ContextNote::for_approved(&approved, Some(STORED.to_owned())));
    with_approval.context.push(approved);
    let (third, _) = common::run(&provider, &with_approval, &CancelToken::new());
    assert!(third.is_ok(), "{:?}", third.err());
    let approved_prompt = backend.requests()[2].prompt.clone();
    assert_eq!(approved_prompt.matches(&envelope).count(), 1, "M7: the envelope occurs once");
    assert!(bracketed(&approved_prompt).is_empty(), "M7: a marker line next to the envelope");
    assert!(!approved_prompt.contains(STORED), "M7: the stored text is sent");

    // M8 the same wording as every network dialect, which render the note with the same function.
    assert_eq!(*line, marker_line(&note), "M8: the wording is the one of the dialects");
    println!("marker line apple: {line}");
}

#[test]
fn the_provider_reports_the_device_declares_no_tools_and_refuses_a_foreign_model() {
    let backend = Arc::new(ScriptedBackend::new().with_context_size(Some(8192)).with_turn(BackendScript::reply("never asked")));
    let provider = AppleProvider::new(backend.clone());
    let caps = provider.capabilities(MODEL_ID);
    assert!(!caps.tools, "the device declares no tools");
    assert_eq!(caps.locality, Locality::OnDevice);
    assert_eq!(caps.context_tokens, Some(8192), "the window of the backend");
    let unknown = AppleProvider::new(Arc::new(ScriptedBackend::new()));
    assert_eq!(unknown.capabilities(MODEL_ID).context_tokens, Some(4096), "the documented window when the backend cannot say");

    // A tool and a foreign model are refused before the backend.
    let mut with_tool = common::user_request("", "hello");
    with_tool.tools.push(ToolSpec { name: "read_file".to_owned(), description: "reads".to_owned(), input_schema: Default::default() });
    let (result, _) = common::run(&provider, &with_tool, &CancelToken::new());
    let error = result.expect_err("a tool is refused");
    assert!(matches!(error, AiError::InvalidRequest(_)), "{error}");
    let mut foreign = common::user_request("", "hello");
    foreign.model = "another-model".to_owned();
    let (result, _) = common::run(&provider, &foreign, &CancelToken::new());
    let error = result.expect_err("a foreign model is refused");
    assert!(matches!(error, AiError::InvalidRequest(_)), "{error}");
    assert_eq!(backend.calls(), 0, "the backend ran for a refused request");
}

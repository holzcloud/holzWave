//! The ask desk and the service against a scripted provider: no network and no Zig.
//!
//! A request from a shell is an ask. What is proved here: it waits for the person and reaches no provider before the
//! answer; only `settle` with the seal of the card releases it; the piped text goes through the context gate and is
//! never shown as text; the destination is the person's own last route or the only provider; the turn is ask-only; the
//! request is not kept; and every end (a refusal, a timeout, a program that left, a closed pane, a shutdown) costs the
//! provider nothing. Every wait is bounded.

mod common;

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use holzwave_core::ai::provider::{CancelToken, ChatRequest};
use holzwave_core::ai::scripted::{Script, ScriptedProvider};
use holzwave_core::ai::service::events::AiEvent;
use holzwave_core::ai::service::{
    AiService, AiSink, AskEvent, AskFault, AskInput, AskLimits, AskNotify, AskOut, AskVerdict, AskView, Asks, ClosedWhy,
    SendRequest,
};
use holzwave_core::ai::store::MemoryStore as ChatMemory;
use holzwave_core::ai::types::{Capabilities, Locality, Part, Role};
use holzwave_core::layout::PaneId;
use holzwave_core::settings::{AiLocalContext, Settings};

/// A key written in the piped text. It appears in test code only: a hit anywhere else is a leak.
const KEY_CANARY: &str = "sk-ant-api03-zqask7c41e90b3aXYZ0123456789";
/// A word of the piped text that must never reach the card or an event.
const TEXT_CANARY: &str = "zq-piped-text-5e1b94c2d7";

// ---- the rig ------------------------------------------------------------------------------------------------------

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sink of the page: keeps the conversation id and the kind name of every event it gets.
#[derive(Default)]
struct Pages {
    seen: Mutex<Vec<(String, &'static str)>>,
}

impl AiSink for Pages {
    fn emit(&self, event: &AiEvent) {
        lock(&self.seen).push((event.conversation.clone(), event.kind.name()));
    }
}

impl Pages {
    fn conversations(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock(&self.seen).iter().map(|(id, _)| id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    fn kinds_of(&self, chat: &str) -> Vec<&'static str> {
        lock(&self.seen).iter().filter(|(id, _)| id == chat).map(|(_, kind)| *kind).collect()
    }
}

/// The notifier of the window: keeps every event with the pane it was for.
#[derive(Default)]
struct Notes {
    log: Mutex<Vec<(PaneId, AskEvent)>>,
}

impl AskNotify for Notes {
    fn notify(&self, pane: PaneId, event: &AskEvent) {
        lock(&self.log).push((pane, event.clone()));
    }
}

impl Notes {
    fn all(&self) -> Vec<(PaneId, AskEvent)> {
        lock(&self.log).clone()
    }

    fn waiting_views(&self) -> Vec<AskView> {
        self.all()
            .into_iter()
            .filter_map(|(_, event)| match event {
                AskEvent::Waiting { view } => Some(view),
                AskEvent::Closed { .. } => None,
            })
            .collect()
    }

    fn closed(&self) -> Vec<(String, ClosedWhy)> {
        self.all()
            .into_iter()
            .filter_map(|(_, event)| match event {
                AskEvent::Closed { ask, why } => Some((ask, why)),
                AskEvent::Waiting { .. } => None,
            })
            .collect()
    }

    /// The first card that was shown, waiting up to five seconds for it.
    fn wait_waiting(&self) -> Option<AskView> {
        common::wait_until(Duration::from_secs(5), || !self.waiting_views().is_empty());
        self.waiting_views().into_iter().next()
    }

    fn wait_closed(&self, count: usize) -> bool {
        common::wait_until(Duration::from_secs(5), || self.closed().len() >= count)
    }
}

/// What the caller's standard output and standard error received.
#[derive(Default)]
struct Log {
    texts: Vec<String>,
    notes: Vec<String>,
}

/// The caller's end of an ask: records what it is given, and can start to fail after some pieces.
struct Screen {
    log: Arc<Mutex<Log>>,
    fail_after: Option<usize>,
}

impl AskOut for Screen {
    fn text(&mut self, text: &str) -> io::Result<()> {
        let mut log = lock(&self.log);
        if self.fail_after.is_some_and(|limit| log.texts.len() >= limit) {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the program left"));
        }
        log.texts.push(text.to_owned());
        Ok(())
    }

    fn note(&mut self, text: &str) -> io::Result<()> {
        lock(&self.log).notes.push(text.to_owned());
        Ok(())
    }
}

/// An ask on its thread.
struct Run {
    handle: JoinHandle<Result<(), AskFault>>,
    log: Arc<Mutex<Log>>,
    cancel: CancelToken,
}

impl Run {
    fn texts(&self) -> Vec<String> {
        lock(&self.log).texts.clone()
    }

    fn notes(&self) -> Vec<String> {
        lock(&self.log).notes.clone()
    }

    fn answer(&self) -> String {
        self.texts().concat()
    }

    fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// The end of the ask, waiting up to five seconds for it.
    fn end(self) -> Result<(), AskFault> {
        assert!(common::wait_until(Duration::from_secs(5), || self.handle.is_finished()), "the ask did not end");
        self.handle.join().expect("the ask thread did not panic")
    }
}

/// A desk, a service over it and a scripted provider that serves `replies` in order.
struct Rig {
    asks: Arc<Asks>,
    service: Arc<AiService>,
    pages: Arc<Pages>,
    notes: Arc<Notes>,
    provider: Arc<ScriptedProvider>,
}

impl Rig {
    fn new(locality: Locality, replies: &[&str]) -> Rig {
        Rig::build(locality, replies, AskLimits::default())
    }

    fn build(locality: Locality, replies: &[&str], limits: AskLimits) -> Rig {
        let capabilities = Capabilities::conservative(locality);
        let turns = replies.iter().map(|reply| Script::new().text(*reply).usage(1, 1)).collect();
        Rig::with_capabilities(capabilities, turns, limits, None)
    }

    /// A rig whose provider plays `turns`.
    fn with_turns(locality: Locality, turns: Vec<Script>) -> Rig {
        Rig::with_capabilities(Capabilities::conservative(locality), turns, AskLimits::default(), None)
    }

    fn with_capabilities(
        capabilities: Capabilities,
        turns: Vec<Script>,
        limits: AskLimits,
        host: Option<Arc<dyn holzwave_core::ai::tools::ToolHost>>,
    ) -> Rig {
        let pages = Arc::new(Pages::default());
        let notes = Arc::new(Notes::default());
        let asks = Asks::with_limits(pages.clone(), notes.clone(), limits);
        let mut builder =
            AiService::builder(Arc::new(ChatMemory::new()), asks.clone() as Arc<dyn AiSink>, common::scratch_dir("ask"));
        if let Some(host) = host {
            builder = builder.tool_host(host);
        }
        let service = Arc::new(builder.build());
        let mut provider = ScriptedProvider::new("scripted").with_capabilities(capabilities);
        for turn in turns {
            provider = provider.with_turn(turn);
        }
        let provider = Arc::new(provider);
        service.register_provider("scripted", provider.clone()).expect("the provider is registered");
        asks.add_candidate("scripted", "m");
        Rig { asks, service, pages, notes, provider }
    }

    /// Starts an ask from `pane` on a thread of its own.
    fn start(&self, pane: PaneId, prompt: &str, input: &str) -> Run {
        self.start_failing(pane, prompt, input, None)
    }

    fn start_failing(&self, pane: PaneId, prompt: &str, input: &str, fail_after: Option<usize>) -> Run {
        let (asks, service) = (self.asks.clone(), self.service.clone());
        let log = Arc::new(Mutex::new(Log::default()));
        let cancel = CancelToken::new();
        let (thread_log, thread_cancel) = (log.clone(), cancel.clone());
        let ask = AskInput { pane, prompt: prompt.to_owned(), input: input.to_owned() };
        let handle = thread::Builder::new()
            .name("test-ask".to_owned())
            .spawn(move || {
                let mut screen = Screen { log: thread_log, fail_after };
                asks.run(&service, ask, &mut screen, &thread_cancel)
            })
            .expect("a thread");
        Run { handle, log, cancel }
    }

    /// Switches the setting `ai_local_context` to `auto`: a destination on the device is not asked about.
    fn local_auto(&self) {
        let settings = Settings { ai_local_context: AiLocalContext::Auto, ..Settings::default() };
        self.service.apply_settings(&settings);
    }

    /// The chats the service lists: an ask leaves none.
    fn chats(&self) -> usize {
        self.service.list_conversations().expect("a listing").chats.len()
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.provider.requests()
    }
}

fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The text of the last user message of a request.
fn user_text(request: &ChatRequest) -> String {
    let message = request.messages.iter().rev().find(|message| message.role == Role::User).expect("a user message");
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---- the tracer ---------------------------------------------------------------------------------------------------

#[test]
fn an_ask_waits_for_the_person_and_only_then_reaches_the_provider() {
    let rig = Rig::new(Locality::Cloud, &["The build fails because the key is missing."]);
    let piped = format!("first line {TEXT_CANARY}\nANTHROPIC_API_KEY={KEY_CANARY}\nthird line\n");
    let run = rig.start(3, "why does this fail", &piped);

    let view = rig.notes.wait_waiting().expect("a card waits within five seconds");
    assert!(is_hex(&view.ask, 32), "{view:?}");
    assert!(is_hex(&view.seal, 64), "{view:?}");
    assert_eq!(view.pane, 3);
    assert_eq!(view.prompt, "why does this fail");
    assert!(view.prompt_findings.is_empty());
    let input = view.input.clone().expect("the piped text is counted");
    assert_eq!(input.lines, 3);
    assert_eq!(input.bytes, piped.len() as u64);
    assert!(input.sent_bytes > 0 && input.findings.iter().any(|f| f.rule == "anthropic_key" && f.count == 1), "{input:?}");
    assert_eq!((view.profile.as_str(), view.model.as_str(), view.locality), ("scripted", "m", Locality::Cloud));

    // The card holds counts and findings, never the piped text.
    let wire = serde_json::to_string(&AskEvent::Waiting { view: view.clone() }).expect("serialises");
    for word in [TEXT_CANARY, KEY_CANARY, "first line", "third line"] {
        assert!(!wire.contains(word), "the card holds piped text: {word}");
        assert!(!format!("{view:?}").contains(word), "the debug text holds piped text: {word}");
    }
    assert_eq!(rig.notes.waiting_views().len(), 1, "one waiting event");
    assert_eq!(rig.notes.all()[0].0, 3, "told to the pane that asked");
    assert_eq!(run.notes(), ["waiting for your answer in holzWave (pane 3)"], "exactly one note");

    // Nothing leaves while the person has not answered.
    assert!(!common::wait_until(Duration::from_millis(500), || rig.provider.calls() > 0), "a request left too early");
    assert!(!run.is_finished(), "the ask returned before an answer");
    assert!(rig.pages.conversations().is_empty(), "no event of the ask reaches the page");

    rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect("the answer is taken");
    let answer = {
        let end = common::wait_until(Duration::from_secs(5), || run.is_finished());
        assert!(end, "the ask ends after the answer");
        run.answer()
    };
    assert_eq!(answer, "The build fails because the key is missing.");
    run.end().expect("the ask ends ok");

    let requests = rig.requests();
    assert_eq!(requests.len(), 1, "one ask is one request");
    let request = &requests[0];
    assert_eq!(request.context.len(), 1, "the piped text travels as one approved context");
    let envelope = request.context[0].envelope();
    assert!(envelope.contains("ANTHROPIC_API_KEY=[REDACTED:anthropic_key]"), "{envelope}");
    assert!(!envelope.contains("zqask7c41e90b3a"), "the key reached the provider");
    assert!(envelope.contains("text piped into wsh ai"), "{envelope}");
    assert_eq!(user_text(request), "why does this fail", "the user's text is the prompt");
    assert!(request.tools.is_empty(), "an ask declares no tool");

    assert!(rig.notes.wait_closed(1));
    assert_eq!(rig.notes.closed(), [(view.ask.clone(), ClosedWhy::Sent)]);
    assert!(common::wait_until(Duration::from_secs(5), || rig.chats() == 0 && rig.service.active_turns() == 0));
    assert!(rig.pages.conversations().is_empty(), "still nothing reached the page");
}

#[test]
fn the_answer_reaches_the_caller_in_pieces_and_the_chat_is_deleted_afterwards() {
    // Two answers of 20000 characters, each streamed by the model in forty pieces.
    let long: String = (0..20_000).map(|n| char::from(b'a' + (n % 26) as u8)).collect();
    let streamed = |reply: &str| {
        let mut script = Script::new();
        for piece in reply.as_bytes().chunks(500) {
            script = script.text(String::from_utf8_lossy(piece).into_owned());
        }
        script.usage(1, 1)
    };
    let rig = Rig::with_turns(Locality::OnDevice, vec![streamed(&long), streamed(&long)]);
    rig.local_auto();
    for round in 0..2 {
        let run = rig.start(1, "write a long answer", "");
        assert!(common::wait_until(Duration::from_secs(5), || run.is_finished()), "round {round} ends");
        let pieces = run.texts();
        assert!(!pieces.is_empty() && pieces.iter().all(|piece| !piece.is_empty()), "the answer arrives as text pieces");
        assert_eq!(pieces.concat(), long, "the pieces join to the reply, in order and whole");
        assert!(run.notes().is_empty(), "nothing waited, so no note was written");
        run.end().expect("the ask ends ok");
    }
    assert_eq!(rig.provider.calls(), 2);
    assert!(rig.notes.all().is_empty(), "a destination on the device with auto asks nothing");
    assert!(common::wait_until(Duration::from_secs(5), || rig.service.active_turns() == 0));
    assert_eq!(rig.chats(), 0, "no chat is left behind");
    assert!(rig.pages.conversations().is_empty());
}

#[test]
fn events_of_an_ask_never_reach_the_page_sink_and_other_chats_still_do() {
    let rig = Rig::new(Locality::OnDevice, &["from the ask", "from the chat"]);
    rig.local_auto();
    let run = rig.start(1, "ask something", "");
    assert!(common::wait_until(Duration::from_secs(5), || run.is_finished()));
    assert_eq!(run.answer(), "from the ask");
    run.end().expect("the ask ends ok");
    assert!(rig.pages.conversations().is_empty(), "no event of the ask reached the page");
    assert_eq!(rig.asks.last_route(), None, "an ask does not set the route of the next one");

    // An ordinary chat through the same service reaches the page and sets the route.
    let chat = rig.service.new_conversation().expect("a chat id");
    let request = SendRequest::new(chat.clone(), "hello there".to_owned(), 1).profile("scripted").model("m");
    rig.service.send(request).expect("the send is accepted");
    assert!(
        common::wait_until(Duration::from_secs(5), || rig.pages.kinds_of(chat.as_str()).contains(&"turn_ended")),
        "the chat turn ends"
    );
    let kinds = rig.pages.kinds_of(chat.as_str());
    assert_eq!(kinds.first().copied(), Some("turn_started"));
    assert!(kinds.contains(&"delta") || kinds.contains(&"message"), "{kinds:?}");
    assert_eq!(rig.pages.conversations(), [chat.as_str().to_owned()], "only the ordinary chat is seen");
    assert_eq!(rig.asks.last_route(), Some(("scripted".to_owned(), "m".to_owned())));

    // And the counter of requests: the ask and the chat each made one.
    assert_eq!(rig.provider.calls(), 2);
}

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
use holzwave_core::ai::tools::{Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName, ToolOutput};
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

/// What a rig is built from.
struct Setup {
    capabilities: Capabilities,
    turns: Vec<Script>,
    limits: AskLimits,
    host: Option<Arc<dyn ToolHost>>,
    candidate: bool,
}

impl Setup {
    fn new(locality: Locality) -> Setup {
        Setup {
            capabilities: Capabilities::conservative(locality),
            turns: Vec::new(),
            limits: AskLimits::default(),
            host: None,
            candidate: true,
        }
    }

    fn replies(mut self, replies: &[&str]) -> Setup {
        self.turns = replies.iter().map(|reply| Script::new().text(*reply).usage(1, 1)).collect();
        self
    }

    fn turns(mut self, turns: Vec<Script>) -> Setup {
        self.turns = turns;
        self
    }

    fn limits(mut self, limits: AskLimits) -> Setup {
        self.limits = limits;
        self
    }

    fn host(mut self, host: Arc<dyn ToolHost>, capabilities: Capabilities) -> Setup {
        self.host = Some(host);
        self.capabilities = capabilities;
        self
    }

    /// No provider is registered with the desk as set up.
    fn no_candidate(mut self) -> Setup {
        self.candidate = false;
        self
    }

    fn rig(self) -> Rig {
        let pages = Arc::new(Pages::default());
        let notes = Arc::new(Notes::default());
        let asks = Asks::with_limits(pages.clone(), notes.clone(), self.limits);
        let mut builder =
            AiService::builder(Arc::new(ChatMemory::new()), asks.clone() as Arc<dyn AiSink>, common::scratch_dir("ask"));
        if let Some(host) = self.host {
            builder = builder.tool_host(host);
        }
        let service = Arc::new(builder.build());
        let mut provider = ScriptedProvider::new("scripted").with_capabilities(self.capabilities);
        for turn in self.turns {
            provider = provider.with_turn(turn);
        }
        let provider = Arc::new(provider);
        service.register_provider("scripted", provider.clone()).expect("the provider is registered");
        if self.candidate {
            asks.add_candidate("scripted", "m");
        }
        Rig { asks, service, pages, notes, provider }
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
        Setup::new(locality).replies(replies).rig()
    }

    /// A rig whose provider plays `turns`.
    fn with_turns(locality: Locality, turns: Vec<Script>) -> Rig {
        Setup::new(locality).turns(turns).rig()
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

// ---- the rules ----------------------------------------------------------------------------------------------------

/// Waits for the end of an ask that must come soon.
fn ends_within(run: Run, wait: Duration) -> Result<(), AskFault> {
    assert!(common::wait_until(wait, || run.is_finished()), "the ask did not end within {wait:?}");
    run.end()
}

fn reason(result: Result<(), AskFault>) -> &'static str {
    result.expect_err("the ask ends with a fault").reason()
}

#[test]
fn a_refusal_sends_nothing_and_starts_a_cooldown() {
    let limits = AskLimits { cooldown: Duration::from_millis(400), ..AskLimits::default() };
    let rig = Setup::new(Locality::Cloud).replies(&["unused"]).limits(limits).rig();

    let first = rig.start(3, "explain this", "some text\n");
    let view = rig.notes.wait_waiting().expect("a card waits");
    rig.asks.settle(&view.ask, AskVerdict::Refuse, &view.seal).expect("the refusal is taken");
    assert_eq!(reason(first.end()), "refused");
    assert_eq!(rig.provider.calls(), 0, "a refusal sends nothing");
    assert_eq!(rig.notes.closed(), [(view.ask.clone(), ClosedWhy::Refused)]);

    // The same pane rests: refused at once, with no card and no chat.
    let again = rig.start(3, "explain this", "some text\n");
    assert_eq!(reason(ends_within(again, Duration::from_secs(1))), "cooling_down");
    assert_eq!(rig.notes.waiting_views().len(), 1, "no second card");
    assert_eq!(rig.chats(), 0);

    // Another pane is not held back.
    let other = rig.start(4, "explain that", "");
    assert!(common::wait_until(Duration::from_secs(5), || rig.notes.waiting_views().len() == 2), "pane 4 waits");
    other.cancel.cancel();
    assert_eq!(reason(ends_within(other, Duration::from_secs(2))), "gone");

    // After the rest the first pane waits again.
    thread::sleep(Duration::from_millis(500));
    let later = rig.start(3, "explain this", "");
    assert!(common::wait_until(Duration::from_secs(5), || rig.notes.waiting_views().len() == 3), "pane 3 waits again");
    later.cancel.cancel();
    assert_eq!(reason(ends_within(later, Duration::from_secs(2))), "gone");
    assert_eq!(rig.provider.calls(), 0);
}

#[test]
fn nobody_answering_ends_the_ask_without_a_request() {
    let limits = AskLimits { wait: Duration::from_millis(300), ..AskLimits::default() };
    let rig = Setup::new(Locality::Cloud).replies(&["unused"]).limits(limits).rig();
    let run = rig.start(3, "anyone there", "");
    let view = rig.notes.wait_waiting().expect("a card waits");
    assert_eq!(reason(ends_within(run, Duration::from_secs(5))), "timed_out");
    assert_eq!(rig.notes.closed(), [(view.ask.clone(), ClosedWhy::TimedOut)]);
    assert_eq!(rig.provider.calls(), 0, "nothing was sent");
    assert_eq!(rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect_err("late").reason(), "unknown_ask");

    // The timeout started the rest of the pane.
    let again = rig.start(3, "anyone there", "");
    assert_eq!(reason(ends_within(again, Duration::from_secs(1))), "cooling_down");
}

#[test]
fn a_caller_that_leaves_while_the_ask_waits_withdraws_it() {
    let rig = Rig::new(Locality::Cloud, &["unused"]);
    let run = rig.start(3, "hello", "some text\n");
    let view = rig.notes.wait_waiting().expect("a card waits");
    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(1))), "gone");
    assert!(rig.notes.wait_closed(1));
    assert_eq!(rig.notes.closed(), [(view.ask.clone(), ClosedWhy::Gone)]);
    assert_eq!(rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect_err("late").reason(), "unknown_ask");
    assert_eq!(rig.provider.calls(), 0);
    assert!(rig.asks.pending(&|_| true).is_empty());
}

#[test]
fn a_caller_that_leaves_during_the_answer_stops_the_turn() {
    let stalling = Script::new().text("partial").stall(Duration::from_secs(30));
    let rig = Rig::with_turns(Locality::Cloud, vec![stalling]);
    let run = rig.start(3, "hello", "");
    let view = rig.notes.wait_waiting().expect("a card waits");
    rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect("the answer is taken");
    assert!(common::wait_until(Duration::from_secs(5), || run.answer() == "partial"), "the answer started");
    assert_eq!(rig.service.active_turns(), 1);

    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(1))), "gone");
    assert!(common::wait_until(Duration::from_secs(1), || rig.service.active_turns() == 0), "the turn was stopped");
    assert!(common::wait_until(Duration::from_secs(2), || rig.chats() == 0), "the chat was deleted");
    assert_eq!(rig.provider.calls(), 1);
    assert_eq!(rig.notes.closed(), [(view.ask.clone(), ClosedWhy::Sent)], "the card was closed once, by the answer");
}

#[test]
fn a_wrong_seal_keeps_the_ask_and_an_unknown_ask_is_named() {
    let rig = Rig::new(Locality::Cloud, &["fine"]);
    let run = rig.start(3, "hello", "text\n");
    let view = rig.notes.wait_waiting().expect("a card waits");

    let wrong = format!("{:0>64}", "1");
    let refused = rig.asks.settle(&view.ask, AskVerdict::Send, &wrong).expect_err("a wrong seal");
    assert_eq!(refused.reason(), "seal_mismatch");
    assert_eq!(refused.to_string(), "this request changed since it was shown; look at it again");
    assert_eq!(rig.asks.settle(&view.ask, AskVerdict::Send, "").expect_err("no seal").reason(), "seal_mismatch");
    assert_eq!(rig.asks.pending(&|_| true).len(), 1, "the ask keeps waiting");
    assert!(!run.is_finished());
    assert_eq!(rig.provider.calls(), 0);

    let nobody = "f".repeat(32);
    assert_eq!(rig.asks.settle(&nobody, AskVerdict::Send, &view.seal).expect_err("unknown").reason(), "unknown_ask");
    assert_eq!(
        rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal.replace(&view.seal[..1], "x")).expect_err("bad").reason(),
        "seal_mismatch"
    );

    rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect("the right seal works");
    run.end().expect("the ask ends ok");
    assert_eq!(rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect_err("twice").reason(), "unknown_ask");
    assert_eq!(rig.provider.calls(), 1);
}

#[test]
fn a_closed_pane_withdraws_its_asks_and_a_shutdown_withdraws_all() {
    let stalling = Script::new().text("partial").stall(Duration::from_secs(30));
    let rig = Rig::with_turns(Locality::Cloud, vec![stalling]);

    // An ask that already streams, on pane 5.
    let streaming = rig.start(5, "stream", "");
    let five = rig.notes.wait_waiting().expect("a card waits");
    rig.asks.settle(&five.ask, AskVerdict::Send, &five.seal).expect("sent");
    assert!(common::wait_until(Duration::from_secs(5), || streaming.answer() == "partial"));

    // Two that wait, on panes 3 and 4.
    let three = rig.start(3, "three", "");
    assert!(common::wait_until(Duration::from_secs(5), || rig.notes.waiting_views().len() == 2));
    let four = rig.start(4, "four", "");
    assert!(common::wait_until(Duration::from_secs(5), || rig.notes.waiting_views().len() == 3));
    let views = rig.notes.waiting_views();
    let (ask3, ask4) = (views[1].ask.clone(), views[2].ask.clone());

    assert_eq!(rig.asks.pane_closed(3), 1);
    assert_eq!(reason(ends_within(three, Duration::from_secs(1))), "pane_closed");
    assert!(!four.is_finished(), "pane 4 is left alone");
    assert_eq!(rig.asks.pane_closed(5), 1);
    assert_eq!(reason(ends_within(streaming, Duration::from_secs(2))), "pane_closed");
    assert!(common::wait_until(Duration::from_secs(1), || rig.service.active_turns() == 0));

    assert_eq!(rig.asks.shutdown(), 1);
    assert_eq!(reason(ends_within(four, Duration::from_secs(1))), "shutting_down");
    let late = rig.start(6, "too late", "");
    assert_eq!(reason(ends_within(late, Duration::from_secs(1))), "shutting_down");

    // Every card was closed once, and the answered one by its answer only.
    assert!(common::wait_until(Duration::from_secs(2), || rig.notes.closed().len() >= 3));
    let mut closed = rig.notes.closed();
    closed.sort_by(|a, b| a.0.cmp(&b.0));
    let mut expected = vec![
        (five.ask.clone(), ClosedWhy::Sent),
        (ask3, ClosedWhy::PaneClosed),
        (ask4, ClosedWhy::ShuttingDown),
    ];
    expected.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(closed, expected, "one close for each card");
}

#[test]
fn local_destinations_skip_the_card_only_when_the_setting_says_auto() {
    // On this computer, with the default setting: the person is asked.
    let rig = Rig::new(Locality::Loopback, &["hello from here"]);
    let run = rig.start(1, "say hi", "");
    rig.notes.wait_waiting().expect("a loopback destination asks by default");
    assert_eq!(rig.provider.calls(), 0);
    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(2))), "gone");

    // With auto it is answered at once, and no card is drawn.
    rig.local_auto();
    let cards = rig.notes.waiting_views().len();
    let run = rig.start(2, "say hi", "");
    assert!(common::wait_until(Duration::from_secs(5), || run.is_finished()));
    assert_eq!(run.answer(), "hello from here");
    run.end().expect("ok");
    assert_eq!(rig.notes.waiting_views().len(), cards, "no card for a destination on this computer");

    // A cloud destination asks even with auto.
    let cloud = Rig::new(Locality::Cloud, &["from the cloud"]);
    cloud.local_auto();
    let run = cloud.start(1, "say hi", "");
    cloud.notes.wait_waiting().expect("a cloud destination asks with auto");
    assert!(!common::wait_until(Duration::from_millis(300), || cloud.provider.calls() > 0));
    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(2))), "gone");

    // And a destination on the local network asks as well.
    let lan = Rig::new(Locality::Lan, &["from the lan"]);
    lan.local_auto();
    let run = lan.start(1, "say hi", "");
    lan.notes.wait_waiting().expect("a local network destination asks with auto");
    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(2))), "gone");
}

#[test]
fn local_only_refuses_a_destination_that_leaves_the_device_before_any_card() {
    let rig = Rig::new(Locality::Cloud, &["unused"]);
    rig.service.apply_settings(&Settings { ai_local_only: true, ..Settings::default() });
    let run = rig.start(3, "hello", "text\n");
    let fault = run.end().expect_err("refused before any card");
    assert_eq!(fault.reason(), "context");
    assert_eq!(fault.to_string(), "local only is on and this destination leaves the device");
    assert!(rig.notes.all().is_empty(), "no card was shown");
    assert_eq!(rig.provider.calls(), 0);
    assert_eq!(rig.chats(), 0);

    // On this computer the same setting lets the ask through to its card.
    let local = Rig::new(Locality::Loopback, &["ok"]);
    local.service.apply_settings(&Settings { ai_local_only: true, ..Settings::default() });
    let run = local.start(3, "hello", "");
    local.notes.wait_waiting().expect("a destination on this computer asks");
    run.cancel.cancel();
    assert_eq!(reason(ends_within(run, Duration::from_secs(2))), "gone");
}

#[test]
fn the_destination_follows_the_last_chat_then_the_only_provider_and_never_a_guess() {
    // No provider at all.
    let rig = Setup::new(Locality::Cloud).no_candidate().rig();
    assert_eq!(reason(rig.start(1, "hello", "").end()), "no_provider");
    assert!(rig.notes.all().is_empty());
    assert_eq!(rig.chats(), 0);

    // One provider without a model.
    rig.asks.add_candidate("scripted", "");
    assert_eq!(reason(rig.start(1, "hello", "").end()), "no_model");

    // Two providers and no chat yet: a sentence, not a guess.
    let rig = Rig::new(Locality::Cloud, &["unused"]);
    rig.asks.add_candidate("second", "m2");
    let fault = rig.start(1, "hello", "").end().expect_err("a choice is needed");
    assert_eq!(fault.reason(), "choose_provider");
    assert!(rig.notes.all().is_empty() && rig.chats() == 0 && rig.provider.calls() == 0);

    // An ordinary chat turn on the second provider decides.
    let second = Arc::new(
        ScriptedProvider::new("second")
            .with_capabilities(Capabilities::conservative(Locality::Cloud))
            .with_turn(Script::new().text("from the second").usage(1, 1))
            .with_turn(Script::new().text("the ask answer").usage(1, 1)),
    );
    rig.service.register_provider("second", second.clone()).expect("registered");
    let chat = rig.service.new_conversation().expect("a chat id");
    let request = SendRequest::new(chat.clone(), "hi".to_owned(), 1).profile("second").model("m2");
    rig.service.send(request).expect("sent");
    assert!(common::wait_until(Duration::from_secs(5), || rig.pages.kinds_of(chat.as_str()).contains(&"turn_ended")));
    assert_eq!(rig.asks.last_route(), Some(("second".to_owned(), "m2".to_owned())));

    let run = rig.start(1, "hello", "");
    let view = rig.notes.wait_waiting().expect("a card waits");
    assert_eq!((view.profile.as_str(), view.model.as_str()), ("second", "m2"), "the route of the last chat");
    rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect("sent");
    run.end().expect("ok");
    assert_eq!(second.calls(), 2, "the chat turn and the ask went to the second provider");
    assert_eq!(rig.provider.calls(), 0, "the first provider saw nothing");
}

#[test]
fn the_piped_text_is_cleaned_and_a_canary_never_reaches_the_provider_or_the_card() {
    let rig = Rig::new(Locality::Cloud, &["done"]);
    let hidden = "\u{e0049}\u{e0067}\u{e006e}\u{e006f}\u{e0072}\u{e0065}";
    let piped = format!(
        "plain {TEXT_CANARY}\n\u{1b}[31mred {KEY_CANARY}\u{1b}[0m\nhidden{hidden}tail\nflip \u{202e}this\u{202c}\n"
    );
    let prompt = format!("explain{hidden} this");
    let run = rig.start(3, &prompt, &piped);
    let view = rig.notes.wait_waiting().expect("a card waits");

    assert_eq!(view.prompt, "explain this", "the prompt is shown as it is sent: cleaned");
    assert!(view.prompt_findings.iter().any(|f| f.rule == "tag_chars" && f.count == 6), "{:?}", view.prompt_findings);
    let input = view.input.clone().expect("counts");
    let rules: Vec<&str> = input.findings.iter().map(|f| f.rule.as_str()).collect();
    for rule in ["escape_sequence", "tag_chars", "bidi_control", "anthropic_key"] {
        assert!(rules.contains(&rule), "{rule} is not among {rules:?}");
    }
    assert!(input.sent_bytes < input.bytes, "what is sent is smaller than what arrived");

    let wire = serde_json::to_string(&AskEvent::Waiting { view: view.clone() }).expect("serialises");
    for canary in [TEXT_CANARY, "zqask7c41e90b3a", "\u{1b}", "\u{202e}", "tail", "red "] {
        assert!(!wire.contains(canary), "the card holds {canary:?}");
    }
    assert!(!wire.contains(hidden), "the hidden characters are not in the card");

    rig.asks.settle(&view.ask, AskVerdict::Send, &view.seal).expect("sent");
    run.end().expect("ok");
    let requests = rig.requests();
    let envelope = requests[0].context[0].envelope();
    assert!(!envelope.contains("zqask7c41e90b3a"), "the key reached the provider");
    assert!(!envelope.contains('\u{1b}') && !envelope.contains('\u{202e}') && !envelope.contains(hidden), "{envelope:?}");
    assert!(envelope.contains(TEXT_CANARY), "the text itself is sent after the person agreed");
    assert_eq!(user_text(&requests[0]), "explain this");
}

/// A tool host that offers every tool for every pane, and keeps what it was asked.
#[derive(Default)]
struct EveryTool {
    conversations: Mutex<Vec<String>>,
    runs: std::sync::atomic::AtomicUsize,
}

impl ToolHost for EveryTool {
    fn available(&self, _tool: ToolName, _pane: PaneId) -> bool {
        true
    }

    fn resolve(&self, scope: &Scope, _tool: &Tool) -> Result<Resolved, ToolError> {
        lock(&self.conversations).push(scope.conversation.clone());
        Ok(Resolved::local(1, "this computer").with_path("/tmp/example.txt"))
    }

    fn run(&self, _ticket: &Ticket, _cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(ToolOutput::new("ok".to_owned()))
    }
}

#[test]
fn a_model_that_asks_for_a_tool_is_stopped_and_the_call_is_voided() {
    let host = Arc::new(EveryTool::default());
    let capabilities = Capabilities { tools: true, ..Capabilities::conservative(Locality::OnDevice) };
    let script = Script::new()
        .tool_call("call_1", "read_file", serde_json::json!({ "path": "/tmp/example.txt" }))
        .stop(holzwave_core::ai::types::StopReason::ToolUse);
    let rig = Setup::new(Locality::OnDevice).turns(vec![script]).host(host.clone(), capabilities).rig();
    rig.local_auto();

    let run = rig.start(1, "read the file", "");
    let fault = ends_within(run, Duration::from_secs(5)).expect_err("the ask is stopped");
    assert_eq!(fault.reason(), "tools_refused");
    assert_eq!(fault.to_string(), "the model asked for a tool, which wsh ai does not offer; the request was stopped");
    assert!(!rig.requests()[0].tools.is_empty(), "this host offers tools, so the model could ask for one");
    let conversations = lock(&host.conversations).clone();
    assert_eq!(conversations.len(), 1, "the call was proposed once");
    assert!(rig.service.approvals().pending(&conversations[0]).is_empty(), "the call was voided");
    assert_eq!(host.runs.load(std::sync::atomic::Ordering::SeqCst), 0, "no tool ran");
    assert!(common::wait_until(Duration::from_secs(5), || rig.service.active_turns() == 0 && rig.chats() == 0));
    assert!(rig.pages.conversations().is_empty(), "no approval card can exist for an ask");
}

#[test]
fn the_sizes_and_the_waiting_list_are_bounded() {
    let rig = Rig::new(Locality::Cloud, &["unused"]);
    let big_prompt = "a".repeat(16_385);
    assert_eq!(reason(rig.start(1, &big_prompt, "").end()), "too_large");
    let big_input = "b".repeat(1_048_577);
    assert_eq!(reason(rig.start(1, "hello", &big_input).end()), "too_large");
    assert_eq!(reason(rig.start(1, "he\0llo", "").end()), "not_text");
    assert_eq!(reason(rig.start(1, "hello", "te\0xt").end()), "not_text");
    assert_eq!(reason(rig.start(1, "  \n ", "").end()), "empty");
    assert_eq!(reason(rig.start(1, "", "\u{1b}[31m\u{202e}\n").end()), "empty");
    assert_eq!(reason(rig.start(1, "\u{e0041}", "   \n").end()), "empty");
    assert!(rig.notes.all().is_empty() && rig.provider.calls() == 0);
    assert_eq!(rig.chats(), 0);

    // The limits themselves are allowed.
    let edge = rig.start(1, &"a".repeat(16_384), "");
    let view = rig.notes.wait_waiting().expect("a prompt of 16384 bytes is accepted");
    assert_eq!(view.prompt.len(), 16_384);
    edge.cancel.cancel();
    assert_eq!(reason(ends_within(edge, Duration::from_secs(2))), "gone");

    // One place on the list: a second concurrent ask is refused, the first keeps waiting.
    let limits = AskLimits { waiting: 1, ..AskLimits::default() };
    let rig = Setup::new(Locality::Cloud).replies(&["unused"]).limits(limits).rig();
    let first = rig.start(3, "one", "");
    rig.notes.wait_waiting().expect("the first waits");
    let second = rig.start(4, "two", "");
    assert_eq!(reason(ends_within(second, Duration::from_secs(2))), "too_many_waiting");
    assert_eq!(rig.asks.pending(&|_| true).len(), 1, "the first still waits");
    assert!(!first.is_finished());
    first.cancel.cancel();
    assert_eq!(reason(ends_within(first, Duration::from_secs(2))), "gone");
}

#[test]
fn pending_lists_the_waiting_asks_of_the_wanted_panes_only() {
    let rig = Rig::new(Locality::Cloud, &["fine"]);
    let mut runs = Vec::new();
    for (count, (pane, prompt)) in [(3, "first"), (4, "second"), (3, "third")].into_iter().enumerate() {
        runs.push(rig.start(pane, prompt, ""));
        assert!(common::wait_until(Duration::from_secs(5), || rig.notes.waiting_views().len() == count + 1));
    }
    let prompts = |views: Vec<AskView>| -> Vec<String> { views.into_iter().map(|view| view.prompt).collect() };
    assert_eq!(prompts(rig.asks.pending(&|pane| pane == 3)), ["first", "third"], "arrival order, wanted panes only");
    assert_eq!(prompts(rig.asks.pending(&|_| true)), ["first", "second", "third"]);
    assert!(rig.asks.pending(&|pane| pane == 9).is_empty());

    // An answered ask is not pending any more.
    let views = rig.notes.waiting_views();
    rig.asks.settle(&views[0].ask, AskVerdict::Refuse, &views[0].seal).expect("refused");
    assert_eq!(prompts(rig.asks.pending(&|pane| pane == 3)), ["third"]);
    for run in runs {
        run.cancel.cancel();
        let _ = ends_within(run, Duration::from_secs(2));
    }
    assert!(rig.asks.pending(&|_| true).is_empty());
}

#[test]
fn no_type_of_the_desk_prints_text_in_debug_and_the_view_has_the_pinned_shape() {
    let rig = Rig::new(Locality::Cloud, &["fine"]);
    let prompt = format!("question {TEXT_CANARY}");
    let piped = format!("data {TEXT_CANARY}\n");
    let run = rig.start(3, &prompt, &piped);
    let view = rig.notes.wait_waiting().expect("a card waits");

    // Debug output: kinds, ids and sizes, never the prompt or the piped text. The prompt is the one text in the view and
    // it is not printed.
    let input = AskInput { pane: 3, prompt: prompt.clone(), input: piped.clone() };
    let event = AskEvent::Waiting { view: view.clone() };
    let closed = AskEvent::Closed { ask: view.ask.clone(), why: ClosedWhy::TimedOut };
    let faults = [
        AskFault::NoProvider,
        AskFault::Refused,
        AskFault::Turn { end: "failed".to_owned(), text: TEXT_CANARY.to_owned() },
        AskFault::Context(holzwave_core::ai::service::ContextRefusal::LocalOnly),
        AskFault::Service(holzwave_core::ai::service::ServiceError::Busy),
    ];
    let mut printed = vec![
        format!("{input:?}"),
        format!("{view:?}"),
        format!("{:?}", view.input),
        format!("{event:?}"),
        format!("{closed:?}"),
        format!("{:?}", rig.asks),
        format!("{:#?}", rig.asks),
        format!("{:?}", AskLimits::default()),
    ];
    printed.extend(faults.iter().map(|fault| format!("{fault:?}")));
    for text in &printed {
        assert!(!text.contains(TEXT_CANARY), "a Debug text holds typed text: {text}");
    }
    // A failed turn's own sentence is shown by Display (it is the error text of the provider), never by Debug.
    assert_eq!(faults[2].to_string(), TEXT_CANARY);

    // The pinned key order of the view and of the two events.
    let json = serde_json::to_string(&view).expect("serialises");
    let order = ["ask", "seal", "pane", "prompt", "prompt_findings", "input", "profile", "model", "host", "locality"];
    let positions: Vec<usize> =
        order.iter().map(|key| json.find(&format!("\"{key}\":")).unwrap_or_else(|| panic!("{key} is missing"))).collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "the keys are out of order: {json}");
    let waiting = serde_json::to_string(&event).expect("serialises");
    assert!(waiting.starts_with("{\"kind\":\"waiting\",\"view\":{\"ask\":"), "{waiting}");
    let closing = serde_json::to_string(&closed).expect("serialises");
    assert_eq!(closing, format!("{{\"kind\":\"closed\",\"ask\":\"{}\",\"why\":\"timed_out\"}}", view.ask));
    let counts = serde_json::to_string(view.input.as_ref().expect("counts")).expect("serialises");
    assert!(counts.starts_with("{\"lines\":1,\"bytes\":") && counts.contains("\"sent_bytes\":"), "{counts}");

    // The six closed names and the verdict names.
    let names: Vec<&str> = [
        ClosedWhy::Sent,
        ClosedWhy::Refused,
        ClosedWhy::Gone,
        ClosedWhy::TimedOut,
        ClosedWhy::PaneClosed,
        ClosedWhy::ShuttingDown,
    ]
    .iter()
    .map(|why| why.as_str())
    .collect();
    assert_eq!(names, ["sent", "refused", "gone", "timed_out", "pane_closed", "shutting_down"]);
    assert_eq!(AskVerdict::parse("send"), Some(AskVerdict::Send));
    assert_eq!(AskVerdict::parse("refuse"), Some(AskVerdict::Refuse));
    assert_eq!(AskVerdict::parse("Send"), None);
    assert_eq!(AskVerdict::parse(""), None);

    // The fixed phrases have no control character and no full stop at the end.
    for (name, phrase) in holzwave_core::ai::service::ask::FIXED {
        assert!(!phrase.chars().any(char::is_control) && !phrase.ends_with('.'), "{name}: {phrase}");
        assert!(phrase.is_ascii(), "{name} is not ASCII");
    }
    run.cancel.cancel();
    let _ = ends_within(run, Duration::from_secs(2));
}

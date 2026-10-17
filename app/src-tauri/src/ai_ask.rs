//! The shell side of the ask desk: a request from `wsh ai` becomes a card in the window, and the webview command is the
//! only way to release it.
//!
//! The core's desk ([`holzwave_core::ai::service::Asks`]) holds every rule: the route, the seal, the waits, the limits,
//! the cleaning. This file is glue. [`ShellAskNotify`] tells the window of the pane that asked, [`IpcOut`] writes the
//! answer to the socket's stream, and [`AppCore::ai_ask`] hands the request of the socket to the desk. The answer of the
//! person comes in through [`AppCore::ai_ask_answer`], which only the webview command calls: the socket has no command,
//! field or flag that reaches it, so a process in a pane can ask and can never answer.
//!
//! Nothing here logs, prints or stores the prompt or the piped text.

use std::io;
use std::sync::{Arc, OnceLock, Weak};

use holzwave_core::ai::provider::CancelToken;
use holzwave_core::ai::service::{AskEvent, AskFault, AskInput, AskNotify, AskOut, AskVerdict};
use holzwave_core::layout::PaneId;

use crate::ai_host::AiFault;
use crate::state::{AppCore, Out, Sink, Target};

/// Tells the window of a pane that a request waits or is over. It holds the sink of the shell and a weak reference to
/// the core (set once, to find the window of a pane), so it keeps the core alive for nothing.
pub struct ShellAskNotify {
    sink: Arc<dyn Sink>,
    core: OnceLock<Weak<AppCore>>,
}

impl ShellAskNotify {
    /// A notifier over the shell's sink. It sends nothing until it is bound.
    pub fn new(sink: Arc<dyn Sink>) -> ShellAskNotify {
        ShellAskNotify { sink, core: OnceLock::new() }
    }

    /// Gives the notifier the core that knows which window has a pane. Set once; a later call changes nothing.
    pub fn bind(&self, core: Weak<AppCore>) {
        let _ = self.core.set(core);
    }
}

impl AskNotify for ShellAskNotify {
    fn notify(&self, pane: PaneId, event: &AskEvent) {
        match event {
            // A card is drawn in the window that has the pane. A pane that no window has is not asked about.
            AskEvent::Waiting { .. } => {
                let label = self.core.get().and_then(Weak::upgrade).and_then(|core| core.pane_window(pane));
                if let Some(label) = label {
                    self.sink.send(Target::window(&label), Out::AiAsk(event.clone()));
                }
            }
            // A card goes in every window: a page that holds one may be another window than the pane's now.
            AskEvent::Closed { .. } => self.sink.send(Target::All, Out::AiAsk(event.clone())),
        }
    }
}

/// The socket's sink as the desk's output: text for the standard output of `wsh`, notes for its standard error.
pub struct IpcOut<'a>(pub &'a mut dyn holzwave_ipc::Sink);

impl AskOut for IpcOut<'_> {
    fn text(&mut self, text: &str) -> io::Result<()> {
        self.0.text(text)
    }

    fn note(&mut self, text: &str) -> io::Result<()> {
        self.0.note(text)
    }
}

impl AppCore {
    /// A request of `wsh ai` from `pane`: the prompt and the piped text. It blocks the thread of the stream until the
    /// ask ends, streams the answer to `out` and ends with the fixed sentence of the fault when there is none. When the
    /// client leaves, `gone` cancels the ask at once.
    pub fn ai_ask(
        &self,
        pane: Option<u64>,
        prompt: String,
        input: String,
        gone: &holzwave_ipc::Gone,
        out: &mut dyn holzwave_ipc::Sink,
    ) -> Result<(), String> {
        let pane = pane.ok_or("HOLZWAVE_PANE is not set")?;
        if !self.pane_is_open(pane) {
            return Err("this pane is not open any more".into());
        }
        let token = CancelToken::new();
        let hook = token.clone();
        gone.on_gone(move || hook.cancel());
        let request = AskInput { pane, prompt, input };
        self.ai().asks().run(self.ai().service(), request, &mut IpcOut(out), &token).map_err(|fault| fault.to_string())
    }

    /// The person's press on a card: the ask, `send` or `refuse`, and the seal the card carried. Only the webview
    /// command calls this. A refusal is the fault `ask` with the reason `unknown_ask`, `seal_mismatch` or
    /// `bad_verdict`; a seal that does not match leaves the card waiting.
    pub fn ai_ask_answer(&self, ask: &str, verdict: &str, seal: &str) -> Result<(), AiFault> {
        let verdict = AskVerdict::parse(verdict).ok_or_else(|| AiFault::from_ask(&AskFault::BadVerdict))?;
        self.ai().asks().settle(ask, verdict, seal).map_err(|fault| AiFault::from_ask(&fault))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use holzwave_core::ai::profiles::KeyVault;
    use holzwave_core::ai::scripted::{Script, ScriptedProvider};
    use holzwave_core::ai::service::ClosedWhy;
    use holzwave_core::ai::types::{Capabilities, Locality};
    use holzwave_core::layout::MAIN_WINDOW;
    use holzwave_core::settings::{AiLocalContext, Settings};
    use holzwave_ipc::{Cmd, Event, Request};

    use crate::ai_host::AiParts;
    use crate::state::ShellSpec;
    use crate::windows::{CursorSpot, Place};

    /// A scratch data directory, removed at the end of the test.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("holzwave-ask-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch dir");
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A socket folder with a short name: the path of a Unix socket is limited to about a hundred characters.
    struct SocketDir(PathBuf);

    impl SocketDir {
        fn new() -> SocketDir {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from("/tmp").join(format!("hwa-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            SocketDir(path)
        }
    }

    impl Drop for SocketDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Keeps every event the shell sends, with its target, in order.
    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<(Target, Out)>>,
    }

    impl Sink for Recorder {
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

    impl Recorder {
        /// The events of `wsh ai` requests with their targets.
        fn asks(&self) -> Vec<(Target, AskEvent)> {
            self.log
                .lock()
                .expect("log")
                .iter()
                .filter_map(|(target, out)| match out {
                    Out::AiAsk(event) => Some((target.clone(), event.clone())),
                    _ => None,
                })
                .collect()
        }

        /// Every event the page could see for a chat turn.
        fn chat_events(&self) -> usize {
            self.log.lock().expect("log").iter().filter(|(_, out)| matches!(out, Out::Ai(_))).count()
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

    /// A core over `dir` with the recording sink and no pane started.
    fn core_with(dir: &std::path::Path) -> (Arc<AppCore>, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        let core = AppCore::new_with_ai(
            dir.to_path_buf(),
            rec.clone(),
            ShellSpec::new("/bin/cat", &[]),
            AiParts { vault: KeyVault::unavailable(), clock: Some(fixed_clock), protected: Vec::new() },
        );
        (core, rec)
    }

    /// What the real socket told the client of the request.
    #[derive(Default)]
    struct Heard {
        events: Mutex<Vec<Event>>,
        end: Mutex<Option<holzwave_ipc::Outcome>>,
    }

    impl Heard {
        fn events(&self) -> Vec<Event> {
            self.events.lock().expect("events").clone()
        }

        fn end(&self) -> Option<holzwave_ipc::Outcome> {
            self.end.lock().expect("end").clone()
        }

        fn text(&self) -> String {
            self.events()
                .iter()
                .filter_map(|event| match event {
                    Event::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect()
        }
    }

    /// A `wsh ai` request over a real socket in a folder with a short name, served by the app's own handler.
    struct Stream {
        heard: Arc<Heard>,
        client: std::thread::JoinHandle<()>,
        _server: holzwave_ipc::Server,
        _socket: SocketDir,
    }

    impl Stream {
        fn open(core: &Arc<AppCore>, pane: u64, prompt: &str, piped: &str) -> Stream {
            let socket = SocketDir::new();
            let token = holzwave_ipc::new_token().expect("a token");
            let handler = Arc::new(crate::IpcHandler(core.clone()));
            let server = holzwave_ipc::serve_with(&socket.0, token.clone(), handler, holzwave_ipc::Limits::default())
                .expect("the socket is served");
            let heard = Arc::new(Heard::default());
            let request = Request {
                token,
                pane: Some(pane),
                cmd: Cmd::Ai { prompt: prompt.to_owned(), stdin_bytes: piped.len() as u32 },
            };
            let (dir, shared, body) = (socket.0.clone(), heard.clone(), piped.to_owned());
            let client = std::thread::spawn(move || {
                let mut on_event = |event: &Event| {
                    shared.events.lock().expect("events").push(event.clone());
                    Ok(())
                };
                let outcome =
                    holzwave_ipc::stream_request(&dir, &request, body.as_bytes(), &mut on_event).expect("the stream runs");
                *shared.end.lock().expect("end") = Some(outcome);
            });
            Stream { heard, client, _server: server, _socket: socket }
        }

        /// The end of the stream, waited for for a few seconds.
        fn end(self) -> holzwave_ipc::Outcome {
            assert!(wait_for(Duration::from_secs(5), || self.heard.end().is_some()), "the stream did not end");
            self.client.join().expect("the client ends");
            self.heard.end().expect("an end")
        }
    }

    /// A cloud provider named `scripted` with one model, registered as set up.
    fn cloud_provider(core: &AppCore, turns: Vec<Script>) -> Arc<ScriptedProvider> {
        let mut provider =
            ScriptedProvider::new("scripted").with_capabilities(Capabilities::conservative(Locality::Cloud));
        for turn in turns {
            provider = provider.with_turn(turn);
        }
        let provider = Arc::new(provider);
        core.ai().register_provider("scripted", provider.clone()).expect("registered");
        core.ai().asks().add_candidate("scripted", "m");
        provider
    }

    /// The first card the window was told about, waited for.
    fn waiting_card(rec: &Recorder) -> holzwave_core::ai::service::AskView {
        assert!(wait_for(Duration::from_secs(5), || !rec.asks().is_empty()), "the window is told");
        match rec.asks().remove(0).1 {
            AskEvent::Waiting { view } => view,
            other => panic!("the first event is a waiting card, not {other:?}"),
        }
    }

    #[test]
    fn wsh_ai_waits_for_the_window_and_only_the_page_method_releases_it() {
        let scratch = Scratch::new("tracer");
        let (core, rec) = core_with(&scratch.0);
        let provider = cloud_provider(&core, vec![Script::new().text("It fails ").text("because of a typo.").usage(3, 4)]);
        let pane = core.view_in(MAIN_WINDOW).expect("main").focus;
        let piped = "a first line\nANTHROPIC_API_KEY=sk-ant-api03-zqshell3b7d19e0c4XYZ0123456789\nlast line\n";
        let stream = Stream::open(&core, pane, "why is it slow", piped);

        // The window is told, and the stream holds one note and nothing else.
        let view = waiting_card(&rec);
        assert_eq!(rec.asks()[0].0, Target::window(MAIN_WINDOW), "the card goes to the window of the pane");
        assert_eq!(view.pane, pane);
        assert_eq!(view.prompt, "why is it slow");
        let input = view.input.clone().expect("the piped text is counted");
        assert_eq!((input.lines, input.bytes), (3, piped.len() as u64));
        let wire = serde_json::to_string(&Out::AiAsk(AskEvent::Waiting { view: view.clone() })).expect("serialises");
        assert!(!wire.contains("a first line") && !wire.contains("zqshell3b7d19e0c4"), "{wire}");

        assert!(!wait_for(Duration::from_millis(600), || provider.calls() > 0), "a request left before the answer");
        assert_eq!(
            stream.heard.events(),
            [Event::Note { text: format!("waiting for your answer in holzWave (pane {pane})") }],
            "one note and nothing else"
        );
        assert!(stream.heard.end().is_none(), "the stream is still open");
        assert_eq!(rec.chat_events(), 0, "no event of the ask is a chat event");

        // A wrong seal keeps the ask; only the page method with the card's seal releases it.
        let wrong = core.ai_ask_answer(&view.ask, "send", &"0".repeat(64)).expect_err("a wrong seal is refused");
        assert_eq!((wrong.code, wrong.reason), ("ask", Some("seal_mismatch")));
        assert_eq!(provider.calls(), 0);
        core.ai_ask_answer(&view.ask, "send", &view.seal).expect("the answer is taken");

        let heard = stream.heard.clone();
        let end = stream.end();
        assert!(end.ok, "{end:?}");
        assert_eq!(heard.text(), "It fails because of a typo.");
        assert_eq!(provider.calls(), 1);

        // A second answer for the same ask is told as unknown, and the close was told to every window.
        let again = core.ai_ask_answer(&view.ask, "send", &view.seal).expect_err("a second answer is refused");
        assert_eq!((again.code, again.reason), ("ask", Some("unknown_ask")));
        let closed: Vec<(Target, AskEvent)> =
            rec.asks().into_iter().filter(|(_, event)| matches!(event, AskEvent::Closed { .. })).collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].0, Target::All);
        assert_eq!(closed[0].1, AskEvent::Closed { ask: view.ask.clone(), why: ClosedWhy::Sent });
        assert!(wait_for(Duration::from_secs(5), || core.ai().conversation_list().map_or(false, |l| l.chats.is_empty())));
    }

    #[test]
    fn a_pane_that_closes_withdraws_its_waiting_ask() {
        let scratch = Scratch::new("pane-closes");
        let (core, rec) = core_with(&scratch.0);
        let provider = cloud_provider(&core, vec![Script::new().text("never").usage(1, 1)]);
        let view = core.split_in(MAIN_WINDOW, true).expect("a split");
        let pane = view.focus;
        let stream = Stream::open(&core, pane, "hello", "some text\n");
        let card = waiting_card(&rec);
        assert_eq!(card.pane, pane);

        core.close_pane_in(MAIN_WINDOW, pane).expect("the pane closes");
        let end = stream.end();
        assert!(!end.ok);
        assert_eq!(end.message, "the pane was closed, so the request was withdrawn");
        assert!(wait_for(Duration::from_secs(5), || rec.asks().len() == 2), "the card is told away");
        let closed = rec.asks().remove(1);
        assert_eq!(closed.0, Target::All, "a card goes in every window");
        assert_eq!(closed.1, AskEvent::Closed { ask: card.ask.clone(), why: ClosedWhy::PaneClosed });
        assert_eq!(provider.calls(), 0, "nothing was sent");
        assert_eq!(
            core.ai_ask_answer(&card.ask, "send", &card.seal).expect_err("late").reason,
            Some("unknown_ask"),
            "an answer to the withdrawn card finds nothing"
        );
    }

    #[test]
    fn shutdown_ends_a_waiting_ask() {
        let scratch = Scratch::new("shutdown");
        let (core, rec) = core_with(&scratch.0);
        let provider = cloud_provider(&core, vec![Script::new().text("never").usage(1, 1)]);
        let pane = core.view_in(MAIN_WINDOW).expect("main").focus;
        let stream = Stream::open(&core, pane, "hello", "");
        let card = waiting_card(&rec);

        let started = Instant::now();
        core.shutdown();
        let end = stream.end();
        assert!(started.elapsed() < Duration::from_secs(3), "the stream joins within three seconds");
        assert!(!end.ok);
        assert_eq!(end.message, "holzWave is closing");
        assert_eq!(provider.calls(), 0);
        assert!(wait_for(Duration::from_secs(5), || rec.asks().len() == 2));
        assert_eq!(rec.asks()[1].1, AskEvent::Closed { ask: card.ask, why: ClosedWhy::ShuttingDown });
    }

    /// A sink for the stream handler without a socket.
    #[derive(Default)]
    struct Collect {
        texts: Vec<String>,
        notes: Vec<String>,
    }

    impl holzwave_ipc::Sink for Collect {
        fn text(&mut self, text: &str) -> io::Result<()> {
            self.texts.push(text.to_owned());
            Ok(())
        }

        fn note(&mut self, text: &str) -> io::Result<()> {
            self.notes.push(text.to_owned());
            Ok(())
        }
    }

    #[test]
    fn an_ask_declares_no_tool_to_the_model_while_a_chat_does() {
        let scratch = Scratch::new("no-tool");
        let (core, rec) = {
            let rec = Arc::new(Recorder::default());
            let core = AppCore::new_with_ai(
                scratch.0.clone(),
                rec.clone(),
                ShellSpec::new("/bin/sh", &["-c", "sleep 30"]),
                AiParts { vault: KeyVault::unavailable(), clock: Some(fixed_clock), protected: Vec::new() },
            );
            (core, rec)
        };
        let capabilities = Capabilities { tools: true, ..Capabilities::conservative(Locality::OnDevice) };
        let provider = Arc::new(
            ScriptedProvider::new("scripted")
                .with_capabilities(capabilities)
                .with_turn(Script::new().text("from the chat").usage(1, 1))
                .with_turn(Script::new().text("from the ask").usage(1, 1)),
        );
        core.ai().register_provider("scripted", provider.clone()).expect("registered");
        core.ai().asks().add_candidate("scripted", "scripted");
        core.ai().apply_settings(&Settings { ai_local_context: AiLocalContext::Auto, ..Settings::default() });
        let pane = core.view_in(MAIN_WINDOW).expect("main").focus;
        core.start_pane(pane, 80, 24, 0, 0).expect("the pane starts");

        // An ordinary chat on a terminal pane gets tools.
        let chat = core.ai().conversation_new().expect("a chat id");
        core.ai_send(&chat, "hello".to_owned(), pane, Some("scripted".to_owned()), None).expect("sent");
        assert!(wait_for(Duration::from_secs(5), || provider.calls() == 1), "the chat turn reaches the provider");
        assert!(wait_for(Duration::from_secs(5), || rec.chat_events() >= 3));
        let declared: Vec<String> = provider.requests()[0].tools.iter().map(|spec| spec.name.clone()).collect();
        assert!(!declared.is_empty(), "a chat on a terminal pane declares tools");

        // An ask from the same pane declares none, and its answer streams back.
        assert!(wait_for(Duration::from_secs(5), || core.ai().service().active_turns() == 0));
        let (gone, mut out) = (holzwave_ipc::Gone::new(), Collect::default());
        core.ai_ask(Some(pane), "what is this".to_owned(), String::new(), &gone, &mut out).expect("the ask is answered");
        assert_eq!(out.texts.concat(), "from the ask");
        assert!(out.notes.is_empty(), "nothing waited");
        let requests = provider.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[1].tools.is_empty(), "an ask declares no tool: {:?}", requests[1].tools);
        assert!(rec.asks().is_empty(), "a destination on the device with auto asks nothing");
    }

    #[test]
    fn a_request_without_a_pane_or_for_a_closed_pane_is_refused() {
        let scratch = Scratch::new("no-pane");
        let (core, rec) = core_with(&scratch.0);
        let provider = cloud_provider(&core, vec![Script::new().text("never").usage(1, 1)]);
        let gone = holzwave_ipc::Gone::new();
        let mut out = Collect::default();
        let none = core.ai_ask(None, "hello".to_owned(), String::new(), &gone, &mut out);
        assert_eq!(none, Err("HOLZWAVE_PANE is not set".to_owned()));
        let closed = core.ai_ask(Some(9999), "hello".to_owned(), String::new(), &gone, &mut out);
        assert_eq!(closed, Err("this pane is not open any more".to_owned()));
        assert!(out.texts.is_empty() && out.notes.is_empty());
        assert!(rec.asks().is_empty(), "no card was shown");
        assert_eq!(provider.calls(), 0);
        assert!(core.ai().conversation_list().expect("a listing").chats.is_empty(), "no conversation was made");
    }
}

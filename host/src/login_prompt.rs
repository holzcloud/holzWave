//! The questions of a keyboard-interactive login (a one-time code, a PAM prompt), raised for the person and answered by them.
//!
//! The core runs the login and asks a [`Prompter`]; the host turns each question into the event `login-prompt` and waits
//! for the page's `login_prompt_answer` or `login_prompt_cancel`, or for the time to run out (`login-prompt-end` says how it
//! ended, so every window that drew the dialog takes it away).
//!
//! What the server wrote is untrusted: the core already cleaned and bounded it, and it travels as text only. What names the
//! connection in the dialog is looked up in the person's own list by the id the core stamped into the round, never taken
//! from the server. An answer is moved from the command into the login and is kept nowhere: not in this host, not in an
//! event, not in a log, not in a `Debug` text. A question that comes with an id that is not a saved connection is not
//! shown and ends the login as cancelled.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use holzwave_core::ssh::{PROMPT_LINES, PROMPT_WAIT, PromptAnswer, PromptRound, Prompter};
use serde::Serialize;

use crate::state::{Out, Sink, Target};

/// The most questions that wait for an answer at once: one per saved connection that is logging in, and the pool holds
/// that many connections at most. A round that arrives when the table is full is cancelled, so a flood cannot fill it.
const MAX_PENDING: usize = 16;

/// The most bytes of one answer. A password or a code is short; the bound keeps a misbehaving page from sending a book.
const MAX_ANSWER: usize = 1024;

/// How a question ended, as `login-prompt-end` says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptEnd {
    Answered,
    Cancelled,
    TimedOut,
}

/// A question that waits for the person.
struct Pending {
    /// Where the answer goes. A rendezvous of capacity one: a second answer finds the entry gone.
    to: SyncSender<PromptAnswer>,
    /// How many answers the round needs.
    lines: usize,
}

/// The id, the name and the host of a saved connection, looked up by the id the core stamped into the round.
type Lookup = dyn Fn(&str) -> Option<(String, String)> + Send + Sync;

pub struct PromptHost {
    sink: Arc<dyn Sink>,
    /// The name and the host of the connection with this id, from the saved list.
    lookup: Box<Lookup>,
    wait: Duration,
    pending: Mutex<HashMap<String, Pending>>,
}

impl PromptHost {
    pub fn new(sink: Arc<dyn Sink>, lookup: Box<Lookup>) -> Arc<Self> {
        Self::with_wait(sink, lookup, PROMPT_WAIT)
    }

    /// [`PromptHost::new`] with the time a person has, so a test need not wait two minutes.
    pub fn with_wait(sink: Arc<dyn Sink>, lookup: Box<Lookup>, wait: Duration) -> Arc<Self> {
        Arc::new(Self { sink, lookup, wait, pending: Mutex::new(HashMap::new()) })
    }

    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `id` could be one this host issued: 32 lowercase hex characters.
    fn well_formed(id: &str) -> bool {
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// The person answered the question `id`. Moves the answers into the login and returns true; an id that was never
    /// issued, that ended, that is not an id, or answers of another count or an absurd size change nothing and return false.
    /// Never blocks.
    pub fn answer(&self, id: &str, answers: Vec<String>) -> bool {
        if !Self::well_formed(id) || answers.len() > PROMPT_LINES || answers.iter().any(|a| a.len() > MAX_ANSWER) {
            return false;
        }
        let mut table = self.table();
        if table.get(id).is_none_or(|p| p.lines != answers.len()) {
            return false;
        }
        let Some(pending) = table.remove(id) else { return false };
        drop(table);
        pending.to.try_send(PromptAnswer::Answers(answers)).is_ok()
    }

    /// The person gave up on the question `id`. Returns true when it was waiting; an unknown or ended id changes nothing.
    pub fn cancel(&self, id: &str) -> bool {
        if !Self::well_formed(id) {
            return false;
        }
        let Some(pending) = self.table().remove(id) else { return false };
        pending.to.try_send(PromptAnswer::Cancelled).is_ok()
    }

    /// How many questions wait now.
    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.table().len()
    }

    fn end(&self, id: String, outcome: PromptEnd) {
        self.sink.send(Target::All, Out::LoginPromptEnd { id, outcome });
    }
}

impl Prompter for PromptHost {
    fn ask(&self, round: PromptRound) -> PromptAnswer {
        // The title is the person's own: no saved connection with this id, no dialog.
        let Some((connection, host)) = (self.lookup)(&round.connection) else { return PromptAnswer::Cancelled };
        let Ok(id) = holzwave_ipc::new_token() else { return PromptAnswer::Cancelled };
        let (to, from) = mpsc::sync_channel(1);
        {
            let mut table = self.table();
            if table.len() >= MAX_PENDING {
                return PromptAnswer::Cancelled;
            }
            table.insert(id.clone(), Pending { to, lines: round.prompts.len() });
        }
        self.sink.send(
            Target::All,
            Out::LoginPrompt {
                id: id.clone(),
                connection,
                host,
                name: round.name,
                instructions: round.instructions,
                prompts: round.prompts,
            },
        );
        let (answer, outcome) = match from.recv_timeout(self.wait) {
            Ok(PromptAnswer::Answers(answers)) => (PromptAnswer::Answers(answers), PromptEnd::Answered),
            Ok(PromptAnswer::Cancelled) | Err(RecvTimeoutError::Disconnected) => (PromptAnswer::Cancelled, PromptEnd::Cancelled),
            Err(RecvTimeoutError::Timeout) => {
                // An answer may have taken the entry at this very moment: it is in the channel then and is used.
                if self.table().remove(&id).is_none() {
                    match from.try_recv() {
                        Ok(PromptAnswer::Answers(answers)) => (PromptAnswer::Answers(answers), PromptEnd::Answered),
                        _ => (PromptAnswer::Cancelled, PromptEnd::TimedOut),
                    }
                } else {
                    (PromptAnswer::Cancelled, PromptEnd::TimedOut)
                }
            }
        };
        // Whatever the way out, the entry is gone before anyone is told it ended.
        self.table().remove(&id);
        self.end(id, outcome);
        answer
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Instant;

    use super::*;
    use crate::windows::{CursorSpot, Place};
    use holzwave_core::ssh::PromptLine;

    /// Records what is sent.
    #[derive(Default)]
    struct Log(Mutex<Vec<(Target, Out)>>);

    impl Sink for Log {
        fn send(&self, target: Target, out: Out) {
            self.0.lock().unwrap().push((target, out));
        }
        fn close_window(&self, _label: &str) {}
        fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
            Err("no windows here".into())
        }
        fn cursor_spot(&self, _source: &str) -> CursorSpot {
            CursorSpot::Unknown
        }
    }

    impl Log {
        fn events(&self) -> Vec<(Target, Out)> {
            self.0.lock().unwrap().clone()
        }
        fn prompts(&self) -> Vec<Out> {
            self.events().into_iter().map(|(_, o)| o).filter(|o| matches!(o, Out::LoginPrompt { .. })).collect()
        }
        fn ends(&self) -> Vec<(String, PromptEnd)> {
            self.events()
                .into_iter()
                .filter_map(|(_, o)| match o {
                    Out::LoginPromptEnd { id, outcome } => Some((id, outcome)),
                    _ => None,
                })
                .collect()
        }
    }

    fn lookup() -> Box<Lookup> {
        Box::new(|id| (id == "talos").then(|| ("Talos".to_owned(), "talos.example".to_owned())))
    }

    fn round(connection: &str) -> PromptRound {
        PromptRound {
            connection: connection.into(),
            // What the server called itself must never reach the title: a different host here.
            host: "evil.example".into(),
            name: "Two-factor".into(),
            instructions: "Enter the code.".into(),
            prompts: vec![PromptLine { text: "Password:".into(), echo: false }, PromptLine { text: "Code:".into(), echo: true }],
        }
    }

    /// Ask from a thread, and wait until the question is on the wire; returns the thread's result and the id.
    fn ask_in_thread(host: &Arc<PromptHost>, log: &Arc<Log>, connection: &str) -> (thread::JoinHandle<PromptAnswer>, String) {
        let (h, c) = (host.clone(), connection.to_owned());
        let before = log.prompts().len();
        let handle = thread::spawn(move || h.ask(round(&c)));
        let started = Instant::now();
        loop {
            if log.prompts().len() > before {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "the question never arrived");
            thread::sleep(Duration::from_millis(2));
        }
        let Some(Out::LoginPrompt { id, .. }) = log.prompts().into_iter().last() else { unreachable!() };
        (handle, id)
    }

    fn host(wait: Duration) -> (Arc<PromptHost>, Arc<Log>) {
        let log = Arc::new(Log::default());
        (PromptHost::with_wait(log.clone(), lookup(), wait), log)
    }

    #[test]
    fn a_prompt_becomes_one_event_and_an_answer_completes_it() {
        let (host, log) = host(Duration::from_secs(30));
        let (asking, id) = ask_in_thread(&host, &log, "talos");

        let sent = log.events();
        assert_eq!(sent.len(), 1, "one event");
        assert_eq!(sent[0].0, Target::All, "every window");
        let Out::LoginPrompt { id: sent_id, connection, host: shown_host, name, instructions, prompts } = &sent[0].1 else {
            panic!("not a prompt: {:?}", sent[0].1)
        };
        assert_eq!(sent_id.len(), 32);
        assert!(sent_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!((connection.as_str(), shown_host.as_str()), ("Talos", "talos.example"), "from the saved connection, not the server");
        assert_eq!((name.as_str(), instructions.as_str()), ("Two-factor", "Enter the code."));
        assert_eq!(prompts.iter().map(|p| (p.text.as_str(), p.echo)).collect::<Vec<_>>(), [("Password:", false), ("Code:", true)]);
        assert_eq!(host.waiting(), 1);

        assert!(host.answer(&id, vec!["pw".into(), "123456".into()]));
        let PromptAnswer::Answers(answers) = asking.join().unwrap() else { panic!("not answered") };
        assert_eq!(answers, ["pw", "123456"]);
        assert_eq!(log.ends(), [(id.clone(), PromptEnd::Answered)]);
        assert_eq!(host.waiting(), 0, "the entry is gone");
        assert!(!host.answer(&id, vec!["pw".into(), "123456".into()]), "a second answer for the same id");
        assert!(!host.cancel(&id));
    }

    #[test]
    fn cancel_and_timeout_end_the_prompt() {
        let (host, log) = host(Duration::from_secs(30));
        let (asking, id) = ask_in_thread(&host, &log, "talos");
        assert!(host.cancel(&id));
        assert!(matches!(asking.join().unwrap(), PromptAnswer::Cancelled));
        assert_eq!(log.ends(), [(id, PromptEnd::Cancelled)]);
        assert_eq!(host.waiting(), 0);

        // No answer in the (shortened) time.
        let (host, log) = host_with_short_wait();
        let started = Instant::now();
        let answer = host.ask(round("talos"));
        assert!(matches!(answer, PromptAnswer::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(5));
        let ends = log.ends();
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0].1, PromptEnd::TimedOut);
        assert_eq!(host.waiting(), 0);
        assert!(!host.answer(&ends[0].0, vec!["a".into(), "b".into()]), "too late");
    }

    fn host_with_short_wait() -> (Arc<PromptHost>, Arc<Log>) {
        host(Duration::from_millis(150))
    }

    #[test]
    fn an_unknown_or_old_id_changes_nothing() {
        let (host, log) = host(Duration::from_secs(30));
        let two = || vec!["a".to_owned(), "b".to_owned()];
        for bad in ["", "nope", &"0".repeat(32), &"g".repeat(32), &"A".repeat(32), &"0".repeat(33)] {
            assert!(!host.answer(bad, two()), "{bad:?}");
            assert!(!host.cancel(bad), "{bad:?}");
        }
        assert!(log.events().is_empty(), "no event for an id nobody was asked about");

        // A question that waits is not touched by wrong calls: another id, the wrong count, an absurd answer.
        let (asking, id) = ask_in_thread(&host, &log, "talos");
        assert!(!host.answer(&"1".repeat(32), two()));
        assert!(!host.answer(&id, vec!["only one".into()]));
        assert!(!host.answer(&id, vec!["x".repeat(MAX_ANSWER + 1), "b".into()]));
        assert!(!host.answer(&id, vec!["a".into(); PROMPT_LINES + 1]));
        assert_eq!(host.waiting(), 1, "still waiting");
        assert!(host.answer(&id, two()));
        assert!(matches!(asking.join().unwrap(), PromptAnswer::Answers(_)));
        assert!(!host.answer(&id, two()), "an old id");
    }

    #[test]
    fn a_connection_that_is_not_saved_is_never_asked_about() {
        let (host, log) = host(Duration::from_secs(30));
        // What the server calls itself is not a saved connection's id.
        let answer = host.ask(round("evil.example"));
        assert!(matches!(answer, PromptAnswer::Cancelled));
        let answer = host.ask(round(""));
        assert!(matches!(answer, PromptAnswer::Cancelled));
        assert!(log.events().is_empty(), "no dialog without a saved connection");
        assert_eq!(host.waiting(), 0);
    }

    #[test]
    fn answers_are_never_kept() {
        let (host, log) = host(Duration::from_secs(30));
        let (asking, id) = ask_in_thread(&host, &log, "talos");
        let secret = "s3cr3t-4711";
        assert!(host.answer(&id, vec![secret.into(), "424242".into()]));
        let _ = asking.join().unwrap();
        assert_eq!(host.waiting(), 0, "the table that held the question is empty");
        // The events, the table and the host's Debug-able parts hold no answer.
        let everything = format!("{:?}", log.events());
        assert!(!everything.contains(secret) && !everything.contains("424242"), "{everything}");
        let json = serde_json::to_string(&log.events().iter().map(|(_, o)| o).collect::<Vec<_>>()).unwrap();
        assert!(!json.contains(secret) && !json.contains("424242"), "{json}");
        assert_eq!(format!("{:?}", PromptAnswer::Answers(vec![secret.into()])), "Answers(<1 hidden>)");
    }

    #[test]
    fn a_flood_of_questions_cannot_fill_the_table() {
        let (host, log) = host(Duration::from_secs(30));
        let count = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..MAX_PENDING {
            let (h, c) = (host.clone(), count.clone());
            threads.push(thread::spawn(move || {
                let answer = h.ask(round("talos"));
                c.fetch_add(1, Ordering::SeqCst);
                answer
            }));
        }
        let started = Instant::now();
        while log.prompts().len() < MAX_PENDING {
            assert!(started.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(2));
        }
        // The table is full: the next one is cancelled at once and raises nothing.
        assert!(matches!(host.ask(round("talos")), PromptAnswer::Cancelled));
        assert_eq!(log.prompts().len(), MAX_PENDING);
        for (_, out) in log.events() {
            if let Out::LoginPrompt { id, .. } = out {
                assert!(host.cancel(&id));
            }
        }
        for t in threads {
            assert!(matches!(t.join().unwrap(), PromptAnswer::Cancelled));
        }
        assert_eq!(host.waiting(), 0);
    }
}

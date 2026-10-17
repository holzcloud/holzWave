//! Keyboard-interactive login (a password prompt of PAM, a one-time code, a hardware token's challenge), against the
//! in-process SSH server of `common::ssh_server`, which asks rounds of questions and checks the answers.
//!
//! A real `sshd` on loopback cannot offer this method without PAM, so the protocol path is proven here and the first run
//! against a host with a real one-time code is a row of `docs/test-plan.md`.
//!
//! The person is a [`Scripted`] prompter: it records what it was asked and gives canned answers, so no test types
//! anything. What a server says is untrusted text; the tests that look at messages check that no answer and no question
//! text is part of one.

mod common;

use std::collections::VecDeque;
use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use common::ssh_server::{self, Options, Out, Server, round};
use holzwave_core::conn::{ConnState, Link};
use holzwave_core::connections::{Connections, KNOWN_HOSTS_FILE, SecretChange};
use holzwave_core::files::{FileSource, SftpSource};
use holzwave_core::pool::{Connection, Open, OpenError};
use holzwave_core::profiles::{Auth, Profile, Profiles};
use holzwave_core::secrets::SecretStore;
use holzwave_core::ssh::{
    ExecCancel, ExecLimits, PROMPT_LINES, PROMPT_ROUNDS, PromptAnswer, PromptRound, Prompter, SshAuth, SshExec, SshLink,
};

const ASKS_FOR_A_CODE: &str = "this connection asks for a login code; open a pane on it first";

/// A person who answers from a script and writes down every question.
struct Scripted {
    rounds: Mutex<Vec<PromptRound>>,
    answers: Mutex<VecDeque<PromptAnswer>>,
    calls: AtomicUsize,
    /// How long the person thinks before answering.
    think: Duration,
}

impl Scripted {
    fn new(answers: Vec<PromptAnswer>) -> Arc<Self> {
        Self::slow(Duration::ZERO, answers)
    }

    fn slow(think: Duration, answers: Vec<PromptAnswer>) -> Arc<Self> {
        Arc::new(Self { rounds: Mutex::new(Vec::new()), answers: Mutex::new(answers.into()), calls: AtomicUsize::new(0), think })
    }

    fn typing(answers: &[&[&str]]) -> Arc<Self> {
        Self::new(answers.iter().map(|round| said(round)).collect())
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn rounds(&self) -> Vec<PromptRound> {
        self.rounds.lock().unwrap().clone()
    }

    fn auth(self: &Arc<Self>) -> SshAuth {
        SshAuth::Interactive { prompter: Some(self.clone()) }
    }
}

impl Prompter for Scripted {
    fn ask(&self, round: PromptRound) -> PromptAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.rounds.lock().unwrap().push(round);
        thread::sleep(self.think);
        self.answers.lock().unwrap().pop_front().unwrap_or(PromptAnswer::Cancelled)
    }
}

fn said(answers: &[&str]) -> PromptAnswer {
    PromptAnswer::Answers(answers.iter().map(|a| (*a).to_owned()).collect())
}

/// A person who waits until the test lets them answer, and says when the question arrived.
struct Held {
    asked: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
    calls: AtomicUsize,
    answers: Vec<String>,
}

impl Prompter for Held {
    fn ask(&self, _round: PromptRound) -> PromptAnswer {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let _ = self.asked.lock().unwrap().send(());
        let _ = self.release.lock().unwrap().recv_timeout(Duration::from_secs(30));
        PromptAnswer::Answers(self.answers.clone())
    }
}

fn held(answers: &[&str]) -> (Arc<Held>, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (asked, arrived) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let prompter = Arc::new(Held {
        asked: Mutex::new(asked),
        release: Mutex::new(wait),
        calls: AtomicUsize::new(0),
        answers: answers.iter().map(|a| (*a).to_owned()).collect(),
    });
    (prompter, arrived, release)
}

/// A store that must never be read: an interactive login has no saved secret.
struct NoKeychain;

impl SecretStore for NoKeychain {
    fn get(&self, account: &str) -> holzwave_core::Result<Option<String>> {
        panic!("the keychain was read for {account}");
    }
    fn set(&self, account: &str, _: &str) -> holzwave_core::Result<()> {
        panic!("the keychain was written for {account}");
    }
    fn delete(&self, _: &str) -> holzwave_core::Result<()> {
        Ok(())
    }
}

/// A private folder that is removed when the test ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(common::scratch_dir(tag))
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The one round of the plan's first test: a password and a code.
fn code_server() -> Server {
    ssh_server::start(Options::keyboard_interactive(vec![round(
        "Login",
        "Enter your password and the code of your token.",
        &[("Password: ", false), ("Code: ", true)],
        &["pw", "123456"],
    )]))
}

/// Connect a pane with `auth`; its output is read by a helper thread.
fn pane(server: &Server, auth: SshAuth) -> (SshLink, Out) {
    let mut link = SshLink::connect(server.target(auth), 80, 24).unwrap();
    let out = Out::new(link.take_reader().unwrap());
    (link, out)
}

fn count(server: &Server, counter: impl Fn(&ssh_server::Seen) -> &AtomicUsize) -> usize {
    counter(&server.seen).load(Ordering::SeqCst)
}

/// A saved interactive connection `talos` on `server`, in a keychain that panics on any access, with `prompter` set.
fn saved(dir: &Scratch, server: &Server, prompter: Option<Arc<dyn Prompter>>) -> Connections {
    server.trust(&dir.path().join(KNOWN_HOSTS_FILE));
    let (connections, _) = Connections::load(dir.path(), Arc::new(NoKeychain), None, &[]);
    let mut profile = Profile::new("talos", "Talos", "127.0.0.1");
    profile.port = server.addr.port();
    profile.user = "holz".into();
    profile.auth = Auth::Interactive;
    connections.save(profile, SecretChange::Keep).unwrap();
    if let Some(prompter) = prompter {
        connections.set_prompter(prompter);
    }
    connections
}

#[test]
fn a_one_time_code_logs_in_to_a_pane() {
    let server = code_server();
    let person = Scripted::typing(&[&["pw", "123456"]]);

    let (mut link, mut out) = pane(&server, person.auth());
    let mut writer = link.take_writer().unwrap();
    out.until("welcome 0", 10);
    assert_eq!(link.state(), ConnState::Connected);
    writer.write_all(b"hi").unwrap();
    out.until("echo:hi", 10);

    assert_eq!(person.calls(), 1, "the person was asked once");
    let asked = person.rounds();
    assert_eq!(asked[0].prompts.len(), 2);
    assert_eq!(asked[0].prompts[0].text, "Password:");
    assert!(!asked[0].prompts[0].echo);
    assert_eq!(asked[0].prompts[1].text, "Code:");
    assert!(asked[0].prompts[1].echo);
    assert_eq!(asked[0].name, "Login");
    assert_eq!(asked[0].host, "127.0.0.1");
    assert_eq!(count(&server, |s| &s.logins), 1, "one login");
    assert_eq!(count(&server, |s| &s.refused_logins), 0);
    assert_eq!(count(&server, |s| &s.kbd_rounds), 1);
    Box::new(link).close().unwrap();
}

#[test]
fn an_interactive_login_is_the_one_login_of_the_pool() {
    let dir = Scratch::new("kip");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    // The server speaks canonical paths (on a Mac the temporary folder is behind a link).
    let root = root.canonicalize().unwrap();
    std::fs::write(root.join("a.txt"), "alpha").unwrap();
    let server = ssh_server::start(
        Options::keyboard_interactive(vec![round("", "", &[("Code:", false)], &["123456"])]).sftp(root.clone()),
    );
    let person = Scripted::typing(&[&["123456"]]);
    let connections = saved(&dir, &server, Some(person.clone()));
    let connection = connections.session("talos").unwrap();

    let mut link = SshLink::attach(connection.clone(), 80, 24).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    out.until("welcome 0", 10);
    let names: Vec<String> =
        SftpSource::on(connection.clone()).list(root.to_str().unwrap()).unwrap().entries.into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["a.txt"]);
    let result = SshExec::on(connection.clone()).unwrap().exec("echo ok", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.stdout, b"ran:echo ok\r\n");

    assert_eq!(count(&server, |s| &s.connections), 1, "one TCP connection");
    assert_eq!(count(&server, |s| &s.logins), 1, "one login for the pane, the files and the command");
    assert_eq!(count(&server, |s| &s.sftp_sessions), 1);
    assert_eq!(person.calls(), 1, "a person answers once");
    // The prompt was raised for the saved connection, by its id.
    assert_eq!(person.rounds()[0].connection, "talos");
    let _ = link.take_writer();
    Box::new(link).close().unwrap();
}

#[test]
fn a_background_opener_never_asks() {
    let dir = Scratch::new("kbg");
    let server = ssh_server::start(Options::keyboard_interactive(vec![round("", "", &[("Code:", false)], &["123456"])]));
    let person = Scripted::typing(&[&["123456"]]);
    let connections = saved(&dir, &server, Some(person.clone()));
    let connection = connections.session("talos").unwrap();

    // The command runner, the monitor and the agent open pinned: with no session yet they fail at once.
    let started = Instant::now();
    let error: OpenError = connection.block_on(connection.session(Open::Pinned)).unwrap_err();
    assert_eq!(error, OpenError { fatal: false, why: ASKS_FOR_A_CODE.into() });
    let ran = SshExec::on(connection.clone()).unwrap().exec("echo ok", &ExecLimits::default(), &ExecCancel::new());
    let text = ran.unwrap_err().to_string();
    assert!(text.contains(ASKS_FOR_A_CODE), "{text}");
    assert!(started.elapsed() < Duration::from_secs(2), "refused at once, took {:?}", started.elapsed());
    assert_eq!(person.calls(), 0, "a background opener never asks");
    assert_eq!(count(&server, |s| &s.connections), 0, "nothing connected");
    assert_eq!(count(&server, |s| &s.logins), 0);
    assert!(connection.cooldown_left(Instant::now()).is_none(), "the sentence is no failure of the login");

    // A person's pane logs in; afterwards the same runner works through the shared session.
    let mut link = SshLink::attach(connection.clone(), 80, 24).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    out.until("welcome 0", 10);
    let result = SshExec::on(connection.clone()).unwrap().exec("echo ok", &ExecLimits::default(), &ExecCancel::new()).unwrap();
    assert_eq!(result.stdout, b"ran:echo ok\r\n");
    assert_eq!(person.calls(), 1);
    assert_eq!(count(&server, |s| &s.logins), 1);
    let _ = link.take_writer();
    Box::new(link).close().unwrap();
}

#[test]
fn a_slow_answer_is_not_cut_by_the_login_bound() {
    // A person typing a one-time code takes longer than the bound of an ordinary login.
    let server = code_server();
    let person = Scripted::slow(Duration::from_millis(900), vec![said(&["pw", "123456"])]);
    let connection = Connection::standalone("slow", server.target(person.auth())).unwrap();
    connection.set_login_bound(Duration::from_millis(300));
    let session = connection.block_on(connection.session(Open::Interactive));
    assert!(session.is_ok(), "{:?}", session.err());
    assert_eq!(person.calls(), 1);
    assert_eq!(count(&server, |s| &s.logins), 1);

    // The same bound still cuts a login that is not a person's: a server that never speaks.
    let silent = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mut target = server.target(SshAuth::Password("pw".into()));
    target.port = silent.local_addr().unwrap().port();
    let connection = Connection::standalone("silent", target).unwrap();
    connection.set_login_bound(Duration::from_millis(300));
    let started = Instant::now();
    let error = connection.block_on(connection.session(Open::Interactive)).unwrap_err();
    assert_eq!(error.why, "the connection timed out");
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn a_background_opener_never_waits_behind_a_prompt() {
    let dir = Scratch::new("kwb");
    let server = ssh_server::start(Options::keyboard_interactive(vec![round("", "", &[("Code:", false)], &["123456"])]));
    let (person, arrived, release) = held(&["123456"]);
    let connections = saved(&dir, &server, Some(person.clone()));
    let connection = connections.session("talos").unwrap();

    // A pane's login is waiting for the person: the session slot is held.
    let mut link = SshLink::attach(connection.clone(), 80, 24).unwrap();
    let mut out = Out::new(link.take_reader().unwrap());
    arrived.recv_timeout(Duration::from_secs(10)).expect("the question arrives");

    let started = Instant::now();
    let error = connection.block_on(connection.session(Open::Pinned)).unwrap_err();
    assert_eq!(error.why, ASKS_FOR_A_CODE);
    assert!(!error.fatal);
    assert!(started.elapsed() < Duration::from_millis(900), "no waiting behind the person: {:?}", started.elapsed());
    assert_eq!(person.calls.load(Ordering::SeqCst), 1, "the person was asked once");

    release.send(()).unwrap();
    out.until("welcome 0", 10);
    assert_eq!(person.calls.load(Ordering::SeqCst), 1);
    let _ = link.take_writer();
    Box::new(link).close().unwrap();
}

#[test]
fn a_profile_with_the_interactive_login_loads_and_maps() {
    let text = r#"{"version":1,"items":[
        {"id":"talos","name":"Talos","host":"talos.example","user":"holz","auth":"interactive"},
        {"id":"old","name":"Old","host":"old.example","user":"holz","key_path":"~/.ssh/id"}]}"#;
    let (profiles, status) = Profiles::from_json(text);
    assert!(matches!(status, holzwave_core::layout::LoadStatus::Clean), "{status:?}");
    assert_eq!(profiles.get("talos").unwrap().auth, Auth::Interactive);
    assert_eq!(profiles.get("talos").unwrap().problem(), None, "no key path and no secret is needed");
    assert_eq!(profiles.get("old").unwrap().auth, Auth::Key, "an old file without the value loads as before");
    let json = serde_json::to_string(profiles.get("talos").unwrap()).unwrap();
    assert!(json.contains(r#""auth":"interactive""#), "{json}");

    // The target never asks the keychain.
    let target = profiles.ssh_target("talos", &NoKeychain, None, std::path::Path::new("/nowhere/known_hosts")).unwrap();
    assert!(matches!(target.auth, SshAuth::Interactive { prompter: None }));
    assert_eq!(format!("{:?}", target.auth), "Interactive");
}

#[test]
fn closing_a_pane_while_the_question_waits_does_not_wait_for_the_person() {
    let server = code_server();
    let (person, arrived, release) = held(&["pw", "123456"]);
    let (link, _out) = {
        let mut link = SshLink::connect(server.target(SshAuth::Interactive { prompter: Some(person.clone()) }), 80, 24).unwrap();
        let out = Out::new(link.take_reader().unwrap());
        (link, out)
    };
    arrived.recv_timeout(Duration::from_secs(10)).expect("the question arrives");
    let started = Instant::now();
    Box::new(link).close().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5), "closing waited for the person: {:?}", started.elapsed());
    let _ = release.send(());
    assert_eq!(PROMPT_ROUNDS, 3);
    assert_eq!(PROMPT_LINES, 5);
}

// ---- the edges: untrusted text, bounds, rounds, echo, cancel, silence, no secret ----------------------------------

/// Wait for the pane's failure line and return it whole.
fn failure(out: &mut Out, why: &str) {
    out.until("Press Enter to try again", 10);
    let line = format!("Connection to 127.0.0.1 failed: {why}. Press Enter to try again.");
    assert!(out.seen.contains(&line), "wanted {line:?}, the pane shows {:?}", out.seen);
}

#[test]
fn server_text_reaches_the_person_cleaned_and_bounded() {
    let name = format!("{}\x1b[2J\u{202e}", "N".repeat(400));
    let instructions = format!("first\n\u{202e}second\x1b[31m\u{200b}\n{}", "x".repeat(500));
    let long = "P".repeat(500);
    let server = ssh_server::start(Options::keyboard_interactive(vec![round(
        &name,
        &instructions,
        &[(&long, false), ("", true), ("  \u{202e}Code:\x1b[0m\u{0}  ", false)],
        &["a", "b", "c"],
    )]));
    let person = Scripted::typing(&[&["a", "b", "c"]]);
    let (link, mut out) = pane(&server, person.auth());
    out.until("welcome 0", 10);

    let asked = &person.rounds()[0];
    assert_eq!(asked.name, "N".repeat(60), "a name is cut to 60 characters, controls gone");
    assert!(asked.instructions.chars().count() <= 300, "{}", asked.instructions.chars().count());
    assert!(asked.instructions.starts_with("first\nsecond [31m"), "{:?}", asked.instructions);
    assert!(!asked.instructions.chars().any(|c| c.is_control() && c != '\n'));
    assert!(!asked.instructions.contains('\u{202e}') && !asked.instructions.contains('\u{200b}'));
    assert_eq!(asked.prompts[0].text, "P".repeat(200));
    assert_eq!(asked.prompts[1].text, "Answer", "an empty question text becomes Answer");
    assert_eq!(asked.prompts[2].text, "Code: [0m", "the escape and the NUL became spaces");
    Box::new(link).close().unwrap();
}

#[test]
fn at_most_five_prompts_in_a_round_and_three_rounds() {
    let six: Vec<(&str, bool)> = vec![("1", false); PROMPT_LINES + 1];
    let server = ssh_server::start(Options::keyboard_interactive(vec![round("", "", &six, &["a"; 6])]));
    let person = Scripted::typing(&[&["a", "a", "a", "a", "a", "a"]]);
    let (link, mut out) = pane(&server, person.auth());
    failure(&mut out, "the server asked too many questions");
    assert_eq!(person.calls(), 0, "a round of six is not shown");
    assert_eq!(count(&server, |s| &s.logins), 0);
    Box::new(link).close().unwrap();

    // Three rounds with questions are fine, a fourth is not.
    let rounds = |n: usize| (0..n).map(|i| round("", "", &[("Code:", false)], &[&i.to_string()])).collect::<Vec<_>>();
    let server = ssh_server::start(Options::keyboard_interactive(rounds(PROMPT_ROUNDS)));
    let person = Scripted::typing(&[&["0"], &["1"], &["2"]]);
    let (link, mut out) = pane(&server, person.auth());
    out.until("welcome 0", 10);
    assert_eq!(person.calls(), 3);
    Box::new(link).close().unwrap();

    let server = ssh_server::start(Options::keyboard_interactive(rounds(PROMPT_ROUNDS + 1)));
    let person = Scripted::typing(&[&["0"], &["1"], &["2"], &["3"]]);
    let (link, mut out) = pane(&server, person.auth());
    failure(&mut out, "the server asked too many questions");
    assert_eq!(person.calls(), 3, "the fourth round is not shown");
    assert_eq!(count(&server, |s| &s.logins), 0);
    Box::new(link).close().unwrap();
}

#[test]
fn a_round_with_no_prompts_is_answered_without_asking() {
    let server = ssh_server::start(Options::keyboard_interactive(vec![
        round("Notice", "Welcome", &[], &[]),
        round("", "", &[("Code:", false)], &["123456"]),
    ]));
    let person = Scripted::typing(&[&["123456"]]);
    let (link, mut out) = pane(&server, person.auth());
    out.until("welcome 0", 10);
    assert_eq!(person.calls(), 1, "only the round with a question was shown");
    assert_eq!(count(&server, |s| &s.kbd_rounds), 2);
    Box::new(link).close().unwrap();
}

#[test]
fn the_echo_flag_reaches_the_prompter_unchanged() {
    let server = ssh_server::start(Options::keyboard_interactive(vec![round(
        "",
        "",
        &[("A:", true), ("B:", false), ("C:", true), ("D:", false)],
        &["1", "2", "3", "4"],
    )]));
    let person = Scripted::typing(&[&["1", "2", "3", "4"]]);
    let (link, mut out) = pane(&server, person.auth());
    out.until("welcome 0", 10);
    let flags: Vec<bool> = person.rounds()[0].prompts.iter().map(|p| p.echo).collect();
    assert_eq!(flags, [true, false, true, false]);
    Box::new(link).close().unwrap();
}

#[test]
fn a_wrong_answer_is_a_refusal_in_words() {
    let server = ssh_server::start(Options::keyboard_interactive(vec![round("", "", &[("Code:", false)], &["right"])]));
    let person = Scripted::typing(&[&["wrong"], &["right"]]);
    let (mut link, mut out) = pane(&server, person.auth());
    let mut writer = link.take_writer().unwrap();
    failure(&mut out, "holz@127.0.0.1 refused the login");
    // The pane does not ask again by itself: a person's own Enter does.
    thread::sleep(Duration::from_millis(500));
    assert_eq!(person.calls(), 1);
    assert_eq!(count(&server, |s| &s.kbd_refused), 1);
    assert_eq!(link.state(), ConnState::Failed);
    writer.write_all(b"\r").unwrap();
    // The second connection of the server: the first one never logged in.
    out.until("welcome 1", 10);
    assert_eq!(person.calls(), 2);
    assert_eq!(count(&server, |s| &s.logins), 1);
    drop(writer);
    Box::new(link).close().unwrap();
}

#[test]
fn a_cancel_ends_the_login() {
    let server = code_server();
    let person = Scripted::new(vec![PromptAnswer::Cancelled]);
    let (link, mut out) = pane(&server, person.auth());
    failure(&mut out, "the login was cancelled");
    assert_eq!(count(&server, |s| &s.logins), 0, "no login was completed");
    thread::sleep(Duration::from_millis(400));
    assert_eq!(person.calls(), 1, "the pane waits for Enter");
    assert_eq!(link.state(), ConnState::Failed);
    Box::new(link).close().unwrap();
}

#[test]
fn a_wrong_number_of_answers_is_refused_before_it_is_sent() {
    let server = code_server();
    let person = Scripted::typing(&[&["only-one"]]);
    let (link, mut out) = pane(&server, person.auth());
    failure(&mut out, "the answers do not match the questions");
    assert_eq!(count(&server, |s| &s.kbd_rounds), 1);
    assert_eq!(count(&server, |s| &s.kbd_refused), 0, "no answer round went to the server");
    assert_eq!(count(&server, |s| &s.logins), 0);
    Box::new(link).close().unwrap();
}

#[test]
fn no_message_holds_an_answer_or_a_prompt_text() {
    let question = "Enter token XJ-49-ZZ";
    let secret = "s3cr3t-wrong-4711";
    let dir = Scratch::new("kms");
    let six: Vec<(&str, bool)> = vec![(question, false); PROMPT_LINES + 1];
    let cases: Vec<(Options, Vec<PromptAnswer>)> = vec![
        // refused
        (Options::keyboard_interactive(vec![round("", "", &[(question, false)], &["right"])]), vec![said(&[secret])]),
        // cancelled
        (Options::keyboard_interactive(vec![round("", "", &[(question, false)], &["right"])]), vec![PromptAnswer::Cancelled]),
        // too many questions
        (Options::keyboard_interactive(vec![round("", "", &six, &[])]), vec![said(&[secret])]),
        // the wrong number of answers
        (Options::keyboard_interactive(vec![round("", "", &[(question, false), (question, true)], &["a", "b"])]), vec![said(&[secret])]),
    ];
    for (options, answers) in cases {
        let server = ssh_server::start(options);
        let person = Scripted::new(answers);
        // The pane's own line and the connection's error text.
        let (link, mut out) = pane(&server, person.auth());
        out.until("Press Enter to try again", 10);
        assert!(!out.seen.contains(secret) && !out.seen.contains("XJ-49-ZZ"), "the pane says {:?}", out.seen);
        Box::new(link).close().unwrap();

        let person = Scripted::new(vec![said(&[secret])]);
        let connection = Connection::standalone("t", server.target(person.auth())).unwrap();
        let why = connection.block_on(connection.session(Open::Interactive)).unwrap_err().to_string();
        assert!(!why.contains(secret) && !why.contains("XJ-49-ZZ"), "{why}");
        let debug = format!("{connection:?} {:?} {:?}", connection.target(), said(&[secret]));
        assert!(!debug.contains(secret), "{debug}");
    }
    drop(dir);
}

#[test]
fn connections_hand_the_prompter_to_every_interactive_target_in_the_chain() {
    let dir = Scratch::new("kch");
    let (connections, _) = Connections::load(dir.path(), Arc::new(NoKeychain), None, &[]);
    for (id, jump) in [("bastion", None), ("inner", Some("bastion"))] {
        let mut profile = Profile::new(id, id, format!("{id}.example"));
        profile.user = "holz".into();
        profile.auth = Auth::Interactive;
        profile.jump = jump.map(str::to_owned);
        connections.save(profile, SecretChange::Keep).unwrap();
    }

    // With none set the target carries none: the login fails with a sentence instead of asking.
    let target = connections.ssh_target("inner").unwrap();
    assert!(matches!(target.auth, SshAuth::Interactive { prompter: None }));

    let person = Scripted::typing(&[&["x"], &["y"]]);
    connections.set_prompter(person.clone());
    let target = connections.ssh_target("inner").unwrap();
    let ask = |auth: &SshAuth| match auth {
        SshAuth::Interactive { prompter: Some(p) } => {
            let _ = p.ask(PromptRound {
                connection: "what the server says".into(),
                host: String::new(),
                name: String::new(),
                instructions: String::new(),
                prompts: Vec::new(),
            });
        }
        other => panic!("no prompter on {other:?}"),
    };
    ask(&target.auth);
    ask(&target.jump.as_ref().expect("the jump host").auth);
    let ids: Vec<String> = person.rounds().into_iter().map(|r| r.connection).collect();
    assert_eq!(ids, ["inner", "bastion"], "each round carries the id of the profile its target was made from");
    assert_eq!(connections.identity_of("bastion"), Some(("bastion".into(), "bastion.example".into())));
    assert_eq!(connections.identity_of("nobody"), None);
}

#[test]
fn an_interactive_connection_without_a_window_fails_in_words() {
    let dir = Scratch::new("knw");
    let server = code_server();
    let connections = saved(&dir, &server, None);
    let connection = connections.session("talos").unwrap();
    let error = connection.block_on(connection.session(Open::Interactive)).unwrap_err();
    assert_eq!(error, OpenError { fatal: true, why: "this connection needs a window to ask in".into() });
    assert_eq!(count(&server, |s| &s.logins), 0);
}

#[test]
fn an_interactive_profile_cannot_carry_a_secret() {
    let dir = Scratch::new("ksc");
    let (connections, _) = Connections::load(dir.path(), Arc::new(NoKeychain), None, &[]);
    let mut profile = Profile::new("talos", "Talos", "talos.example");
    profile.user = "holz".into();
    profile.auth = Auth::Interactive;

    for secret in ["a-password", ""] {
        let error = connections.save(profile.clone(), SecretChange::Set { value: secret.into() }).unwrap_err();
        assert!(error.to_string().contains("an interactive login has no saved password or code"), "{error}");
    }
    assert!(connections.list().is_empty(), "nothing was saved");
    assert!(!dir.path().join("connections.json").exists(), "nothing was written");

    // Keep and Clear are fine; the keychain is never read (the store panics on a read).
    let saved = connections.save(profile.clone(), SecretChange::Keep).unwrap();
    assert!(!saved.has_secret);
    connections.save(profile, SecretChange::Clear).unwrap();
    assert!(!connections.list()[0].has_secret);
}

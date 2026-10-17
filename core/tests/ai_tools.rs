//! The approval path of the agent's tools, with no network, no terminal and no page.
//!
//! A fake host stands in for the shell. A call goes through every layer the shell will use: typed arguments, host
//! resolution, the deny check, a hash over the exact payload, a card, a decision that carries that hash, a second
//! resolution and a second hash at execution, and only then a ticket for the host. Every wait is bounded, so a broken
//! table fails an assertion instead of hanging the run.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::tools::{
    Access, ApprovalTable, Decision, DenyPolicy, Effect, FsKind, HASH_PREFIX, MAX_CALL_ID_BYTES, MAX_OPEN_CALLS,
    MAX_RESULT_BYTES, MAX_TAINT_LABELS, MAX_TURNS, Pending, Resolved, Scope, Style, TaintSource, Ticket, Tool, ToolError,
    ToolHost, ToolName, ToolOutput, denied_result, fields_of, payload_hash, specs, visible, visible_multiline,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use serde_json::{Value, json};

mod common;

const NOTES: &str = "/srv/app/notes.txt";

type Hook = Box<dyn FnMut(usize) + Send>;

/// One call the fake host executed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ran {
    call_id: String,
    tool: ToolName,
    path: Option<String>,
}

struct HostState {
    kind: FsKind,
    target: String,
    files: BTreeMap<String, String>,
    redirects: BTreeMap<String, String>,
    stamps: BTreeMap<String, i64>,
    unavailable: BTreeSet<ToolName>,
    resolve_count: usize,
    ran: Vec<Ran>,
    ticket_debug: Vec<String>,
    started: usize,
    pane_override: Option<PaneId>,
    hook: Option<Hook>,
    run_delay: Duration,
}

/// The shell, as far as the approval table can tell.
struct FakeHost {
    state: Arc<Mutex<HostState>>,
}

impl FakeHost {
    fn new(kind: FsKind, target: &str) -> Self {
        Self {
            state: Arc::new(Mutex::new(HostState {
                kind,
                target: target.to_owned(),
                files: BTreeMap::new(),
                redirects: BTreeMap::new(),
                stamps: BTreeMap::new(),
                unavailable: BTreeSet::new(),
                resolve_count: 0,
                ran: Vec::new(),
                ticket_debug: Vec::new(),
                started: 0,
                pane_override: None,
                hook: None,
                run_delay: Duration::ZERO,
            })),
        }
    }

    /// A host that acts on another machine, called `build box`.
    fn remote() -> Self {
        Self::new(FsKind::Remote, "build box")
    }

    /// A host that acts on this computer.
    fn local() -> Self {
        Self::new(FsKind::Local, "This computer")
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_file(self, path: &str, text: &str) -> Self {
        self.state().files.insert(path.to_owned(), text.to_owned());
        self
    }

    fn without(self, tool: ToolName) -> Self {
        self.state().unavailable.insert(tool);
        self
    }

    /// Gives the file `path` the stamp `stamp`, as the host would after reading its modification time.
    fn with_stamp(self, path: &str, stamp: i64) -> Self {
        self.state().stamps.insert(path.to_owned(), stamp);
        self
    }

    fn with_run_delay(self, delay: Duration) -> Self {
        self.state().run_delay = delay;
        self
    }

    /// Calls `hook` with the ordinal of the resolution (1 for the first) at the start of every `resolve`.
    fn on_resolve(self, hook: impl FnMut(usize) + Send + 'static) -> Self {
        self.state().hook = Some(Box::new(hook));
        self
    }

    /// From now on `resolve` answers `to` for the path `from`: a symbolic link that was retargeted.
    fn redirect(&self, from: &str, to: &str) {
        self.state().redirects.insert(from.to_owned(), to.to_owned());
    }

    /// A handle that changes the world from inside a hook, where the host itself is not at hand.
    fn knobs(&self) -> Knobs {
        Knobs(Arc::clone(&self.state))
    }

    /// How many times the host began to run a call.
    fn started(&self) -> usize {
        self.state().started
    }

    /// A handle that retargets a path from inside a hook, where the host itself is not at hand.
    fn redirector(&self) -> impl Fn(&str, &str) + Send + 'static {
        let state = Arc::clone(&self.state);
        move |from, to| {
            state.lock().unwrap_or_else(PoisonError::into_inner).redirects.insert(from.to_owned(), to.to_owned());
        }
    }

    fn run_count(&self) -> usize {
        self.state().ran.len()
    }

    fn ran(&self) -> Vec<Ran> {
        self.state().ran.clone()
    }

    /// The `Debug` text of every ticket the host received, plain and pretty.
    fn ticket_debug(&self) -> Vec<String> {
        self.state().ticket_debug.clone()
    }

    fn resolve_count(&self) -> usize {
        self.state().resolve_count
    }
}

/// A handle on the world of a fake host.
#[derive(Clone)]
struct Knobs(Arc<Mutex<HostState>>);

impl Knobs {
    fn state(&self) -> std::sync::MutexGuard<'_, HostState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The host answers another pane from now on.
    fn set_pane(&self, pane: PaneId) {
        self.state().pane_override = Some(pane);
    }

    /// The host calls the machine something else from now on.
    fn set_target(&self, label: &str) {
        self.state().target = label.to_owned();
    }
}

impl ToolHost for FakeHost {
    fn available(&self, tool: ToolName, _pane: PaneId) -> bool {
        !self.state().unavailable.contains(&tool)
    }

    fn resolve(&self, scope: &Scope, tool: &Tool) -> Result<Resolved, ToolError> {
        let (ordinal, hook) = {
            let mut state = self.state();
            state.resolve_count += 1;
            (state.resolve_count, state.hook.take())
        };
        // The hook may change the world through this host, so it runs without the lock.
        let mut hook = hook;
        if let Some(hook) = hook.as_mut() {
            hook(ordinal);
        }
        let mut state = self.state();
        if state.hook.is_none() {
            state.hook = hook;
        }
        let pane = state.pane_override.or(tool.pane()).unwrap_or(scope.pane);
        let mut resolved = match state.kind {
            FsKind::Remote => Resolved::remote(pane, state.target.clone()),
            FsKind::Local => Resolved::local(pane, state.target.clone()),
        };
        if let Some(path) = tool.path() {
            let followed = state.redirects.get(path).cloned().unwrap_or_else(|| path.to_owned());
            if let Some(stamp) = state.stamps.get(&followed) {
                resolved = resolved.with_stamp(*stamp);
            }
            resolved = resolved.with_path(followed);
        }
        if let Some(cwd) = tool.cwd() {
            resolved = resolved.with_cwd(cwd);
        }
        Ok(resolved)
    }

    fn run(&self, ticket: &Ticket, cancel: &CancelToken) -> Result<ToolOutput, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let delay = {
            let mut state = self.state();
            state.started += 1;
            state.run_delay
        };
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        let mut state = self.state();
        let path = ticket.resolved().path().map(str::to_owned);
        state.ran.push(Ran { call_id: ticket.call_id().to_owned(), tool: ticket.tool().name(), path: path.clone() });
        state.ticket_debug.push(format!("{ticket:?}"));
        state.ticket_debug.push(format!("{ticket:#?}"));
        match ticket.tool() {
            Tool::ReadFile(args) => {
                let Some(text) = path.as_ref().and_then(|p| state.files.get(p)) else {
                    return Err(ToolError::Host(Detail::new("no such file")));
                };
                let bytes = text.as_bytes();
                let start = usize::try_from(args.offset()).unwrap_or(usize::MAX).min(bytes.len());
                let end = start.saturating_add(usize::try_from(args.limit()).unwrap_or(usize::MAX)).min(bytes.len());
                Ok(ToolOutput::new(String::from_utf8_lossy(&bytes[start..end]).into_owned()))
            }
            Tool::WriteFile(args) => {
                let Some(path) = path else {
                    return Err(ToolError::Host(Detail::new("no path")));
                };
                state.files.insert(path, args.content.clone());
                Ok(ToolOutput::new(format!("wrote {} bytes", args.content.len())))
            }
            _ => Ok(ToolOutput::new("ok".to_owned())),
        }
    }
}

fn scope(conversation: &str, turn: u64, pane: PaneId) -> Scope {
    Scope { conversation: conversation.to_owned(), turn, pane }
}

fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall { id: id.to_owned(), name: name.to_owned(), args }
}

fn read_call(id: &str, path: &str) -> ToolCall {
    call(id, "read_file", json!({ "path": path }))
}

fn table_with(protected: &[&str]) -> ApprovalTable {
    ApprovalTable::new(DenyPolicy::new(protected.iter().map(PathBuf::from).collect()))
}

fn is_lower_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The tracer: one `read_file` call goes through every layer of the approval path.
#[test]
fn a_read_call_is_proposed_approved_rechecked_and_executed() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let cancel = CancelToken::new();

    let pending = table.propose(&host, &scope, &read_call("call_1", NOTES)).expect("a plain read is proposed");
    assert_eq!(pending.tool, ToolName::ReadFile);
    assert_eq!(pending.effect, holzwave_core::ai::tools::Effect::Read);
    assert_eq!((pending.pane, pending.target.as_str(), pending.remote), (3, "build box", true));
    let names: Vec<&str> = pending.payload.iter().map(|field| field.name).collect();
    assert_eq!(names, ["tool", "pane", "target", "remote", "path", "offset", "limit", "resolved_path"]);
    assert!(is_lower_hex(&pending.hash), "the hash is 64 lower-case hex digits: {}", pending.hash);
    assert!(!pending.tainted && pending.taint.is_empty());
    let listed = table.pending("c1");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].hash, pending.hash);

    table.decide("c1", "call_1", Decision::Allow, &pending.hash).expect("the hash of the card is accepted");
    assert_eq!(table.wait("c1", "call_1", &cancel), Ok(Decision::Allow));
    let output = table.run(&host, &scope, "call_1", &cancel).expect("an approved call runs");
    assert_eq!(output.text(), "alpha beta");

    assert_eq!(host.ran(), [Ran { call_id: "call_1".to_owned(), tool: ToolName::ReadFile, path: Some(NOTES.to_owned()) }]);
    assert_eq!(host.resolve_count(), 2, "the host is asked when the call is proposed and again when it runs");
    assert!(table.pending("c1").is_empty());
    assert_eq!(table.run(&host, &scope, "call_1", &cancel), Err(ToolError::UnknownCall));
    assert_eq!(host.run_count(), 1);
}

#[test]
fn a_payload_changed_after_approval_is_refused_and_nothing_runs() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta").with_file("/srv/app/other.txt", "gamma");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let cancel = CancelToken::new();

    let pending = table.propose(&host, &scope, &read_call("call_1", NOTES)).unwrap();
    table.decide("c1", "call_1", Decision::Allow, &pending.hash).unwrap();
    // A symbolic link is retargeted between the click and the execution.
    host.redirect(NOTES, "/srv/app/other.txt");

    assert_eq!(table.run(&host, &scope, "call_1", &cancel), Err(ToolError::PayloadChanged));
    assert_eq!(host.run_count(), 0, "the host never saw the call");
    assert!(table.pending("c1").is_empty());
    assert_eq!(table.run(&host, &scope, "call_1", &cancel), Err(ToolError::UnknownCall), "the call is gone");

    // The same, when the world changes while the host resolves the call for the second time.
    let host = FakeHost::remote().with_file(NOTES, "alpha beta").with_file("/srv/app/other.txt", "gamma");
    let retarget = host.redirector();
    let host = host.on_resolve(move |ordinal| {
        if ordinal == 2 {
            retarget(NOTES, "/srv/app/other.txt");
        }
    });
    let pending = table.propose(&host, &scope, &read_call("call_2", NOTES)).unwrap();
    table.decide("c1", "call_2", Decision::Allow, &pending.hash).unwrap();
    assert_eq!(table.run(&host, &scope, "call_2", &cancel), Err(ToolError::PayloadChanged));
    assert_eq!(host.run_count(), 0);
}

#[test]
fn a_decision_must_carry_the_hash_the_user_saw() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let cancel = CancelToken::new();

    let first = table.propose(&host, &scope, &read_call("call_1", NOTES)).unwrap();
    let other = table.propose(&host, &scope, &read_call("call_2", "/srv/app/other.txt")).unwrap();
    assert_ne!(first.hash, other.hash);

    let mut one_char_off = first.hash.clone();
    let swapped = if one_char_off.starts_with('0') { "1" } else { "0" };
    one_char_off.replace_range(..1, swapped);
    let upper = first.hash.to_uppercase();
    assert_ne!(upper, first.hash, "the hash of this payload has a letter in it");

    for wrong in [other.hash.as_str(), one_char_off.as_str(), "", upper.as_str()] {
        assert_eq!(table.decide("c1", "call_1", Decision::Allow, wrong), Err(ToolError::HashMismatch));
    }
    assert_eq!(table.pending("c1").len(), 2, "a wrong hash leaves the call awaiting");
    assert_eq!(table.run(&host, &scope, "call_1", &cancel), Err(ToolError::NotApproved));

    table.decide("c1", "call_1", Decision::Allow, &first.hash).expect("a correct decision still works");
    assert_eq!(
        table.decide("c1", "call_1", Decision::Allow, &first.hash),
        Err(ToolError::NotPending),
        "a call is decided once"
    );
    assert_eq!(table.decide("c1", "nope", Decision::Allow, &first.hash), Err(ToolError::UnknownCall));
    assert_eq!(table.decide("c2", "call_1", Decision::Allow, &first.hash), Err(ToolError::UnknownCall));
}

#[test]
fn an_approval_runs_once_even_when_two_threads_race_for_it() {
    let host = Arc::new(FakeHost::remote().with_file(NOTES, "alpha beta").with_run_delay(Duration::from_millis(2)));
    let table = Arc::new(table_with(&[]));
    let scope = scope("c1", 1, 3);

    for round in 0..25 {
        let id = format!("race_{round}");
        let pending = table.propose(&*host, &scope, &read_call(&id, NOTES)).unwrap();
        table.decide("c1", &id, Decision::Allow, &pending.hash).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let runners: Vec<_> = (0..2)
            .map(|_| {
                let (host, table, scope, id, barrier) =
                    (Arc::clone(&host), Arc::clone(&table), scope.clone(), id.clone(), Arc::clone(&barrier));
                thread::spawn(move || {
                    barrier.wait();
                    table.run(&*host, &scope, &id, &CancelToken::new())
                })
            })
            .collect();
        let results: Vec<_> = runners.into_iter().map(|runner| runner.join().expect("a runner thread panicked")).collect();
        let ok = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(ok, 1, "round {round}: exactly one thread runs the call: {results:?}");
        for result in results.iter().filter(|result| result.is_err()) {
            assert!(
                matches!(result, Err(ToolError::AlreadyUsed) | Err(ToolError::UnknownCall)),
                "round {round}: the loser is refused: {result:?}"
            );
        }
        assert_eq!(host.run_count(), round + 1, "the host ran the call once");
    }
}

#[test]
fn a_call_that_was_not_approved_does_not_run() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);

    table.propose(&host, &scope, &read_call("call_1", NOTES)).unwrap();
    assert_eq!(table.run(&host, &scope, "call_1", &CancelToken::new()), Err(ToolError::NotApproved));
    assert_eq!(host.run_count(), 0);
    assert_eq!(table.pending("c1").len(), 1, "the call is still waiting for its decision");
}

#[test]
fn a_denied_call_never_reaches_the_host() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let cancel = CancelToken::new();

    let pending = table.propose(&host, &scope, &read_call("call_1", NOTES)).unwrap();
    table.decide("c1", "call_1", Decision::Deny, &pending.hash).unwrap();
    assert_eq!(table.wait("c1", "call_1", &cancel), Ok(Decision::Deny));
    assert_eq!(host.run_count(), 0);
    assert!(table.pending("c1").is_empty());
    assert_eq!(table.run(&host, &scope, "call_1", &cancel), Err(ToolError::UnknownCall));
    assert_eq!(host.run_count(), 0);

    let result = denied_result("call_1");
    assert_eq!((result.call_id.as_str(), result.is_error), ("call_1", true));
    assert_eq!(result.content, "The user denied this action.");
}

#[test]
fn waiting_for_a_decision_ends_at_once_on_cancel() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = Arc::new(table_with(&[]));
    let scope = scope("c1", 1, 3);
    table.propose(&host, &scope, &read_call("call_1", NOTES)).unwrap();

    let cancel = CancelToken::new();
    let (sender, receiver) = mpsc::channel();
    let waiter = {
        let (table, cancel) = (Arc::clone(&table), cancel.clone());
        thread::spawn(move || {
            let _ = sender.send(table.wait("c1", "call_1", &cancel));
        })
    };
    thread::sleep(Duration::from_millis(30));
    let raised = Instant::now();
    cancel.cancel();
    let result = receiver.recv_timeout(Duration::from_secs(5)).expect("the waiting turn ended");
    let latency = raised.elapsed();
    waiter.join().expect("the waiter thread panicked");
    println!("waiting turn stopped {latency:?} after the cancel");
    assert_eq!(result, Err(ToolError::Cancelled));
    assert!(latency < Duration::from_millis(250), "the wait took {latency:?} to notice the cancel");
    assert!(table.pending("c1").is_empty(), "the call is gone");

    // A token that is cancelled before the wait ends it at once.
    table.propose(&host, &scope, &read_call("call_2", NOTES)).unwrap();
    let started = Instant::now();
    assert_eq!(table.wait("c1", "call_2", &cancel), Err(ToolError::Cancelled));
    assert!(started.elapsed() < Duration::from_millis(250));
    assert!(table.pending("c1").is_empty());
}

#[test]
fn a_protected_path_is_refused_before_a_card_exists() {
    // A secret file on the remote machine.
    let remote = FakeHost::remote().with_file("/srv/app/.env", "KEY=1");
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let denied = table.propose(&remote, &scope, &read_call("call_1", "/srv/app/.env")).unwrap_err();
    assert!(matches!(denied, ToolError::Denied(_)), "{denied:?}");
    let text = denied.to_result("call_1").content;
    assert!(!text.contains(".env") && !text.contains("env_file") && !text.contains("/srv/app"), "{text}");
    assert!(table.pending("c1").is_empty());
    assert_eq!(remote.run_count(), 0);

    // A file the shell protects, on this computer.
    let local = FakeHost::local();
    let table = table_with(&["/data/holzwave"]);
    let denied = table.propose(&local, &scope, &read_call("call_2", "/data/holzwave/settings.json")).unwrap_err();
    assert!(matches!(denied, ToolError::Denied(_)), "{denied:?}");
    let text = denied.to_result("call_2").content;
    assert!(!text.contains("settings") && !text.contains("protected_dir") && !text.contains("/data"), "{text}");
    assert!(table.pending("c1").is_empty());
    assert_eq!(local.run_count(), 0);
}

/// One error of each variant, in the order of the stable codes.
fn every_error() -> Vec<ToolError> {
    vec![
        ToolError::UnknownTool(Detail::new("format_disk")),
        ToolError::InvalidArguments(Detail::new("`path` is required")),
        ToolError::Unavailable,
        ToolError::Denied("secret_location"),
        ToolError::TooMany,
        ToolError::DuplicateCall,
        ToolError::UnknownCall,
        ToolError::NotPending,
        ToolError::HashMismatch,
        ToolError::NotApproved,
        ToolError::AlreadyUsed,
        ToolError::PayloadChanged,
        ToolError::ScopeMismatch,
        ToolError::TaintChanged,
        ToolError::Cancelled,
        ToolError::Host(Detail::new("no such file")),
        ToolError::NotShared,
    ]
}

#[test]
fn tool_errors_have_stable_codes_and_texts_without_arguments() {
    let codes = [
        "unknown_tool",
        "invalid_arguments",
        "unavailable",
        "denied",
        "too_many",
        "duplicate_call",
        "unknown_call",
        "not_pending",
        "hash_mismatch",
        "not_approved",
        "already_used",
        "payload_changed",
        "scope_mismatch",
        "taint_changed",
        "cancelled",
        "host",
        "not_shared",
    ];
    let errors = every_error();
    assert_eq!(errors.len(), codes.len());
    for (error, code) in errors.iter().zip(codes) {
        assert_eq!(error.code(), code);
        assert_eq!(format!("{error:?}"), code, "Debug prints the code and nothing else");
        assert_eq!(format!("{error:#?}"), code);
        let result = error.to_result("c");
        assert!(result.is_error);
        assert_eq!(result.call_id, "c");
        assert!(!result.content.is_empty() && !result.content.contains('\n'), "{code}: {:?}", result.content);
        let _: &dyn std::error::Error = error;
    }

    // The fourteen variants without a detail say the same thing whatever call they came from.
    let canary = "CANARY-9c41";
    let host = FakeHost::remote();
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    let denied = table.propose(&host, &scope, &read_call("a", &format!("/srv/{canary}/.env"))).unwrap_err();
    assert_eq!(denied, ToolError::Denied("env_file"));
    assert_eq!(denied.to_result("a").content, ToolError::Denied("secret_location").to_result("a").content);
    let invalid = table
        .propose(&host, &scope, &call("b", "read_file", json!({ "path": canary, "offset": canary })))
        .unwrap_err();
    assert!(matches!(invalid, ToolError::InvalidArguments(_)));
    assert!(!invalid.to_result("b").content.contains(canary), "{}", invalid.to_result("b").content);
    let unknown_argument = table
        .propose(&host, &scope, &call("c", "read_file", json!({ "path": canary, "extra": canary })))
        .unwrap_err();
    assert!(!unknown_argument.to_result("c").content.contains(canary));

    // A tool the host does not offer is refused before anything is resolved.
    let without = FakeHost::remote().without(ToolName::ReadFile);
    assert_eq!(table.propose(&without, &scope, &read_call("d", NOTES)).unwrap_err(), ToolError::Unavailable);
    assert_eq!(without.resolve_count(), 0);

    // A detail built from hostile text is scrubbed and bounded.
    let hostile = format!("\u{1b}[31mred\nline {}", "x".repeat(500));
    for error in [
        ToolError::Host(Detail::new(&hostile)),
        ToolError::InvalidArguments(Detail::new(&hostile)),
        ToolError::UnknownTool(Detail::new(&hostile)),
    ] {
        let content = error.to_result("c").content;
        assert!(!content.chars().any(char::is_control), "{content:?}");
        let detail = Detail::new(&hostile);
        assert!(detail.as_str().chars().count() <= 241);
        assert!(content.ends_with(detail.as_str()), "{content:?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The catalog: bounds, schemas, the card and the hash.
// ---------------------------------------------------------------------------------------------------------------------

/// How a text field treats line breaks, as the tests state it (written out here, not read from the production table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Lines {
    Reject,
    Escape,
    Keep,
}

/// The bounds of one argument, as the tests state them.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Text { min: usize, max: usize, lines: Lines },
    Int { min: u64, max: u64, default: Option<u64> },
    Flag { default: bool },
}

const MAX_PANE: u64 = u32::MAX as u64;

/// The arguments of a tool in table order: name, bounds, and whether the call has to give it.
fn bounds(tool: ToolName) -> Vec<(&'static str, Kind, bool)> {
    let path = ("path", Kind::Text { min: 1, max: 4096, lines: Lines::Reject }, true);
    let pane = ("pane", Kind::Int { min: 0, max: MAX_PANE, default: None }, false);
    match tool {
        ToolName::ReadFile => vec![
            path,
            ("offset", Kind::Int { min: 0, max: 1 << 50, default: Some(0) }, false),
            ("limit", Kind::Int { min: 1, max: 16384, default: Some(16384) }, false),
            pane,
        ],
        ToolName::ListDir => {
            vec![path, ("max_entries", Kind::Int { min: 1, max: 1000, default: Some(200) }, false), pane]
        }
        ToolName::Grep => vec![
            ("pattern", Kind::Text { min: 1, max: 512, lines: Lines::Reject }, true),
            path,
            ("ignore_case", Kind::Flag { default: false }, false),
            ("max_matches", Kind::Int { min: 1, max: 200, default: Some(50) }, false),
            pane,
        ],
        ToolName::ReadPane => vec![
            ("pane", Kind::Int { min: 0, max: MAX_PANE, default: None }, true),
            ("rows", Kind::Int { min: 1, max: 1000, default: Some(100) }, false),
        ],
        ToolName::WriteFile => {
            vec![path, ("content", Kind::Text { min: 0, max: 131_072, lines: Lines::Keep }, true), pane]
        }
        ToolName::EditFile => vec![
            path,
            ("old_text", Kind::Text { min: 1, max: 32_768, lines: Lines::Keep }, true),
            ("new_text", Kind::Text { min: 0, max: 32_768, lines: Lines::Keep }, true),
            ("replace_all", Kind::Flag { default: false }, false),
            pane,
        ],
        ToolName::RunCommand => vec![
            ("command", Kind::Text { min: 1, max: 8192, lines: Lines::Escape }, true),
            ("cwd", Kind::Text { min: 1, max: 4096, lines: Lines::Reject }, false),
            ("timeout_secs", Kind::Int { min: 1, max: 600, default: Some(60) }, false),
            pane,
        ],
    }
}

/// A full, valid set of arguments for each tool.
fn sample(tool: ToolName) -> Value {
    match tool {
        ToolName::ReadFile => json!({ "path": NOTES, "offset": 4, "limit": 100, "pane": 3 }),
        ToolName::ListDir => json!({ "path": "/srv/app", "max_entries": 10, "pane": 3 }),
        ToolName::Grep => {
            json!({ "pattern": "needle", "path": "/srv/app", "ignore_case": true, "max_matches": 5, "pane": 3 })
        }
        ToolName::ReadPane => json!({ "pane": 3, "rows": 20 }),
        ToolName::WriteFile => json!({ "path": "/srv/app/out.txt", "content": "hello\nworld", "pane": 3 }),
        ToolName::EditFile => json!({
            "path": "/srv/app/out.txt", "old_text": "hello", "new_text": "howdy", "replace_all": true, "pane": 3
        }),
        ToolName::RunCommand => json!({ "command": "ls -l", "cwd": "/srv/app", "timeout_secs": 30, "pane": 3 }),
    }
}

/// `args` with `field` set to `value`.
fn with(mut args: Value, field: &str, value: Value) -> Value {
    args[field] = value;
    args
}

/// `args` without `field`.
fn without_field(mut args: Value, field: &str) -> Value {
    args.as_object_mut().expect("arguments are an object").remove(field);
    args
}

/// The arguments of a parsed tool, serialised back.
fn args_of(tool: &Tool) -> Value {
    match tool {
        Tool::ReadFile(a) => serde_json::to_value(a),
        Tool::ListDir(a) => serde_json::to_value(a),
        Tool::Grep(a) => serde_json::to_value(a),
        Tool::ReadPane(a) => serde_json::to_value(a),
        Tool::WriteFile(a) => serde_json::to_value(a),
        Tool::EditFile(a) => serde_json::to_value(a),
        Tool::RunCommand(a) => serde_json::to_value(a),
    }
    .expect("arguments serialise")
}

fn parse(tool: ToolName, args: &Value) -> Result<Tool, ToolError> {
    Tool::parse(tool.as_str(), args)
}

fn invalid(result: &Result<Tool, ToolError>) -> bool {
    matches!(result, Err(ToolError::InvalidArguments(_)))
}

/// The schema of `read_file` with the descriptions removed, written out in full: it is what models are taught.
fn pinned_read_file_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "minLength": 1, "maxLength": 4096 },
            "offset": { "type": "integer", "minimum": 0, "maximum": 1_125_899_906_842_624u64, "default": 0 },
            "limit": { "type": "integer", "minimum": 1, "maximum": 16384, "default": 16384 },
            "pane": { "type": "integer", "minimum": 0, "maximum": 4_294_967_295u64 }
        },
        "required": ["path"]
    })
}

#[test]
fn every_tool_declares_a_schema_that_matches_its_argument_struct() {
    let required_by_tool: [(ToolName, &[&str]); 7] = [
        (ToolName::ReadFile, &["path"]),
        (ToolName::ListDir, &["path"]),
        (ToolName::Grep, &["pattern", "path"]),
        (ToolName::ReadPane, &["pane"]),
        (ToolName::WriteFile, &["path", "content"]),
        (ToolName::EditFile, &["path", "old_text", "new_text"]),
        (ToolName::RunCommand, &["command"]),
    ];
    assert_eq!(ToolName::ALL.len(), 7);
    for (tool, required_names) in required_by_tool {
        let table = bounds(tool);
        let names: Vec<&str> = table.iter().map(|(name, ..)| *name).collect();

        // The table of the code names the same fields in the same order.
        let code_names: Vec<&str> = fields_of(tool).iter().map(|field| field.name).collect();
        assert_eq!(code_names, names, "{tool:?}");

        // A full sample decodes, and the struct serialises back to exactly the fields of the table.
        let parsed = parse(tool, &sample(tool)).unwrap_or_else(|e| panic!("{tool:?}: {e}"));
        assert_eq!(parsed.name(), tool);
        let back: BTreeSet<String> = args_of(&parsed).as_object().unwrap().keys().cloned().collect();
        let expected: BTreeSet<String> = names.iter().map(|n| (*n).to_owned()).collect();
        assert_eq!(back, expected, "{tool:?}: the struct and the table name the same fields");

        // Removing a field fails exactly for the required ones.
        for (name, _, required) in &table {
            let result = parse(tool, &without_field(sample(tool), name));
            assert_eq!(result.is_err(), *required, "{tool:?}.{name}");
            if *required {
                assert!(invalid(&result), "{tool:?}.{name}: {result:?}");
            }
        }

        // The schema is generated from the table.
        let spec = tool.spec();
        assert_eq!(spec.name, tool.as_str());
        let schema = &spec.input_schema;
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        let properties = schema["properties"].as_object().expect("properties");
        let declared: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
        assert_eq!(declared, names.iter().copied().collect::<BTreeSet<_>>(), "{tool:?}");
        let required: Vec<&str> = schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(required, required_names, "{tool:?}: required fields in table order");
        for (name, kind, _) in &table {
            let property = &properties[*name];
            assert!(!property["description"].as_str().unwrap_or_default().is_empty(), "{tool:?}.{name} has no description");
            match *kind {
                Kind::Text { min, max, .. } => {
                    assert_eq!(property["type"], "string");
                    assert_eq!((property["minLength"].as_u64(), property["maxLength"].as_u64()), (Some(min as u64), Some(max as u64)));
                    assert!(property.get("default").is_none());
                }
                Kind::Int { min, max, default } => {
                    assert_eq!(property["type"], "integer");
                    assert_eq!((property["minimum"].as_u64(), property["maximum"].as_u64()), (Some(min), Some(max)));
                    assert_eq!(property.get("default").and_then(Value::as_u64), default, "{tool:?}.{name}");
                }
                Kind::Flag { default } => {
                    assert_eq!(property["type"], "boolean");
                    assert_eq!(property["default"], default);
                }
            }
        }

        // The description tells the model that the user decides.
        assert!(!spec.description.is_empty() && spec.description.chars().count() <= 400, "{tool:?}");
        assert!(spec.description.contains("The user approves each call"), "{tool:?}: {}", spec.description);
    }

    // The schema of `read_file` is pinned.
    let mut schema = ToolName::ReadFile.spec().input_schema;
    for property in schema["properties"].as_object_mut().unwrap().values_mut() {
        property.as_object_mut().unwrap().remove("description");
    }
    assert_eq!(schema, pinned_read_file_schema());

    // `run_command` says what it is not.
    let command = ToolName::RunCommand.spec().description;
    assert!(command.contains("separate process") && command.contains("not typed into the pane"), "{command}");

    // The defaults of the schema are the values the card shows for a call that leaves the field out.
    let host = FakeHost::remote();
    let table = table_with(&[]);
    let scope = scope("c1", 1, 3);
    for tool in ToolName::ALL {
        let mut minimal = sample(tool);
        for (name, _, required) in bounds(tool) {
            if !required {
                minimal = without_field(minimal, name);
            }
        }
        let pending = table.propose(&host, &scope, &call(tool.as_str(), tool.as_str(), minimal)).unwrap();
        let schema = tool.spec().input_schema;
        for (name, property) in schema["properties"].as_object().unwrap() {
            let Some(default) = property.get("default") else { continue };
            let shown = pending
                .payload
                .iter()
                .find(|field| field.name == name.as_str())
                .unwrap_or_else(|| panic!("{tool:?}: the card shows no {name}"));
            assert_eq!(shown.value, default.to_string(), "{tool:?}.{name}");
        }
    }
}

#[test]
fn an_unknown_argument_is_refused_for_every_tool() {
    let canary = "CANARY-9c41";
    for tool in ToolName::ALL {
        // Every text of the sample carries the canary, so a refusal that quotes a value shows it.
        let mut args = sample(tool);
        for value in args.as_object_mut().unwrap().values_mut() {
            if let Some(text) = value.as_str() {
                *value = json!(format!("{canary} {text}"));
            }
        }
        assert!(parse(tool, &args).is_ok(), "{tool:?}");
        let result = parse(tool, &with(args.clone(), "bogus", json!(canary)));
        assert!(invalid(&result), "{tool:?}: {result:?}");
        let text = result.unwrap_err().to_result("c").content;
        assert!(text.contains("bogus") && !text.contains(canary), "{tool:?}: {text}");

        // A hostile key is scrubbed and bounded.
        let hostile = format!("\u{1b}[2J{}", "k".repeat(400));
        let result = parse(tool, &with(args, &hostile, json!(1)));
        let text = result.unwrap_err().to_result("c").content;
        assert!(!text.chars().any(char::is_control) && text.chars().count() < 400, "{tool:?}: {text}");
    }
}

#[test]
fn text_and_number_bounds_are_enforced_for_every_field() {
    for tool in ToolName::ALL {
        for (name, kind, required) in bounds(tool) {
            let at = |value: Value| parse(tool, &with(sample(tool), name, value));
            let label = format!("{tool:?}.{name}");
            match kind {
                Kind::Text { min, max, .. } => {
                    assert!(at(json!("a".repeat(max))).is_ok(), "{label}: exactly the maximum");
                    assert!(invalid(&at(json!("a".repeat(max + 1)))), "{label}: one byte over the maximum");
                    if min > 0 {
                        assert!(invalid(&at(json!("a".repeat(min - 1)))), "{label}: below the minimum");
                    }
                    assert!(at(json!("a".repeat(min))).is_ok(), "{label}: exactly the minimum");
                    assert!(invalid(&at(json!(7))), "{label}: a number for text");
                    assert!(invalid(&at(json!(["a"]))), "{label}: a list for text");
                    // Bytes are counted, not characters: two-byte characters reach the bound at half the count.
                    if max >= 2 {
                        assert!(invalid(&at(json!("é".repeat(max / 2 + 1)))), "{label}: bytes, not characters");
                    }
                }
                Kind::Int { min, max, .. } => {
                    assert!(at(json!(min)).is_ok(), "{label}: exactly the minimum");
                    assert!(at(json!(max)).is_ok(), "{label}: exactly the maximum");
                    assert!(invalid(&at(json!(max + 1))), "{label}: above the maximum");
                    if min > 0 {
                        assert!(invalid(&at(json!(min - 1))), "{label}: below the minimum");
                    }
                    assert!(invalid(&at(json!(-1))), "{label}: a negative number");
                    assert!(invalid(&at(json!(1.5))), "{label}: a fraction");
                    assert!(invalid(&at(json!(min as f64))), "{label}: a float where an integer is expected");
                    assert!(invalid(&at(json!(min.to_string()))), "{label}: a string for a number");
                    assert!(invalid(&at(json!(true))), "{label}: a flag for a number");
                }
                Kind::Flag { .. } => {
                    assert!(at(json!(true)).is_ok() && at(json!(false)).is_ok(), "{label}");
                    assert!(invalid(&at(json!("true"))), "{label}: a string for a flag");
                    assert!(invalid(&at(json!(1))), "{label}: a number for a flag");
                }
            }
            if required {
                assert!(invalid(&at(Value::Null)), "{label}: null for a required field");
            } else {
                assert!(at(Value::Null).is_ok(), "{label}: null for an optional field means it is left out");
            }
        }
    }
}

#[test]
fn control_characters_are_refused_where_the_field_does_not_allow_them() {
    let refused = ['\0', '\u{1b}', '\u{7f}', '\u{85}', '\u{1}', '\u{9f}'];
    let breaks = ['\n', '\t', '\r'];
    let allowed = ['\u{202e}', '\u{200b}', '\u{e0041}', '\u{ad}', '\u{feff}'];
    let mut checked = 0;
    for tool in ToolName::ALL {
        for (name, kind, _) in bounds(tool) {
            let Kind::Text { lines, .. } = kind else { continue };
            let at = |c: char| parse(tool, &with(sample(tool), name, json!(format!("a{c}b"))));
            for c in refused {
                assert!(invalid(&at(c)), "{tool:?}.{name}: U+{:04X} is refused", u32::from(c));
            }
            for c in breaks {
                assert_eq!(at(c).is_ok(), lines != Lines::Reject, "{tool:?}.{name}: {c:?} with {lines:?}");
            }
            for c in allowed {
                assert!(at(c).is_ok(), "{tool:?}.{name}: U+{:04X} is allowed and shown escaped", u32::from(c));
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 11, "every text field of the seven tools was checked");
    // The card spells the allowed ones out.
    assert_eq!(visible("a\u{202e}b\u{200b}c\u{e0041}"), "a\\u{202e}b\\u{200b}c\\u{e0041}");
}

#[test]
fn a_wrong_type_or_a_missing_field_is_refused_with_scrubbed_text() {
    let canary = "CANARY-9c41";
    for tool in ToolName::ALL {
        let mut args = sample(tool);
        for value in args.as_object_mut().unwrap().values_mut() {
            if let Some(text) = value.as_str() {
                *value = json!(format!("{canary} {text}"));
            }
        }
        for (name, kind, required) in bounds(tool) {
            let wrong = match kind {
                Kind::Text { .. } => json!(canary.len()),
                Kind::Int { .. } | Kind::Flag { .. } => json!(canary),
            };
            let mut attempts = vec![with(args.clone(), name, wrong)];
            if required {
                attempts.push(without_field(args.clone(), name));
                attempts.push(with(args.clone(), name, Value::Null));
            }
            for attempt in attempts {
                let result = parse(tool, &attempt);
                assert!(invalid(&result), "{tool:?}.{name}: {result:?}");
                let text = result.unwrap_err().to_result("c").content;
                assert!(text.contains(name), "{tool:?}.{name} is named: {text}");
                assert!(!text.contains(canary) && !text.chars().any(char::is_control), "{tool:?}.{name}: {text}");
            }
        }
        for not_an_object in [json!([]), json!("x"), json!(null), json!(5), json!(true)] {
            assert!(invalid(&parse(tool, &not_an_object)), "{tool:?}: {not_an_object}");
        }
    }

    // An unknown tool name is scrubbed and bounded.
    let hostile = format!("\u{1b}[2J{}", "y".repeat(300));
    for name in ["format_disk", hostile.as_str(), ""] {
        let Err(ToolError::UnknownTool(detail)) = Tool::parse(name, &json!({})) else {
            panic!("{name:?} is not a tool");
        };
        assert!(detail.as_str().chars().count() <= 241 && !detail.as_str().contains('\u{1b}'), "{detail:?}");
    }
    assert!(matches!(Tool::parse("READ_FILE", &json!({ "path": "/a" })), Err(ToolError::UnknownTool(_))));
}

#[test]
fn the_card_shows_every_field_escaped_and_the_hash_covers_all_of_them() {
    let table = table_with(&[]);
    let host = FakeHost::remote();
    let scope3 = scope("c1", 1, 3);
    let field = |pending: &holzwave_core::ai::tools::Pending, name: &str| {
        pending.payload.iter().find(|f| f.name == name).map(|f| (f.value.clone(), f.bytes))
    };

    // A command with a line break, a tab and a direction override.
    let command = "printf 'a\nb'\t\u{202e}x";
    let pending = table
        .propose(&host, &scope3, &call("run", "run_command", json!({ "command": command, "cwd": "/srv/app" })))
        .unwrap();
    let names: Vec<&str> = pending.payload.iter().map(|f| f.name).collect();
    assert_eq!(names, ["tool", "pane", "target", "remote", "command", "cwd", "timeout_secs", "resolved_cwd"]);
    assert_eq!(field(&pending, "command"), Some(("printf 'a\\nb'\\t\\u{202e}x".to_owned(), command.len())));
    assert_eq!(field(&pending, "timeout_secs").map(|f| f.0), Some("60".to_owned()), "the effective timeout is shown");
    assert_eq!(field(&pending, "resolved_cwd").map(|f| f.0), Some("/srv/app".to_owned()));
    assert_eq!(field(&pending, "tool").map(|f| f.0), Some("run_command".to_owned()));
    assert_eq!(pending.effect, holzwave_core::ai::tools::Effect::Exec);

    // A file write shows its content as it is, with the carriage return spelled out.
    let content = "line1\nline2\r\nend\t\\";
    let pending = table
        .propose(&host, &scope3, &call("w", "write_file", json!({ "path": "/srv/app/out.txt", "content": content })))
        .unwrap();
    assert_eq!(field(&pending, "content"), Some(("line1\nline2\\r\nend\t\\\\".to_owned(), content.len())));
    let names: Vec<&str> = pending.payload.iter().map(|f| f.name).collect();
    assert_eq!(names, ["tool", "pane", "target", "remote", "path", "content", "resolved_path"]);

    // Every part of the world the decision covers changes the hash.
    let write = |path: &str| call("h", "write_file", json!({ "path": path, "content": "x" }));
    let hash_in = |host: &FakeHost, scope: &Scope, call: &ToolCall| {
        let table = table_with(&[]);
        table.propose(host, scope, call).unwrap().hash
    };
    let base = hash_in(&FakeHost::remote(), &scope3, &write("/srv/app/a.txt"));
    let redirected = FakeHost::remote();
    redirected.redirect("/srv/app/a.txt", "/srv/app/b.txt");
    let variants = [
        ("the raw path", hash_in(&FakeHost::remote(), &scope3, &write("/srv/app/c.txt"))),
        ("the pane", hash_in(&FakeHost::remote(), &scope("c1", 1, 4), &write("/srv/app/a.txt"))),
        ("the target label", hash_in(&FakeHost::new(FsKind::Remote, "other box"), &scope3, &write("/srv/app/a.txt"))),
        ("the remote flag", hash_in(&FakeHost::new(FsKind::Local, "build box"), &scope3, &write("/srv/app/a.txt"))),
        ("the resolved path", hash_in(&redirected, &scope3, &write("/srv/app/a.txt"))),
        ("a stamp", hash_in(&FakeHost::remote().with_stamp("/srv/app/a.txt", 1), &scope3, &write("/srv/app/a.txt"))),
        ("another stamp", hash_in(&FakeHost::remote().with_stamp("/srv/app/a.txt", 2), &scope3, &write("/srv/app/a.txt"))),
        (
            "the content",
            hash_in(&FakeHost::remote(), &scope3, &call("h", "write_file", json!({ "path": "/srv/app/a.txt", "content": "y" }))),
        ),
    ];
    let mut seen = BTreeSet::from([base.clone()]);
    for (what, hash) in &variants {
        assert_ne!(*hash, base, "changing {what} changes the hash");
        assert!(seen.insert(hash.clone()), "changing {what} gives a hash that another change gave too");
    }
}

/// SHA-256 over the documented layout, computed here and not by the code under test.
fn expected_hash(pairs: &[(&str, &str)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"holzwave.tool.v1\0");
    for (name, value) in pairs {
        hasher.update((name.len() as u32).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((value.len() as u32).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest: [u8; 32] = hasher.finalize().into();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn the_hash_layout_is_versioned_and_length_prefixed() {
    assert_eq!(HASH_PREFIX, b"holzwave.tool.v1\0");
    let pair = |n: &str, v: &str| (n.to_owned(), v.to_owned());

    let list = [pair("tool", "read_file"), pair("pane", "3"), pair("path", "/a/b")];
    assert_eq!(hex(&payload_hash(&list)), expected_hash(&[("tool", "read_file"), ("pane", "3"), ("path", "/a/b")]));
    assert_eq!(hex(&payload_hash(&[])), expected_hash(&[]), "an empty list is the hash of the prefix");

    // Where a name ends and a value begins is part of the hash.
    assert_ne!(payload_hash(&[pair("a", "bc")]), payload_hash(&[pair("ab", "c")]));
    assert_ne!(payload_hash(&[pair("a", ""), pair("b", "c")]), payload_hash(&[pair("a", "b"), pair("", "c")]));
    // So is the order.
    assert_ne!(payload_hash(&[pair("x", "1"), pair("y", "2")]), payload_hash(&[pair("y", "2"), pair("x", "1")]));

    // The hash of a real card is the hash of the layout over the raw values the call and the host gave.
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let pending = table.propose(&host, &scope("c1", 1, 3), &read_call("call_1", NOTES)).unwrap();
    let expected = expected_hash(&[
        ("tool", "read_file"),
        ("pane", "3"),
        ("target", "build box"),
        ("remote", "true"),
        ("path", NOTES),
        ("offset", "0"),
        ("limit", "16384"),
        ("resolved_path", NOTES),
    ]);
    assert_eq!(pending.hash, expected);
}

#[test]
fn escaping_for_the_card_is_injective_and_shows_hidden_characters() {
    assert_eq!(visible("plain text, 100% ok: é中😀"), "plain text, 100% ok: é中😀");
    assert_eq!(visible(""), "");
    let cases = [
        ("\\", "\\\\"),
        ("\n", "\\n"),
        ("\r", "\\r"),
        ("\t", "\\t"),
        ("\u{1b}", "\\u{1b}"),
        ("\0", "\\u{0}"),
        ("\u{7f}", "\\u{7f}"),
        ("\u{85}", "\\u{85}"),
        ("\u{ad}", "\\u{ad}"),
        ("\u{61c}", "\\u{61c}"),
        ("\u{200b}", "\\u{200b}"),
        ("\u{200f}", "\\u{200f}"),
        ("\u{202e}", "\\u{202e}"),
        ("\u{2028}", "\\u{2028}"),
        ("\u{2066}", "\\u{2066}"),
        ("\u{feff}", "\\u{feff}"),
        ("\u{e0041}", "\\u{e0041}"),
    ];
    for (raw, shown) in cases {
        assert_eq!(visible(raw), shown, "{raw:?}");
    }
    // The multi-line form keeps a line break and a tab and still spells out the rest.
    assert_eq!(visible_multiline("a\nb\tc\r\\\u{1b}"), "a\nb\tc\\r\\\\\\u{1b}");

    // No two different texts give the same output: every string of up to three characters over a hostile alphabet.
    let alphabet = ["a", "\\", "\n", "\t", "\u{1b}", "\u{202e}", "\u{200b}", "n", "\r"];
    let mut strings = vec![String::new()];
    let mut layer = vec![String::new()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for prefix in &layer {
            for letter in alphabet {
                next.push(format!("{prefix}{letter}"));
            }
        }
        strings.extend(next.iter().cloned());
        layer = next;
    }
    for (name, show) in [("visible", visible as fn(&str) -> String), ("visible_multiline", visible_multiline)] {
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        for text in &strings {
            if let Some(other) = seen.insert(show(text), text.clone()) {
                panic!("{name}: {other:?} and {text:?} look the same: {:?}", show(text));
            }
        }
        assert_eq!(seen.len(), strings.len());
    }
}

#[test]
fn tools_the_host_does_not_offer_are_not_declared_to_the_model() {
    let everything = FakeHost::remote();
    let declared: Vec<String> = specs(&everything, 3).into_iter().map(|spec| spec.name).collect();
    let all: Vec<&str> = ToolName::ALL.iter().map(|tool| tool.as_str()).collect();
    assert_eq!(declared, all);
    assert_eq!(all, ["read_file", "list_dir", "grep", "read_pane", "write_file", "edit_file", "run_command"]);
    for tool in ToolName::ALL {
        assert_eq!(ToolName::from_name(tool.as_str()), Some(tool));
    }
    assert_eq!(ToolName::from_name("shell"), None);

    // A host that offers only the read tools declares only those.
    let reading = FakeHost::remote().without(ToolName::WriteFile).without(ToolName::EditFile).without(ToolName::RunCommand);
    let declared: Vec<String> = specs(&reading, 3).into_iter().map(|spec| spec.name).collect();
    assert_eq!(declared, ["read_file", "list_dir", "grep", "read_pane"]);
    assert!(specs(&FakeHost::remote().without(ToolName::ReadFile), 3).iter().all(|spec| spec.name != "read_file"));

    // And a call to a tool it does not offer is refused before the host is asked anything else.
    let table = table_with(&[]);
    let call = call("w", "write_file", sample(ToolName::WriteFile));
    let error = table.propose(&reading, &scope("c1", 1, 3), &call).unwrap_err();
    assert_eq!(error, ToolError::Unavailable);
    assert_eq!(reading.resolve_count(), 0);
    assert!(table.pending("c1").is_empty());
}

#[test]
fn no_debug_output_of_a_tool_type_shows_arguments_paths_or_output() {
    let canary = "CANARY-9c41";
    let text = |extra: &str| format!("/srv/{canary}/{extra}");
    let calls = [
        json!({ "path": text("a") }),
        json!({ "path": text("b") }),
        json!({ "pattern": canary, "path": text("c") }),
        json!({ "pane": 3 }),
        json!({ "path": text("d"), "content": format!("{canary} body") }),
        json!({ "path": text("e"), "old_text": canary, "new_text": format!("{canary} new") }),
        json!({ "command": format!("echo {canary}"), "cwd": text("f") }),
    ];
    for (tool, args) in ToolName::ALL.into_iter().zip(calls) {
        let parsed = parse(tool, &args).unwrap_or_else(|e| panic!("{tool:?}: {e}"));
        for shown in [format!("{parsed:?}"), format!("{parsed:#?}")] {
            assert!(!shown.contains(canary), "{tool:?}: {shown}");
            assert!(shown.contains("bytes>") || tool == ToolName::ReadPane, "{tool:?} shows a size: {shown}");
        }
        // The argument struct on its own.
        let inner = match &parsed {
            Tool::ReadFile(a) => format!("{a:?}{a:#?}"),
            Tool::ListDir(a) => format!("{a:?}{a:#?}"),
            Tool::Grep(a) => format!("{a:?}{a:#?}"),
            Tool::ReadPane(a) => format!("{a:?}{a:#?}"),
            Tool::WriteFile(a) => format!("{a:?}{a:#?}"),
            Tool::EditFile(a) => format!("{a:?}{a:#?}"),
            Tool::RunCommand(a) => format!("{a:?}{a:#?}"),
        };
        assert!(!inner.contains(canary), "{tool:?}: {inner}");
    }

    let resolved = Resolved::local(3, "This computer").with_path(text("g")).with_cwd(text("h")).with_stamp(5);
    for shown in [format!("{resolved:?}"), format!("{resolved:#?}")] {
        assert!(!shown.contains(canary) && shown.contains("bytes>"), "{shown}");
    }
    let output = ToolOutput::new(format!("{canary} output"));
    for shown in [format!("{output:?}"), format!("{output:#?}")] {
        assert!(!shown.contains(canary) && shown.contains("bytes>"), "{shown}");
    }
    let scope = scope("c1", 1, 3);
    assert!(format!("{scope:?}").contains("c1"));

    // A card and a ticket, obtained through a real run.
    let host = FakeHost::remote().with_file(&text("a"), "body");
    let table = table_with(&[]);
    let pending = table.propose(&host, &scope, &read_call("call_1", &text("a"))).unwrap();
    for shown in [format!("{pending:?}"), format!("{pending:#?}"), format!("{table:?}")] {
        assert!(!shown.contains(canary), "{shown}");
    }
    assert!(format!("{pending:?}").contains("bytes>"));
    table.decide("c1", "call_1", Decision::Allow, &pending.hash).unwrap();
    table.run(&host, &scope, "call_1", &CancelToken::new()).unwrap();
    let tickets = host.ticket_debug();
    assert_eq!(tickets.len(), 2);
    for shown in tickets {
        assert!(!shown.contains(canary) && shown.contains("bytes>"), "{shown}");
    }
}

#[test]
fn tool_output_is_bounded_on_a_character_boundary_and_marked() {
    // 40 KiB of three-byte characters.
    let glyphs = ['€', '中', 'あ'];
    let long: String = (0..14_000).map(|i| glyphs[i % 3]).collect();
    assert!(long.len() > 40 * 1024);
    let output = ToolOutput::new(long.clone());
    let shown = output.text();
    assert!(shown.len() <= MAX_RESULT_BYTES, "{} bytes", shown.len());
    assert!(output.truncated() && output.omitted() > 0);
    assert!(shown.starts_with("€中あ€中あ") && long.ends_with(&shown[shown.len() - 9..]));
    let marker = format!("\n[... {} bytes omitted ...]\n", output.omitted());
    let at = shown.find(&marker).expect("the marker line is there");
    let (head, tail) = (&shown[..at], &shown[at + marker.len()..]);
    assert!(long.starts_with(head) && long.ends_with(tail));
    assert_eq!(head.len() + tail.len() + output.omitted(), long.len(), "the count names what was cut");
    assert_eq!(shown.matches("bytes omitted").count(), 1);
    assert!(output.clone().into_text().len() <= MAX_RESULT_BYTES);

    // Four-byte characters and a very long input cut cleanly too.
    for text in ["😀".repeat(20_000), "x".repeat(10 * 1024 * 1024), format!("{}é", "x".repeat(MAX_RESULT_BYTES))] {
        let output = ToolOutput::new(text.clone());
        assert!(output.text().len() <= MAX_RESULT_BYTES && output.truncated(), "{} bytes in", text.len());
        assert!(output.text().starts_with(&text[..8]) && text.ends_with(&output.text()[output.text().len() - 4..]));
    }

    // Text at the limit and below it is returned as it is.
    for len in [0, 1, MAX_RESULT_BYTES - 1, MAX_RESULT_BYTES] {
        let text = "y".repeat(len);
        let output = ToolOutput::new(text.clone());
        assert_eq!((output.text(), output.truncated(), output.omitted()), (text.as_str(), false, 0));
    }
    let output = ToolOutput::new("y".repeat(MAX_RESULT_BYTES + 1));
    assert!(output.truncated() && output.text().len() <= MAX_RESULT_BYTES);
}

// ---------------------------------------------------------------------------------------------------------------------
// The deny list.
// ---------------------------------------------------------------------------------------------------------------------

/// The rule that refuses `path`, or `None` when it is allowed.
fn rule(policy: &DenyPolicy, style: Style, fs: FsKind, path: &str, access: Access) -> Option<&'static str> {
    policy.check(style, fs, path, access).err().map(|denied| denied.rule())
}

/// The rule that refuses a path on another machine with posix names.
fn remote_rule(path: &str, access: Access) -> Option<&'static str> {
    rule(&DenyPolicy::new(Vec::new()), Style::Posix, FsKind::Remote, path, access)
}

#[test]
fn the_deny_list_refuses_secret_locations_for_reading_and_writing() {
    let secrets = [
        "/home/u/.ssh",
        "/home/u/.ssh/id_ed25519",
        "/home/u/.gnupg/pubring.kbx",
        "/home/u/.aws/credentials",
        "/home/u/.kube/config",
        "/home/u/.azure/accessTokens.json",
        "/home/u/.config/gcloud/credentials.db",
        "/home/u/.docker/config.json",
        "/home/u/.netrc",
        "/home/u/.git-credentials",
        "/home/u/proj/id_rsa",
        "/home/u/proj/id_ed25519",
    ];
    for path in secrets {
        for access in [Access::Read, Access::Write] {
            assert_eq!(remote_rule(path, access), Some("secret_location"), "{path} for {access:?}");
        }
    }
    for path in ["/proc/1234/environ", "/proc/self/environ"] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(remote_rule(path, access), Some("proc_environ"), "{path} for {access:?}");
        }
    }
    let allowed = [
        "/home/u/proj/src/main.rs",
        "/home/u/proj/README.md",
        "/home/u/proj/.gitignore",
        "/home/u/proj/.github/workflows/ci.yml",
        "/home/u/sshkeys.txt",
        "/home/u/proj/env.rs",
        "/home/u/proj/docs/ssh.md",
        "/home/u/.config/nvim/init.lua",
        "/home/u/proj/id_ed25519.pub",
        "/proc/cpuinfo",
    ];
    for path in allowed {
        for access in [Access::Read, Access::Write] {
            assert_eq!(remote_rule(path, access), None, "{path} for {access:?}");
        }
    }
}

#[test]
fn start_files_git_internals_and_editor_config_refuse_writes_only() {
    let write_only = [
        ("/home/u/.bashrc", "start_file"),
        ("/home/u/.zshrc", "start_file"),
        ("/home/u/.zshenv", "start_file"),
        ("/home/u/.profile", "start_file"),
        ("/home/u/.config/fish/config.fish", "start_file"),
        ("/home/u/.config/fish/conf.d/x.fish", "start_file"),
        ("/home/u/Documents/PowerShell/Microsoft.PowerShell_profile.ps1", "start_file"),
        ("/home/u/proj/.git/hooks/pre-commit", "git_internals"),
        ("/home/u/proj/.git/config", "git_internals"),
        ("/home/u/.gitconfig", "git_internals"),
        ("/home/u/proj/.vscode/tasks.json", "editor_config"),
        ("/home/u/proj/.idea/workspace.xml", "editor_config"),
    ];
    for (path, expected) in write_only {
        assert_eq!(remote_rule(path, Access::Read), None, "{path} may be read");
        assert_eq!(remote_rule(path, Access::Write), Some(expected), "{path} may not be written");
    }
}

#[test]
fn env_files_are_refused_by_name_in_any_directory() {
    for path in ["/srv/.env", "/srv/app/.env.production", "/srv/app/.envrc", "/srv/app/.ENV.local", ".env", "app/.env"] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(remote_rule(path, access), Some("env_file"), "{path} for {access:?}");
        }
    }
    // Both file systems and both spellings.
    let policy = DenyPolicy::new(Vec::new());
    for style in [Style::Posix, Style::Windows] {
        for fs in [FsKind::Local, FsKind::Remote] {
            assert_eq!(rule(&policy, style, fs, "/srv/app/.env", Access::Read), Some("env_file"));
        }
    }
    // A name that only contains the text is not an env file.
    assert_eq!(remote_rule("/srv/app/dotenv.rs", Access::Read), None);
    assert_eq!(remote_rule("/srv/app/environment.md", Access::Read), None);
    assert_eq!(remote_rule("/srv/.environments/readme", Access::Read), None);
}

#[test]
fn dot_segments_and_repeated_separators_cannot_reach_a_protected_path() {
    for path in [
        "/home/u/proj/../.ssh/id_rsa",
        "/home/u/proj/./../.ssh/x",
        "/home//u///.ssh/x",
        "/../../.ssh/x",
        "/home/u/.ssh/../.ssh/x",
        "/home/u/proj/src/../../.ssh",
        "//home/u/.ssh/./x/..",
        ".ssh/x",
        "../.ssh/x",
    ] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(remote_rule(path, access), Some("secret_location"), "{path} for {access:?}");
        }
    }
    // The spelling is applied, not just looked at: this is `/home/u/proj/main.rs`.
    assert_eq!(remote_rule("/home/u/.ssh/../proj/main.rs", Access::Write), None);
    assert_eq!(remote_rule("/home/u/proj/./src//main.rs", Access::Write), None);
}

#[test]
fn windows_style_paths_match_case_insensitively_and_ignore_trailing_dots_and_streams() {
    let policy = DenyPolicy::new(Vec::new());
    let check = |path: &str, access: Access| rule(&policy, Style::Windows, FsKind::Remote, path, access);
    for (path, expected) in [
        (r"C:\Users\me\.ssh\id_ed25519", "secret_location"),
        ("c:/users/ME/.SSH/config", "secret_location"),
        (r"\\?\C:\Users\me\.aws\credentials", "secret_location"),
        (r"C:\Users\me\.ssh.\id_rsa", "secret_location"),
        (r"C:\Users\me\.ssh \x", "secret_location"),
        (r"C:\Users\me\.SSH...\x", "secret_location"),
        (r"C:\proj\.env::$DATA", "env_file"),
        (r"C:\proj\.ENV.local", "env_file"),
        (r"C:\proj\.env.", "env_file"),
        (r"\\server\share\.aws\credentials", "secret_location"),
    ] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(check(path, access), Some(expected), "{path} for {access:?}");
        }
    }
    for (path, expected) in [
        (r"C:\Users\me\.ssh\..\.bashrc", "start_file"),
        (r"C:\Users\me\Documents\WindowsPowerShell\profile.ps1", "start_file"),
        (r"C:\proj\.GIT\HOOKS\pre-commit", "git_internals"),
        (r"C:\proj\.vscode\tasks.json", "editor_config"),
    ] {
        assert_eq!(check(path, Access::Read), None, "{path} may be read");
        assert_eq!(check(path, Access::Write), Some(expected), "{path} may not be written");
    }
    for path in [r"C:\Users\me\Documents\notes.txt", r"D:\work\sshkeys.txt", r"C:\Users\me\.ssh_notes\x"] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(check(path, access), None, "{path} for {access:?}");
        }
    }
}

#[test]
fn directories_the_shell_protects_are_refused_with_everything_below_them() {
    let policy = DenyPolicy::new(vec![PathBuf::from("/data/holzwave"), PathBuf::from("/run/user/1000/holzwave/ipc")]);
    for path in [
        "/data/holzwave",
        "/data/holzwave/settings.json",
        "/data/holzwave/ai/chats/x.json",
        "/DATA/HolzWave/x",
        "/data/holzwave/../holzwave/x",
        "/run/user/1000/holzwave/ipc/ipc.sock",
        "/run/user/1000/holzwave/ipc",
    ] {
        for access in [Access::Read, Access::Write] {
            assert_eq!(rule(&policy, Style::Posix, FsKind::Local, path, access), Some("protected_dir"), "{path}");
        }
    }
    for path in ["/data/holzwave-other/x", "/data", "/data/other/holzwave", "/run/user/1000/holzwave/other"] {
        assert_eq!(rule(&policy, Style::Posix, FsKind::Local, path, Access::Read), None, "{path}");
    }
    // The directories belong to this computer, not to a machine the user connected to.
    assert_eq!(rule(&policy, Style::Posix, FsKind::Remote, "/data/holzwave/settings.json", Access::Write), None);
    // The other rules still apply below a protected directory's neighbours, and an empty policy protects nothing.
    assert_eq!(rule(&DenyPolicy::new(Vec::new()), Style::Posix, FsKind::Local, "/data/holzwave/x", Access::Read), None);
    assert_eq!(rule(&DenyPolicy::new(vec![PathBuf::new()]), Style::Posix, FsKind::Local, "/x", Access::Read), None);
}

/// A tree for the symbolic link tests: `home/.ssh/config`, `data/settings.json`, `proj/`.
#[cfg(unix)]
fn link_tree(tag: &str) -> PathBuf {
    let root = common::scratch_dir(tag);
    fs::create_dir_all(root.join("home/.ssh")).unwrap();
    fs::write(root.join("home/.ssh/config"), "Host *").unwrap();
    fs::create_dir_all(root.join("data/ipc")).unwrap();
    fs::write(root.join("data/settings.json"), "{}").unwrap();
    fs::create_dir_all(root.join("proj/src")).unwrap();
    root
}

#[cfg(unix)]
#[test]
fn a_symlink_into_a_protected_directory_is_refused() {
    use std::os::unix::fs::symlink;
    let root = link_tree("deny-links");
    let canonical = fs::canonicalize(&root).unwrap();
    symlink(root.join("home/.ssh"), root.join("proj/link")).unwrap();
    symlink(root.join("home/.ssh/config"), root.join("proj/keylink")).unwrap();
    symlink(root.join("proj/link"), root.join("proj/chain")).unwrap();
    symlink("../home/.ssh", root.join("proj/relative")).unwrap();
    symlink(root.join("data"), root.join("proj/datalink")).unwrap();
    let policy = DenyPolicy::new(vec![root.join("data"), root.join("data/ipc")]);

    // Names that look harmless: the target is what counts.
    for path in ["proj/link/config", "proj/keylink", "proj/chain/config", "proj/relative/config", "proj/link/../.ssh/config"] {
        for access in [Access::Read, Access::Write] {
            let denied = policy.check_local(&root.join(path), access).expect_err(path);
            assert_eq!(denied.rule(), "secret_location", "{path} for {access:?}");
        }
    }
    // A link into a directory the shell protects.
    for path in ["proj/datalink", "proj/datalink/settings.json", "proj/datalink/ipc"] {
        let denied = policy.check_local(&root.join(path), Access::Read).expect_err(path);
        assert_eq!(denied.rule(), "protected_dir", "{path}");
    }
    // The same files without a link in between are refused by name or place, and an ordinary path resolves.
    assert!(policy.check_local(&root.join("home/.ssh/config"), Access::Read).is_err());
    assert_eq!(policy.check_local(&root.join("proj/src"), Access::Read).unwrap(), canonical.join("proj/src"));
    assert_eq!(policy.check_local(&root.join("home/.ssh/../config-none"), Access::Read).unwrap(), canonical.join("home/config-none"));
    fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn a_write_target_that_does_not_exist_yet_is_resolved_through_symlinks_first() {
    use std::os::unix::fs::symlink;
    let root = link_tree("deny-missing");
    let canonical = fs::canonicalize(&root).unwrap();
    symlink(root.join("home/.ssh"), root.join("proj/link")).unwrap();
    symlink(root.join("home/.ssh/authorized_keys"), root.join("proj/evil")).unwrap();
    symlink("loop_b", root.join("proj/loop_a")).unwrap();
    symlink("loop_a", root.join("proj/loop_b")).unwrap();
    symlink(root.join("proj/new_dir"), root.join("proj/to_missing")).unwrap();
    let policy = DenyPolicy::new(vec![root.join("data")]);

    // The parent resolves into `.ssh`, and the file is not there yet.
    let denied = policy.check_local(&root.join("proj/link/new_key"), Access::Write).unwrap_err();
    assert_eq!(denied.rule(), "secret_location");
    // A dangling link whose target would be created inside `.ssh`.
    for access in [Access::Write, Access::Read] {
        let denied = policy.check_local(&root.join("proj/evil"), access).unwrap_err();
        assert_eq!(denied.rule(), "secret_location", "{access:?}");
    }
    // A new file in an ordinary place is allowed and comes back resolved.
    let resolved = policy.check_local(&root.join("proj/new.txt"), Access::Write).unwrap();
    assert_eq!(resolved, canonical.join("proj/new.txt"));
    let resolved = policy.check_local(&root.join("proj/src/deep/er/new.txt"), Access::Write).unwrap();
    assert_eq!(resolved, canonical.join("proj/src/deep/er/new.txt"));
    // A dangling link into an ordinary place resolves to its target.
    let resolved = policy.check_local(&root.join("proj/to_missing"), Access::Write).unwrap();
    assert_eq!(resolved, canonical.join("proj/new_dir"));
    // A name that is missing and followed by `..` cannot be resolved, and neither can a relative path or a loop.
    for path in [root.join("proj/missing/../x"), root.join("proj/loop_a"), PathBuf::from("proj/new.txt"), PathBuf::new()] {
        let denied = policy.check_local(&path, Access::Write).unwrap_err();
        assert_eq!(denied.rule(), "unresolvable_path", "{path:?}");
    }
    // The protected data directory is refused for a file that does not exist either.
    let denied = policy.check_local(&root.join("data/ipc/new.sock"), Access::Write).unwrap_err();
    assert_eq!(denied.rule(), "protected_dir");
    fs::remove_dir_all(&root).unwrap();
}

// ---------------------------------------------------------------------------------------------------------------------
// The taint rule, the scope of an approval, cancel, limits.
// ---------------------------------------------------------------------------------------------------------------------

/// Proposes a call, approves it with the hash of its card, and runs it.
fn run_through(table: &ApprovalTable, host: &dyn ToolHost, scope: &Scope, call: &ToolCall) -> Result<ToolOutput, ToolError> {
    let pending = table.propose(host, scope, call)?;
    table.decide(&scope.conversation, &call.id, Decision::Allow, &pending.hash)?;
    table.run(host, scope, &call.id, &CancelToken::new())
}

fn sample_call(id: &str, tool: ToolName) -> ToolCall {
    call(id, tool.as_str(), sample(tool))
}

#[test]
fn reading_untrusted_content_taints_the_turn_and_marks_later_cards() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let turn = scope("c1", 1, 3);
    assert!(table.taint_labels(&turn).is_empty());

    let before = table.propose(&host, &turn, &read_call("before", NOTES)).unwrap();
    assert!(!before.tainted && before.taint.is_empty());
    table.taint(&turn, TaintSource::Context).unwrap();
    assert_eq!(table.taint_labels(&turn), ["attached context"]);
    let after = table.propose(&host, &turn, &read_call("after", NOTES)).unwrap();
    assert!(after.tainted);
    assert_eq!(after.taint, ["attached context"]);
    let listed = table.pending("c1");
    assert_eq!((listed[0].call_id.as_str(), listed[0].tainted), ("before", false), "an older card keeps what it knew");
    assert_eq!((listed[1].call_id.as_str(), listed[1].tainted), ("after", true));

    // Every read tool and the command tool taint the turn with their own label, each once.
    let mut expected = vec!["attached context".to_owned()];
    for (index, tool) in [ToolName::ReadFile, ToolName::ListDir, ToolName::Grep, ToolName::ReadPane, ToolName::RunCommand]
        .into_iter()
        .enumerate()
    {
        let args = if tool == ToolName::ReadFile { json!({ "path": NOTES }) } else { sample(tool) };
        run_through(&table, &host, &turn, &call(&format!("run_{index}"), tool.as_str(), args)).unwrap();
        expected.push(format!("output of {}", tool.as_str()));
        assert_eq!(table.taint_labels(&turn), expected);
    }
    run_through(&table, &host, &turn, &read_call("again", NOTES)).unwrap();
    assert_eq!(table.taint_labels(&turn), expected, "a label is listed once");

    // A write does not return untrusted content.
    run_through(&table, &host, &turn, &sample_call("write", ToolName::WriteFile)).unwrap();
    assert_eq!(table.taint_labels(&turn), expected);
    assert!(table.taint_labels(&turn).len() <= MAX_TAINT_LABELS);

    // A run that fails returns nothing, so it adds nothing.
    let other = scope("c1", 2, 3);
    let failed = run_through(&table, &host, &other, &read_call("missing", "/srv/app/missing.txt"));
    assert!(matches!(failed, Err(ToolError::Host(_))), "{failed:?}");
    assert!(table.taint_labels(&other).is_empty());
}

#[test]
fn a_side_effect_approval_does_not_survive_new_taint() {
    let host = FakeHost::remote();
    let table = table_with(&[]);
    let turn = scope("c1", 1, 3);
    let cancel = CancelToken::new();

    // Approved, then untrusted content arrives: the approval is void, whatever the tool.
    for (index, tool) in [ToolName::WriteFile, ToolName::EditFile, ToolName::RunCommand].into_iter().enumerate() {
        let id = format!("call_{index}");
        let pending = table.propose(&host, &turn, &sample_call(&id, tool)).unwrap();
        assert!(pending.effect.is_side_effect());
        table.decide("c1", &id, Decision::Allow, &pending.hash).unwrap();
        table.taint(&turn, TaintSource::ToolOutput(ToolName::ReadFile)).unwrap();
        assert_eq!(table.run(&host, &turn, &id, &cancel), Err(ToolError::TaintChanged), "{tool:?}");
        assert_eq!(host.run_count(), 0);
        assert_eq!(table.run(&host, &turn, &id, &cancel), Err(ToolError::UnknownCall), "the call is gone");
    }

    // A decision on a call that was proposed before the rise is void at once, and the waiting turn hears it.
    let pending = table.propose(&host, &turn, &sample_call("late", ToolName::WriteFile)).unwrap();
    table.taint(&turn, TaintSource::Context).unwrap();
    assert_eq!(table.decide("c1", "late", Decision::Allow, &pending.hash), Err(ToolError::TaintChanged));
    assert_eq!(table.wait("c1", "late", &cancel), Err(ToolError::TaintChanged));
    assert!(table.pending("c1").is_empty());
    assert_eq!(host.run_count(), 0);

    // A refusal is still a refusal.
    let pending = table.propose(&host, &turn, &sample_call("refused", ToolName::WriteFile)).unwrap();
    table.taint(&turn, TaintSource::Context).unwrap();
    table.decide("c1", "refused", Decision::Deny, &pending.hash).unwrap();
    assert_eq!(table.wait("c1", "refused", &cancel), Ok(Decision::Deny));

    // A call proposed after the rise is shown as tainted and can be approved and run.
    let pending = table.propose(&host, &turn, &sample_call("fresh", ToolName::WriteFile)).unwrap();
    assert!(pending.tainted);
    table.decide("c1", "fresh", Decision::Allow, &pending.hash).unwrap();
    assert!(table.run(&host, &turn, "fresh", &cancel).is_ok());
    assert_eq!(host.run_count(), 1);

    // The rise can also come while the host resolves the call for the second time.
    let table = Arc::new(table_with(&[]));
    let (hooked, watcher) = (Arc::clone(&table), turn.clone());
    let host = FakeHost::remote().on_resolve(move |ordinal| {
        if ordinal == 2 {
            hooked.taint(&watcher, TaintSource::Context).unwrap();
        }
    });
    let pending = table.propose(&host, &turn, &sample_call("race", ToolName::RunCommand)).unwrap();
    table.decide("c1", "race", Decision::Allow, &pending.hash).unwrap();
    assert_eq!(table.run(&host, &turn, "race", &cancel), Err(ToolError::TaintChanged));
    assert_eq!(host.run_count(), 0);
}

#[test]
fn a_read_approval_survives_new_taint() {
    let table = Arc::new(table_with(&[]));
    let turn = scope("c1", 1, 3);
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let pending = table.propose(&host, &turn, &read_call("read", NOTES)).unwrap();
    table.decide("c1", "read", Decision::Allow, &pending.hash).unwrap();
    table.taint(&turn, TaintSource::Context).unwrap();
    let output = table.run(&host, &turn, "read", &CancelToken::new()).expect("a read approval is not void");
    assert_eq!(output.text(), "alpha beta");

    // Also when the rise comes during the second resolution, and when it comes before the decision.
    let (hooked, watcher) = (Arc::clone(&table), turn.clone());
    let host = FakeHost::remote().with_file(NOTES, "alpha beta").on_resolve(move |ordinal| {
        if ordinal == 2 {
            hooked.taint(&watcher, TaintSource::ToolOutput(ToolName::Grep)).unwrap();
        }
    });
    let pending = table.propose(&host, &turn, &read_call("read2", NOTES)).unwrap();
    table.taint(&turn, TaintSource::Context).unwrap();
    table.decide("c1", "read2", Decision::Allow, &pending.hash).expect("a read decision is not void");
    assert!(table.run(&host, &turn, "read2", &CancelToken::new()).is_ok());
    assert_eq!(host.run_count(), 1);
}

#[test]
fn taint_belongs_to_one_turn_and_ends_with_it() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let turn = scope("c1", 1, 3);
    let cancel = CancelToken::new();
    let pending = table.propose(&host, &turn, &sample_call("w", ToolName::WriteFile)).unwrap();
    table.decide("c1", "w", Decision::Allow, &pending.hash).unwrap();

    // Another turn of the same conversation and another conversation do not touch this approval.
    table.taint(&scope("c1", 2, 3), TaintSource::Context).unwrap();
    table.taint(&scope("c2", 1, 3), TaintSource::Context).unwrap();
    assert!(table.taint_labels(&turn).is_empty());
    assert!(table.run(&host, &turn, "w", &cancel).is_ok());

    // The turn ends: its ledger and its leftovers are dropped, and a later turn with the same numbers starts clean.
    table.taint(&turn, TaintSource::Context).unwrap();
    assert_eq!(table.taint_labels(&turn), ["attached context"]);
    table.propose(&host, &turn, &read_call("left_over", NOTES)).unwrap();
    table.finish_turn(&turn);
    assert!(table.taint_labels(&turn).is_empty());
    assert!(table.pending("c1").is_empty(), "the leftover call is gone");
    assert_eq!(table.run(&host, &turn, "left_over", &cancel), Err(ToolError::UnknownCall));
    let again = table.propose(&host, &turn, &read_call("fresh", NOTES)).unwrap();
    assert!(!again.tainted && again.taint.is_empty());
    // The other turns kept theirs.
    assert_eq!(table.taint_labels(&scope("c1", 2, 3)), ["attached context"]);
    assert_eq!(table.taint_labels(&scope("c2", 1, 3)), ["attached context"]);
}

#[test]
fn an_approval_for_one_pane_cannot_run_in_another() {
    let cancel = CancelToken::new();
    let table = table_with(&[]);
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let turn = scope("c1", 1, 3);
    let pending = table.propose(&host, &turn, &read_call("call_1", NOTES)).unwrap();
    table.decide("c1", "call_1", Decision::Allow, &pending.hash).unwrap();

    assert_eq!(table.run(&host, &scope("c1", 1, 4), "call_1", &cancel), Err(ToolError::ScopeMismatch));
    assert_eq!(table.run(&host, &scope("c1", 2, 3), "call_1", &cancel), Err(ToolError::ScopeMismatch));
    assert_eq!(table.run(&host, &scope("c2", 1, 3), "call_1", &cancel), Err(ToolError::UnknownCall));
    assert_eq!(host.run_count(), 0);
    // The approval is still there for the turn it belongs to.
    assert!(table.run(&host, &turn, "call_1", &cancel).is_ok());

    // A host that answers another pane at execution is not the host that was approved.
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let knobs = host.knobs();
    let pending = table.propose(&host, &turn, &read_call("call_2", NOTES)).unwrap();
    table.decide("c1", "call_2", Decision::Allow, &pending.hash).unwrap();
    knobs.set_pane(4);
    assert_eq!(table.run(&host, &turn, "call_2", &cancel), Err(ToolError::PayloadChanged));
    assert_eq!(host.run_count(), 0);

    // And neither is one that names another machine.
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let knobs = host.knobs();
    let pending = table.propose(&host, &turn, &read_call("call_3", NOTES)).unwrap();
    table.decide("c1", "call_3", Decision::Allow, &pending.hash).unwrap();
    knobs.set_target("another box");
    assert_eq!(table.run(&host, &turn, "call_3", &cancel), Err(ToolError::PayloadChanged));
    assert_eq!(host.run_count(), 0);

    // The pane the call names decides where it acts, and the card says so.
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let pending = table
        .propose(&host, &turn, &call("call_4", "read_file", json!({ "path": NOTES, "pane": 9 })))
        .unwrap();
    assert_eq!(pending.pane, 9);
    assert!(pending.payload.iter().any(|f| f.name == "pane" && f.value == "9"));
}

/// Waits for `wait` on another thread, with a ceiling the test owns.
fn wait_on(table: &Arc<ApprovalTable>, conversation: &str, call_id: &str, cancel: &CancelToken) -> mpsc::Receiver<Result<Decision, ToolError>> {
    let (sender, receiver) = mpsc::channel();
    let (table, cancel) = (Arc::clone(table), cancel.clone());
    let (conversation, call_id) = (conversation.to_owned(), call_id.to_owned());
    thread::spawn(move || {
        let _ = sender.send(table.wait(&conversation, &call_id, &cancel));
    });
    receiver
}

#[test]
fn cancel_voids_the_open_calls_of_a_turn_a_pane_or_a_conversation() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = Arc::new(table_with(&[]));
    let never = CancelToken::new();
    let propose = |conversation: &str, turn: u64, pane: PaneId, id: &str| {
        table.propose(&host, &scope(conversation, turn, pane), &read_call(id, NOTES)).unwrap()
    };
    let a = propose("c1", 1, 3, "a");
    let b = propose("c1", 1, 3, "b");
    propose("c1", 2, 3, "c");
    propose("c1", 3, 4, "d");
    propose("c2", 1, 3, "e");
    table.decide("c1", "b", Decision::Allow, &b.hash).unwrap();
    let _ = a;

    // A turn: its waiting call and its approved call are voided, the others are not.
    let waiting = wait_on(&table, "c1", "a", &never);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(table.cancel_turn(&scope("c1", 1, 3)), 2);
    assert_eq!(waiting.recv_timeout(Duration::from_secs(5)).expect("the waiting turn ended"), Err(ToolError::Cancelled));
    assert_eq!(table.run(&host, &scope("c1", 1, 3), "b", &never), Err(ToolError::Cancelled), "an approved call is voided too");
    assert_eq!(host.run_count(), 0);
    let ids = |conversation: &str| table.pending(conversation).into_iter().map(|p| p.call_id).collect::<Vec<_>>();
    assert_eq!(ids("c1"), ["c", "d"]);
    assert_eq!(ids("c2"), ["e"]);
    assert_eq!(table.cancel_turn(&scope("c1", 1, 3)), 0, "nothing is left to void");

    // A pane: every turn that acts for it, in every conversation.
    assert_eq!(table.cancel_pane(3), 2);
    assert_eq!(ids("c1"), ["d"]);
    assert!(ids("c2").is_empty());
    assert_eq!(table.cancel_pane(3), 0);

    // A conversation.
    assert_eq!(table.cancel_conversation("c1"), 1);
    assert!(ids("c1").is_empty());
    assert_eq!(table.wait("c1", "d", &never), Err(ToolError::Cancelled));

    // The voided leftovers that nobody collected stay until their turn is finished.
    assert!(format!("{table:?}").contains("open: 2"), "{table:?}");
    for (conversation, turn) in [("c1", 2), ("c2", 1)] {
        table.finish_turn(&scope(conversation, turn, 3));
    }
    assert!(format!("{table:?}").contains("open: 0"), "{table:?}");

    // A call that is already running is left to its own token.
    let slow = Arc::new(FakeHost::remote().with_file(NOTES, "alpha beta").with_run_delay(Duration::from_millis(300)));
    let turn = scope("c1", 5, 3);
    let pending = table.propose(&*slow, &turn, &read_call("slow", NOTES)).unwrap();
    table.decide("c1", "slow", Decision::Allow, &pending.hash).unwrap();
    let runner = {
        let (table, slow, turn) = (Arc::clone(&table), Arc::clone(&slow), turn.clone());
        thread::spawn(move || table.run(&*slow, &turn, "slow", &CancelToken::new()))
    };
    assert!(common::wait_until(Duration::from_secs(5), || slow.started() == 1), "the call started");
    assert_eq!(table.cancel_turn(&turn), 0, "a running call is not voided");
    assert!(runner.join().unwrap().is_ok());
}

#[test]
fn the_number_of_open_calls_and_the_call_id_are_bounded() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let turn = scope("c1", 1, 3);
    let cancel = CancelToken::new();
    assert_eq!((MAX_OPEN_CALLS, MAX_CALL_ID_BYTES, MAX_TURNS), (16, 128, 64));

    for index in 0..MAX_OPEN_CALLS {
        table.propose(&host, &turn, &read_call(&format!("call_{index}"), NOTES)).expect("an open call below the limit");
    }
    let error = table.propose(&host, &turn, &read_call("one_too_many", NOTES)).unwrap_err();
    assert_eq!(error, ToolError::TooMany);
    assert_eq!(table.propose(&host, &turn, &read_call("call_3", NOTES)).unwrap_err(), ToolError::DuplicateCall);
    // Finishing one makes room for one.
    let first = table.pending("c1").remove(0);
    table.decide("c1", &first.call_id, Decision::Deny, &first.hash).unwrap();
    assert_eq!(table.wait("c1", &first.call_id, &cancel), Ok(Decision::Deny));
    table.propose(&host, &turn, &read_call("one_more", NOTES)).expect("room again");
    assert_eq!(table.propose(&host, &turn, &read_call("and_again", NOTES)).unwrap_err(), ToolError::TooMany);
    table.finish_turn(&turn);
    table.propose(&host, &turn, &read_call("after_finish", NOTES)).expect("room after the turn ended");

    // A repeated id in another conversation is another call.
    table.propose(&host, &scope("c2", 1, 3), &read_call("after_finish", NOTES)).expect("ids belong to a conversation");

    // The call id.
    let table = table_with(&[]);
    let long = "x".repeat(MAX_CALL_ID_BYTES + 1);
    for id in [long.as_str(), "", "two\nlines", "tab\there", "esc\u{1b}"] {
        let result = table.propose(&host, &turn, &read_call(id, NOTES));
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))), "{id:?}");
    }
    table.propose(&host, &turn, &read_call(&"x".repeat(MAX_CALL_ID_BYTES), NOTES)).expect("128 bytes are allowed");

    // Turns that hold state.
    let table = table_with(&[]);
    for index in 0..MAX_TURNS as u64 {
        table.taint(&scope("c1", index, 3), TaintSource::Context).expect("a turn below the limit");
    }
    assert_eq!(table.taint(&scope("c1", MAX_TURNS as u64, 3), TaintSource::Context), Err(ToolError::TooMany));
    let error = table.propose(&host, &scope("c1", MAX_TURNS as u64, 3), &read_call("late", NOTES)).unwrap_err();
    assert_eq!(error, ToolError::TooMany);
    table.taint(&scope("c1", 0, 3), TaintSource::ToolOutput(ToolName::Grep)).expect("a turn that has a ledger can go on");
    table.finish_turn(&scope("c1", 0, 3));
    table.taint(&scope("c1", MAX_TURNS as u64, 3), TaintSource::Context).expect("room after a turn ended");
}

#[test]
fn the_table_has_no_remembered_approval_and_a_decision_has_two_values() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let turn = scope("c1", 1, 3);
    let cancel = CancelToken::new();
    run_through(&table, &host, &turn, &read_call("first", NOTES)).unwrap();

    // The identical call under a new id is a new question.
    let again = table.propose(&host, &turn, &read_call("second", NOTES)).unwrap();
    assert_eq!(table.pending("c1").len(), 1);
    assert_eq!(table.run(&host, &turn, "second", &cancel), Err(ToolError::NotApproved));
    assert_eq!(host.run_count(), 1);
    // And the same id again, once the first is done, is a new call too.
    table.decide("c1", "second", Decision::Allow, &again.hash).unwrap();
    assert!(table.run(&host, &turn, "second", &cancel).is_ok());
    assert_eq!(run_through(&table, &host, &turn, &read_call("first", NOTES)).unwrap().text(), "alpha beta");
    assert_eq!(host.run_count(), 3);

    // Two values, checked by the compiler: adding a third breaks this match.
    fn arms(decision: Decision) -> u8 {
        match decision {
            Decision::Allow => 1,
            Decision::Deny => 2,
        }
    }
    assert_eq!((arms(Decision::Allow), arms(Decision::Deny)), (1, 2));
    assert_eq!(serde_json::to_string(&Decision::Allow).unwrap(), "\"allow\"");
    assert_eq!(serde_json::to_string(&Decision::Deny).unwrap(), "\"deny\"");
    assert_eq!(serde_json::from_str::<Decision>("\"allow\"").unwrap(), Decision::Allow);
    for text in ["always", "remember", "session", "allow_always", "allow_for_session", "Allow", "ALLOW", "yes", "true", ""] {
        assert!(serde_json::from_str::<Decision>(&format!("\"{text}\"")).is_err(), "{text:?} is not a decision");
    }
    assert!(serde_json::from_str::<Decision>("true").is_err() && serde_json::from_str::<Decision>("1").is_err());
}

#[test]
fn the_agent_cannot_write_its_own_configuration() {
    let root = common::scratch_dir("agent-config");
    for dir in ["data/ipc", "home/.ssh", "proj/.git/hooks", "proj/.vscode", "proj/src"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for file in [
        "data/settings.json",
        "data/ai.json",
        "home/.zshrc",
        "home/.ssh/config",
        "proj/.git/hooks/pre-commit",
        "proj/.vscode/tasks.json",
        "proj/.env",
        "proj/src/lib.rs",
    ] {
        fs::write(root.join(file), "x").unwrap();
    }
    let table = ApprovalTable::new(DenyPolicy::new(vec![root.join("data"), root.join("data/ipc")]));
    let host = FakeHost::local();
    let turn = scope("c1", 1, 3);
    let text = |relative: &str| root.join(relative).to_str().unwrap().to_owned();

    for (index, relative) in [
        "data/settings.json",
        "data/ai.json",
        "data/ipc/ipc.sock",
        "home/.zshrc",
        "home/.ssh/config",
        "proj/.git/hooks/pre-commit",
        "proj/.vscode/tasks.json",
        "proj/.env",
    ]
    .into_iter()
    .enumerate()
    {
        let path = text(relative);
        let write = call(&format!("w{index}"), "write_file", json!({ "path": path, "content": "x" }));
        let edit = call(&format!("e{index}"), "edit_file", json!({ "path": path, "old_text": "x", "new_text": "y" }));
        for attempt in [write, edit] {
            let result = table.propose(&host, &turn, &attempt);
            assert!(matches!(result, Err(ToolError::Denied(_))), "{} {relative}: {result:?}", attempt.name);
        }
    }
    for relative in ["data/ai.json", "home/.ssh/config", "proj/.env"] {
        let result = table.propose(&host, &turn, &read_call("r", &text(relative)));
        assert!(matches!(result, Err(ToolError::Denied(_))), "read {relative}: {result:?}");
    }
    // Reading a start file and an ordinary source file is a card.
    for (index, relative) in ["home/.zshrc", "proj/src/lib.rs"].into_iter().enumerate() {
        let pending = table.propose(&host, &turn, &read_call(&format!("ok{index}"), &text(relative))).expect(relative);
        assert!(pending.payload.iter().any(|f| f.name == "resolved_path"));
    }
    // A command may not start in the places the file tools may not enter.
    let command = |cwd: &str| call("cmd", "run_command", json!({ "command": "ls", "cwd": cwd }));
    let result = table.propose(&host, &turn, &command(&text("home/.ssh")));
    assert!(matches!(result, Err(ToolError::Denied(_))), "{result:?}");
    table.propose(&host, &turn, &command(&text("proj"))).expect("an ordinary directory is fine");
    assert_eq!(host.run_count(), 0);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn wsh_and_the_ipc_crate_cannot_reach_the_approval_table() {
    let manifest = |name: &str| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name).join("Cargo.toml");
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let (ipc, wsh) = (manifest("ipc"), manifest("wsh"));
    assert!(ipc.contains("name = \"holzwave-ipc\"") && wsh.contains("holzwave-ipc"), "the manifests were read");
    for (name, text) in [("ipc", &ipc), ("wsh", &wsh)] {
        let code: String = text.lines().filter(|line| !line.trim_start().starts_with('#')).collect::<Vec<_>>().join("\n");
        assert!(!code.contains("holzwave-core") && !code.contains("holzwave_core"), "the {name} manifest names the core");
    }
}

#[test]
fn the_open_calls_of_a_conversation_can_be_listed_again_after_a_reload() {
    let host = Arc::new(FakeHost::remote().with_file(NOTES, "alpha beta").with_run_delay(Duration::from_millis(200)));
    let table = Arc::new(table_with(&[]));
    let turn = scope("c1", 1, 3);
    // Proposal order is not the order of the ids.
    let proposed: Vec<Pending> = ["z_call", "a_call", "m_call"]
        .into_iter()
        .map(|id| table.propose(&*host, &turn, &read_call(id, NOTES)).unwrap())
        .collect();
    let other = table.propose(&*host, &scope("c2", 1, 3), &read_call("x_call", NOTES)).unwrap();
    // One is decided, one is running.
    table.decide("c1", "a_call", Decision::Allow, &proposed[1].hash).unwrap();
    table.decide("c1", "m_call", Decision::Allow, &proposed[2].hash).unwrap();
    let runner = {
        let (table, host, turn) = (Arc::clone(&table), Arc::clone(&host), turn.clone());
        thread::spawn(move || table.run(&*host, &turn, "m_call", &CancelToken::new()))
    };
    assert!(common::wait_until(Duration::from_secs(5), || host.started() == 1));

    let listed = table.pending("c1");
    assert_eq!(listed.iter().map(|p| p.call_id.as_str()).collect::<Vec<_>>(), ["z_call"]);
    assert_eq!(serde_json::to_value(&listed[0]).unwrap(), serde_json::to_value(&proposed[0]).unwrap());
    assert_eq!(table.pending("c2").len(), 1);
    assert_eq!(table.pending("c2")[0].hash, other.hash);
    assert!(table.pending("nobody").is_empty());
    runner.join().unwrap().unwrap();

    // Two awaiting calls come back in the order they were proposed, with the payload they were proposed with.
    let again: Vec<Pending> = ["y_call", "b_call"]
        .into_iter()
        .map(|id| table.propose(&*host, &scope("c3", 1, 3), &read_call(id, NOTES)).unwrap())
        .collect();
    let listed = table.pending("c3");
    assert_eq!(listed.iter().map(|p| p.call_id.as_str()).collect::<Vec<_>>(), ["y_call", "b_call"]);
    for (listed, proposed) in listed.iter().zip(&again) {
        assert_eq!(serde_json::to_string(listed).unwrap(), serde_json::to_string(proposed).unwrap());
    }
}

#[test]
fn the_card_data_serialises_to_a_pinned_shape() {
    let host = FakeHost::remote().with_file(NOTES, "alpha beta");
    let table = table_with(&[]);
    let pending = table.propose(&host, &scope("c1", 1, 3), &read_call("call_1", NOTES)).unwrap();
    let field = |name: &str, value: &str| format!("{{\"name\":\"{name}\",\"value\":\"{value}\",\"bytes\":{}}}", value.len());
    let payload = [
        field("tool", "read_file"),
        field("pane", "3"),
        field("target", "build box"),
        field("remote", "true"),
        field("path", NOTES),
        field("offset", "0"),
        field("limit", "16384"),
        field("resolved_path", NOTES),
    ]
    .join(",");
    let expected = format!(
        "{{\"conversation\":\"c1\",\"call_id\":\"call_1\",\"tool\":\"read_file\",\"effect\":\"read\",\"pane\":3,\
         \"target\":\"build box\",\"remote\":true,\"payload\":[{payload}],\"hash\":\"{}\",\"tainted\":false,\"taint\":[]}}",
        pending.hash
    );
    assert_eq!(serde_json::to_string(&pending).unwrap(), expected);
    assert_eq!(pending.effect, Effect::Read);

    let value = serde_json::to_value(&pending).unwrap();
    let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(sorted.len(), 11, "{keys:?}");
    assert!(value["payload"].as_array().unwrap().iter().all(|f| f.as_object().unwrap().len() == 3));

    // A tainted card says so.
    let turn = scope("c1", 1, 3);
    table.taint(&turn, TaintSource::Context).unwrap();
    let tainted = table.propose(&host, &turn, &read_call("call_2", NOTES)).unwrap();
    let value = serde_json::to_value(&tainted).unwrap();
    assert_eq!((value["tainted"].clone(), value["taint"].clone()), (json!(true), json!(["attached context"])));
}

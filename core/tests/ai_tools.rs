//! The approval path of the agent's tools, with no network, no terminal and no page.
//!
//! A fake host stands in for the shell. A call goes through every layer the shell will use: typed arguments, host
//! resolution, the deny check, a hash over the exact payload, a card, a decision that carries that hash, a second
//! resolution and a second hash at execution, and only then a ticket for the host. Every wait is bounded, so a broken
//! table fails an assertion instead of hanging the run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::tools::{
    ApprovalTable, Decision, DenyPolicy, FsKind, Resolved, Scope, Ticket, Tool, ToolError, ToolHost, ToolName,
    ToolOutput, denied_result,
};
use holzwave_core::ai::types::ToolCall;
use holzwave_core::layout::PaneId;
use serde_json::{Value, json};

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

    fn resolve_count(&self) -> usize {
        self.state().resolve_count
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
        let pane = tool.pane().unwrap_or(scope.pane);
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
        let delay = self.state().run_delay;
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        let mut state = self.state();
        let path = ticket.resolved().path().map(str::to_owned);
        state.ran.push(Ran { call_id: ticket.call_id().to_owned(), tool: ticket.tool().name(), path: path.clone() });
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

    // The thirteen variants without a detail say the same thing whatever call they came from.
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

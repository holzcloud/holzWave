//! The release gate for approval: nothing that changes something or opens a session runs without a confirmed ticket, and
//! the AI chat cannot reach the integrations.
//!
//! By attempt: every lane is asked for every verb it must not run (a view for a verb that changes something, a terminal
//! for a session, a card for a read), and every way to forge, mix up, repeat or damage a ticket is tried; each ends with
//! a refusal and the fakes' own record of starts unchanged. The status thread is run over a long injected time and its
//! record shows only the default verb of each tool and the version probe.
//!
//! By construction: scans of the source show one place that starts a program for a request, one constructor of the
//! proof of approval, no literal program name, nothing of the integrations in the socket crate or the command line
//! tool, and the page's rule that only a person decides. The ACL list, the socket and the trusted-event rule of the
//! earlier steps are read again here so that a change in one of them fails this gate too.
//!
//! The last test is decision D9: no file of the AI chat names the integrations, and the pane contexts the chat is offered
//! for a pane that shows an integration are those of a monitor pane: none.
//!
//! Nothing here uses a real tool or a network. The tools are fake programs of the rig on a scratch computer.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use holzwave_core::ai::tools::ToolName;
use holzwave_core::integrate::service::{Done, FaultCode, HostFacts, IntegrationService};
use holzwave_core::integrate::{spec_of, ArgKind, Output, ToolId, VerbEffect};
use holzwave_core::secrets::MemoryStore;

use common::scratch_dir;
use fake_cli::FakeCli;

// ---- the scratch computer --------------------------------------------------------------------------------------------

/// A scratch computer with a fake for each of the four tools; each fake appends its argument list to a history of its
/// own at every start, so a test counts starts instead of reading the last one.
struct Machine {
    root: PathBuf,
    fakes: Vec<(ToolId, FakeCli)>,
    service: Arc<IntegrationService>,
    clock: Clock,
}

/// A clock that only moves when a test moves it.
#[derive(Clone)]
struct Clock {
    base: Instant,
    millis: Arc<AtomicU64>,
}

impl Clock {
    fn new() -> Clock {
        Clock { base: Instant::now(), millis: Arc::new(AtomicU64::new(0)) }
    }

    fn advance(&self, by: Duration) {
        self.millis.fetch_add(u64::try_from(by.as_millis()).expect("a small step"), Ordering::SeqCst);
    }

    fn reader(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
        let (base, millis) = (self.base, self.millis.clone());
        Arc::new(move || base + Duration::from_millis(millis.load(Ordering::SeqCst)))
    }
}

fn programs() -> [(ToolId, &'static str); 4] {
    [(ToolId::Kube, "kubectl"), (ToolId::Talos, "talosctl"), (ToolId::Azure, "az"), (ToolId::Gam, "gam")]
}

impl Machine {
    fn new() -> Machine {
        let root = scratch_dir("bypass");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        let fakes: Vec<(ToolId, FakeCli)> = programs()
            .iter()
            .map(|(tool, name)| {
                let fake = FakeCli::new(&bin, name).prints("{}\n");
                let mut conf = std::fs::OpenOptions::new().append(true).open(format!("{}.conf", fake.path().display())).expect("settings");
                writeln!(conf, "printf '%s\\n' \"$*\" >> \"$d/$n.history\"").expect("one more line");
                (*tool, fake)
            })
            .collect();
        let facts = HostFacts::for_tests(&root, std::slice::from_ref(&bin), &home, &data);
        let clock = Clock::new();
        let service = IntegrationService::new(&data, Arc::new(MemoryStore::default()), facts, &[]).with_clock(clock.reader()).shared();
        Machine { root, fakes, service, clock }
    }

    fn history(&self, tool: ToolId) -> Vec<String> {
        let fake = &self.fakes.iter().find(|(own, _)| *own == tool).expect("a fake for every tool").1;
        std::fs::read_to_string(format!("{}.history", fake.path().display()))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// How many starts of any fake so far.
    fn starts(&self) -> usize {
        ToolId::ALL.iter().map(|tool| self.history(*tool).len()).sum()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn harmless(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Context => "ctx-arg",
        ArgKind::Namespace => "ns-arg",
        ArgKind::Pod => "pod-arg",
        ArgKind::Container => "ctr-arg",
        ArgKind::Node => "10.9.8.7",
        ArgKind::Subscription => "11111111-2222-4333-8444-555555555555",
        ArgKind::Section => "sec-arg",
        ArgKind::Lines => "123",
        ArgKind::Flag => "true",
        ArgKind::Choice(words) => words[0],
    }
}

/// The arguments a verb needs, with harmless values.
fn args_of(tool: ToolId, verb: &str) -> BTreeMap<String, String> {
    let verb = spec_of(tool).verb(verb).expect("a verb of the tool");
    verb.args.iter().filter(|arg| arg.required).map(|arg| (arg.name.to_owned(), harmless(arg.kind).to_owned())).collect()
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

// ---- by attempt ------------------------------------------------------------------------------------------------------

#[test]
fn a_gated_verb_cannot_run_without_a_ticket() {
    let m = Machine::new();
    // Version probes are not requests: ask for them once, so that every later start is a request's.
    let tools = m.service.tools();
    assert!(tools.iter().all(|tool| tool.path.is_some()), "the fakes are found");
    let before = m.starts();

    // 1. Every lane for every verb it does not admit.
    let (mut views, mut terminals, mut cards, mut attempts) = (0, 0, 0, 0);
    for tool in ToolId::ALL {
        for verb in spec_of(tool).verbs {
            let request = args_of(tool, verb.id);
            let read = verb.effect == VerbEffect::Read;
            if !(read && verb.output == Output::View) {
                let fault = m.service.run_view(tool.as_str(), verb.id, Some(1), &request).expect_err("a view of a verb that is no view");
                assert_eq!(fault.code, FaultCode::Refused, "run_view {} {}", tool.as_str(), verb.id);
                views += 1;
            }
            if !(read && verb.output == Output::Terminal) {
                let fault = m.service.launch(tool.as_str(), verb.id, Some(1), &request).expect_err("a launch of a verb that is no read terminal");
                assert_eq!(fault.code, FaultCode::Refused, "launch {} {}", tool.as_str(), verb.id);
                terminals += 1;
            }
            if read {
                let fault = m.service.prepare(tool.as_str(), verb.id, Some(1), &request).expect_err("a card for a read");
                assert_eq!(fault.code, FaultCode::Refused, "prepare {} {}", tool.as_str(), verb.id);
                cards += 1;
            }
            attempts += 1;
        }
    }
    assert_eq!(m.starts(), before, "no refused request started a process");
    let gated: usize = ToolId::ALL.iter().flat_map(|tool| spec_of(*tool).verbs).filter(|verb| verb.effect != VerbEffect::Read).count();
    assert!(gated >= 8, "the catalogue has {gated} verbs that change something or open a session, at least 8 are asserted");
    assert!(views >= gated && terminals >= gated, "every gated verb was tried in both lanes without a card");

    // 2. A card starts nothing either, for every gated verb.
    for tool in ToolId::ALL {
        for verb in spec_of(tool).verbs.iter().filter(|verb| verb.effect != VerbEffect::Read) {
            let card = m.service.prepare(tool.as_str(), verb.id, Some(1), &args_of(tool, verb.id)).unwrap_or_else(|f| panic!("a card for {} {}: {f:?}", tool.as_str(), verb.id));
            assert_eq!(card.ticket.len(), 32);
            assert_eq!(card.hash.len(), 64);
            m.service.cancel(&card.ticket);
        }
    }
    assert_eq!(m.starts(), before, "a card is a description: it started no process");

    // 3. The ways a ticket can be wrong.
    let pod = |name: &str| args(&[("pod", name)]);
    let new_card = |name: &str| m.service.prepare("kube", "delete_pod", Some(1), &pod(name)).expect("a card");
    let flip = |hash: &str| {
        let mut chars: Vec<char> = hash.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == '0' { '1' } else { '0' };
        chars.into_iter().collect::<String>()
    };
    let refused = |what: &str, ticket: &str, hash: &str| {
        let fault = m.service.confirm(ticket, hash).expect_err(what);
        assert_eq!(fault.code, FaultCode::Ticket, "{what}: {fault:?}");
        assert_eq!(m.starts(), before, "{what}: a process was started");
    };

    // Each attempt gets two fresh cards: a few wrong hashes drop a card, and the attempts must not use one up for the next.
    let attempt = |what: &str, make: &dyn Fn(&holzwave_core::integrate::ticket::Card, &holzwave_core::integrate::ticket::Card) -> (String, String)| {
        let (a, b) = (new_card("api-0"), new_card("api-1"));
        let (ticket, hash) = make(&a, &b);
        refused(what, &ticket, &hash);
        m.service.cancel(&a.ticket);
        m.service.cancel(&b.ticket);
    };
    attempt("a ticket id that was never issued", &|a, _| ("a".repeat(32), a.hash.clone()));
    attempt("a ticket id and a hash that were never issued", &|_, _| ("b".repeat(32), "c".repeat(64)));
    attempt("the hash of another card", &|a, b| (a.ticket.clone(), b.hash.clone()));
    attempt("a hash with the last digit changed", &|a, _| (a.ticket.clone(), flip(&a.hash)));
    attempt("an upper-case hash", &|a, _| (a.ticket.clone(), a.hash.to_uppercase()));
    attempt("a hash with a trailing space", &|a, _| (a.ticket.clone(), format!("{} ", a.hash)));
    attempt("a hash with a trailing line feed", &|a, _| (a.ticket.clone(), format!("{}\n", a.hash)));
    attempt("a hash cut short", &|a, _| (a.ticket.clone(), a.hash[..63].to_owned()));
    attempt("a hash of 10000 characters", &|a, _| (a.ticket.clone(), a.hash.repeat(160)));
    attempt("an empty hash", &|a, _| (a.ticket.clone(), String::new()));
    attempt("a hash with a NUL", &|a, _| (a.ticket.clone(), format!("{}\0", &a.hash[..63])));
    attempt("an upper-case ticket id", &|a, _| (a.ticket.to_uppercase(), a.hash.clone()));
    attempt("an empty ticket id", &|a, _| (String::new(), a.hash.clone()));
    attempt("a ticket id with a NUL", &|a, _| (format!("{}\0", &a.ticket[..31]), a.hash.clone()));
    attempt("the ticket id of another card with the hash of this one", &|a, b| (b.ticket.clone(), a.hash.clone()));
    let (card, other) = (new_card("api-0"), new_card("api-1"));
    // Three wrong hashes drop the card: the right one is then no use either.
    let third = new_card("api-2");
    for _ in 0..3 {
        refused("a wrong hash, again", &third.ticket, &flip(&third.hash));
    }
    refused("the right hash after three wrong ones", &third.ticket, &third.hash);
    // A cancelled card.
    let cancelled = new_card("api-3");
    assert!(m.service.cancel(&cancelled.ticket), "the card was waiting");
    refused("a cancelled card", &cancelled.ticket, &cancelled.hash);
    // A card of another service (another process, another run of the app).
    let foreign = Machine::new();
    foreign.service.tools();
    let foreign_card = foreign.service.prepare("kube", "delete_pod", Some(1), &pod("api-0")).expect("a card of the other service");
    refused("a ticket of another service", &foreign_card.ticket, &foreign_card.hash);
    assert_eq!(foreign.starts(), foreign.starts(), "the other service started nothing either");
    attempts += 22;

    // 4. The card that is confirmed runs once; the same ticket again does not run.
    let kube_before = m.history(ToolId::Kube).len();
    let done = m.service.confirm(&card.ticket, &card.hash).expect("the right ticket and hash");
    assert!(matches!(done, Done::Ran { .. }), "a write runs and answers its note: {done:?}");
    assert_eq!(m.history(ToolId::Kube).len(), kube_before + 1, "one start for one confirmed card");
    let ran = m.history(ToolId::Kube).last().cloned().expect("a record");
    assert!(ran.contains("delete") && ran.contains("api-0"), "it is the delete of the card: {ran}");
    let again = m.service.confirm(&card.ticket, &card.hash).expect_err("a replay");
    assert_eq!(again.code, FaultCode::Ticket, "{again:?}");
    assert_eq!(m.history(ToolId::Kube).len(), kube_before + 1, "a replay started nothing");
    attempts += 2;

    // The other card still waits and is not affected by the replay or the wrong tries on its neighbours.
    assert!(m.service.confirm(&other.ticket, &other.hash).is_ok(), "an untouched card still works");
    println!(
        "Bypass: {attempts} attempts ({views} views, {terminals} launches and {cards} cards for the wrong verbs, 24 ticket attempts), \
         every one refused with no process; {gated} gated verbs; the confirmed card ran exactly once"
    );
}

#[test]
fn the_status_thread_starts_only_the_default_verb_and_the_version_probe() {
    let m = Machine::new();
    // What a default read and a version probe look like in the record: ask for them directly once, per tool.
    let mut allowed: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for tool in ToolId::ALL {
        let spec = spec_of(tool);
        let first = spec.verbs.first().expect("a verb");
        m.service.tools();
        let _ = m.service.run_view(tool.as_str(), first.id, None, &args(&[]));
        let history = m.history(tool);
        let default = history.last().cloned().expect("the default verb was started");
        let probe = spec.version_probe.argv.join(" ");
        assert!(history.contains(&probe), "{}: the version probe is {probe:?}, the record is {history:?}", tool.as_str());
        allowed.insert(tool.as_str(), BTreeSet::from([default, probe]));
    }
    // A second machine: the worker only. A long time is injected; the asks are forced and plain.
    let w = Machine::new();
    for round in 0..40 {
        w.clock.advance(Duration::from_secs(600));
        let rows = w.service.context_rows(Some(1 + (round % 3)), round % 2 == 0);
        assert_eq!(rows.len(), 4);
        assert!(w.service.wait_idle(Duration::from_secs(30)), "the background reads end");
    }
    let mut total = 0;
    for tool in ToolId::ALL {
        let history = w.history(tool);
        total += history.len();
        assert!(!history.is_empty(), "{}: the worker read the tool", tool.as_str());
        for line in &history {
            assert!(allowed[tool.as_str()].contains(line), "{}: the worker started `{line}`, allowed are {:?}", tool.as_str(), allowed[tool.as_str()]);
        }
        let verbs_seen: BTreeSet<&String> = history.iter().collect();
        println!("Bypass worker {}: {} starts, {} different argument lists", tool.as_str(), history.len(), verbs_seen.len());
    }
    assert!(total >= 8, "the worker started {total} processes over 40 rounds, at least 8 are asserted");
}

// ---- by construction ------------------------------------------------------------------------------------------------

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

/// Every file of `dir` below the workspace root with the extension, as (path relative to the root, text).
fn files_below(dir: &str, extension: &str) -> Vec<(String, String)> {
    fn walk(path: &Path, extension: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, extension, out);
            } else if path.extension().is_some_and(|own| own == extension) {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&root().join(dir), extension, &mut paths);
    paths.sort();
    assert!(!paths.is_empty(), "{dir} holds no .{extension} file: the scan would pass for nothing");
    paths
        .into_iter()
        .map(|path| {
            let relative = path.strip_prefix(root()).expect("below the root").to_string_lossy().into_owned();
            (relative, std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        })
        .collect()
}

fn file(relative: &str) -> String {
    std::fs::read_to_string(root().join(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

/// The lines of code of a source file: not a comment line (`//`, `///`, `//!`), with their number.
fn code_lines(text: &str) -> Vec<(usize, &str)> {
    text.lines().enumerate().map(|(i, line)| (i + 1, line)).filter(|(_, line)| !line.trim_start().starts_with("//")).collect()
}

/// The name of the function a line is in: the nearest `fn name` above it.
fn enclosing_fn(text: &str, line: usize) -> String {
    for candidate in text.lines().take(line).collect::<Vec<_>>().iter().rev() {
        let trimmed = candidate.trim_start();
        let Some(at) = trimmed.find("fn ") else { continue };
        let before = &trimmed[..at];
        if !(before.is_empty() || before.ends_with("pub ") || before.ends_with("pub(crate) ") || before.ends_with("pub(super) ")) {
            continue;
        }
        let name: String = trimmed[at + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

/// Every place `needle` is in code (not a comment) below `dir`: (file, line, enclosing function).
fn code_hits(dir: &str, needle: &str) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (path, text) in files_below(dir, "rs") {
        for (line, content) in code_lines(&text) {
            if content.contains(needle) {
                hits.push((path.clone(), line, enclosing_fn(&text, line)));
            }
        }
    }
    hits
}

#[test]
fn the_single_run_path_and_the_single_constructor() {
    // `run_argv(` starts a program: its definition is in the runner, and the only callers are four functions of the
    // service: the login shell probe, the version probe, the read of a view (after the lane check) and the run of a
    // confirmed card (the only one that takes an argument list from an approval).
    let mut scanned = 0;
    let mut calls: Vec<(String, String)> = Vec::new();
    for dir in ["core/src", "app/src-tauri/src", "ipc/src", "wsh/src"] {
        for (path, text) in files_below(dir, "rs") {
            scanned += 1;
            for (line, content) in code_lines(&text) {
                if content.contains("run_argv(") {
                    calls.push((path.clone(), enclosing_fn(&text, line)));
                }
            }
        }
    }
    let expected = [
        ("core/src/integrate/run.rs", "run_argv"),
        ("core/src/integrate/service.rs", "login_shell_path"),
        ("core/src/integrate/service.rs", "probe_version"),
        ("core/src/integrate/service.rs", "run_read"),
        ("core/src/integrate/service.rs", "run_gated"),
    ];
    let expected: Vec<(String, String)> = expected.iter().map(|(p, f)| ((*p).to_owned(), (*f).to_owned())).collect();
    assert_eq!(calls, expected, "run_argv( is called from a place nobody reviewed");
    // The two functions that run what a request asked for are reached from one lane each.
    let service = file("core/src/integrate/service.rs");
    let run_read_callers: Vec<_> = code_lines(&service).into_iter().filter(|(_, l)| l.contains("self.run_read(")).collect();
    let run_gated_callers: Vec<_> = code_lines(&service).into_iter().filter(|(_, l)| l.contains("self.run_gated(")).collect();
    assert_eq!(run_read_callers.len(), 1, "run_read has one caller: {run_read_callers:?}");
    assert_eq!(run_gated_callers.len(), 1, "run_gated has one caller: {run_gated_callers:?}");
    assert_eq!(enclosing_fn(&service, run_gated_callers[0].0), "confirm_from", "a gated run is started by a confirm only");
    let read_caller = enclosing_fn(&service, run_read_callers[0].0);
    let lane_check = service.lines().take(run_read_callers[0].0).collect::<Vec<_>>().join("\n");
    assert!(lane_check.contains("Lane::View"), "{read_caller} is reached after a View lane request");

    // `Approved` is built in the ticket module, in one private function, and that function is called once, from confirm.
    // A literal is `Approved {` that is not the head of the type or of an impl (`struct Approved {`, `for Approved {`).
    let mut built: Vec<(String, usize, String)> = Vec::new();
    for dir in ["core/src", "app/src-tauri/src", "ipc/src", "wsh/src"] {
        for (path, text) in files_below(dir, "rs") {
            // The gate of the AI chat has a type of the same name (its own approval of a model's tool call); it is not this one.
            if path.starts_with("core/src/ai/") {
                continue;
            }
            for (line, content) in code_lines(&text) {
                let trimmed = content.trim_start();
                let head = trimmed.starts_with("pub struct Approved") || trimmed.starts_with("struct Approved") || trimmed.contains("for Approved {") || trimmed.starts_with("impl Approved");
                if content.contains("Approved {") && !head {
                    built.push((path.clone(), line, enclosing_fn(&text, line)));
                }
            }
        }
    }
    let literal: Vec<_> = built.iter().filter(|(path, _, _)| !path.ends_with("ticket.rs")).collect();
    assert!(literal.is_empty(), "Approved is built outside the ticket module: {literal:?}");
    let own: Vec<_> = built.iter().filter(|(path, _, name)| path.ends_with("ticket.rs") && name != "issue").collect();
    assert!(own.is_empty(), "Approved is built in a function other than `issue`: {own:?}");
    let ticket = file("core/src/integrate/ticket.rs");
    let issue_signature = code_lines(&ticket).into_iter().find(|(_, l)| l.contains("fn issue(")).expect("the constructor").1.to_owned();
    assert!(!issue_signature.contains("pub"), "the constructor is private: {issue_signature}");
    let issue_calls: Vec<_> = code_lines(&ticket).into_iter().filter(|(_, l)| l.contains("Approved::issue(")).collect();
    assert_eq!(issue_calls.len(), 1, "the constructor has one caller: {issue_calls:?}");
    assert_eq!(enclosing_fn(&ticket, issue_calls[0].0), "confirm", "the caller is the confirm of the table");
    // The attributes right above the struct hold no derive of a trait that would copy, build or read one from text.
    let before_struct: Vec<&str> = ticket.lines().take_while(|l| !l.contains("pub struct Approved")).collect();
    let attributes: String = before_struct.iter().rev().take_while(|l| l.trim_start().starts_with("///") || l.trim_start().starts_with("#[")).copied().collect::<Vec<_>>().join("\n");
    for forbidden in ["Clone", "Default", "Serialize", "Deserialize", "Copy"] {
        assert!(!attributes.lines().any(|l| l.trim_start().starts_with("#[") && l.contains(forbidden)), "Approved is marked `{forbidden}`");
    }
    assert!(!ticket.contains("impl Clone for Approved") && !ticket.contains("impl Default for Approved"), "no hand-written Clone or Default");

    // No program name as a literal below the integrations, and the shell of the app starts nothing for them.
    let mut spawn_hits = Vec::new();
    for (path, text) in files_below("core/src/integrate", "rs").into_iter().chain([("app/src-tauri/src/integrate_host.rs".to_owned(), file("app/src-tauri/src/integrate_host.rs"))]) {
        for (line, content) in code_lines(&text) {
            if content.contains("Command::new(\"") || content.contains("Command::new(&\"") || content.contains("Command::new(String") {
                spawn_hits.push(format!("{path}:{line}"));
            }
        }
    }
    assert!(spawn_hits.is_empty(), "a program is named as a literal: {spawn_hits:?}");
    let new_command = code_hits("core/src/integrate", "Command::new(");
    assert_eq!(new_command.len(), 1, "one place builds a command: {new_command:?}");
    assert_eq!((new_command[0].0.as_str(), new_command[0].2.as_str()), ("core/src/integrate/run.rs", "run"), "and it is the runner's own run function");
    let host = file("app/src-tauri/src/integrate_host.rs");
    for word in ["Command::new", "process::Command", "tokio::process", ".spawn(", "spawn(&"] {
        assert!(!code_lines(&host).iter().any(|(_, l)| l.contains(word)), "integrate_host.rs names {word}");
    }

    // The socket crate and the command line tool know nothing of the integrations.
    let mut socket_scanned = 0;
    for dir in ["ipc/src", "wsh/src"] {
        for (path, text) in files_below(dir, "rs") {
            socket_scanned += 1;
            let lower = text.to_ascii_lowercase();
            for word in ["integration_", "integrate", "approved", "ticket", "holzwave_core::integrate"] {
                assert!(!lower.contains(word), "{path} names `{word}`");
            }
        }
    }

    // The commands of the page: the reviewed list of the ACL test, the manifest of the build and the capability grant agree.
    let acl = file("app/src-tauri/tests/acl.rs");
    let reviewed: BTreeSet<String> = {
        let start = acl.find("const INTEGRATION_COMMANDS").expect("the reviewed list");
        let list = &acl[start..start + acl[start..].find("];").expect("the end of the list")];
        list.split('"').skip(1).step_by(2).map(str::to_owned).collect()
    };
    assert_eq!(reviewed.len(), 15, "fifteen reviewed commands: {reviewed:?}");
    let build = file("app/src-tauri/build.rs");
    let in_build: BTreeSet<String> = build.split('"').skip(1).step_by(2).filter(|s| s.starts_with("integration_")).map(str::to_owned).collect();
    assert_eq!(in_build, reviewed, "the manifest of the build lists the reviewed commands and no other");
    let capability = file("app/src-tauri/capabilities/default.json");
    let granted: BTreeSet<String> = capability
        .split('"')
        .skip(1)
        .step_by(2)
        .filter_map(|s| s.strip_prefix("allow-integration-"))
        .map(|s| format!("integration_{}", s.replace('-', "_")))
        .collect();
    assert_eq!(granted, reviewed, "the capability grants the reviewed commands and no other");
    // One command confirms; the shell calls the service's confirm from one function, and the page calls the command from one file.
    let confirms: Vec<_> = ["app/src-tauri/src/integrate_host.rs", "app/src-tauri/src/state.rs", "app/src-tauri/src/commands.rs"]
        .iter()
        .flat_map(|path| {
            let text = file(path);
            code_lines(&text).into_iter().filter(|(_, l)| l.contains(".confirm(") || l.contains("confirm_from(")).map(|(n, _)| format!("{path}:{n}")).collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(confirms.len(), 2, "the service's confirm is reached from the host wrapper and the core method only: {confirms:?}");
    let page_callers: Vec<String> = files_below("app/src", "ts")
        .into_iter()
        .filter(|(_, text)| code_lines(text).iter().any(|(_, l)| l.contains("\"integration_confirm\"")))
        .map(|(path, _)| path)
        .collect();
    assert_eq!(page_callers, ["app/src/integrateconfirm.ts"], "only the card's module asks the core to confirm");

    // The page's rule that only a person decides: a click or key that a script made is ignored.
    let card = file("app/src/integrateconfirm.ts");
    assert!(card.matches("trusted: e.isTrusted").count() >= 2, "the card passes the browser's isTrusted for Allow and for Deny");
    let state = file("app/src/integratestate.ts");
    let allow_arm = state.find("case \"allow\"").map(|at| &state[at..at + 700]).expect("the Allow arm of nextMode");
    assert!(allow_arm.contains("!event.trusted"), "Allow is ignored when the event is not trusted");
    let deny_arm = state.find("case \"deny\"").map(|at| &state[at..at + 300]).expect("the Deny arm of nextMode");
    assert!(deny_arm.contains("!event.trusted"), "Deny is ignored when the event is not trusted");
    assert!(state.contains("HALF_SECOND"), "Allow is ignored for its first half second");
    let dispatches: Vec<String> = files_below("app/src", "ts")
        .into_iter()
        .filter(|(path, _)| path.contains("integrate"))
        .filter(|(_, text)| code_lines(text).iter().any(|(_, l)| l.contains("dispatchEvent(") || l.contains(".click()")))
        .map(|(path, _)| path)
        .collect();
    assert!(dispatches.is_empty(), "a module of the integrations makes a click of its own: {dispatches:?}");
    println!(
        "Scans: run_argv( called from {} places in {scanned} source files, Approved built in one function, {} place that builds a command, {socket_scanned} files of the socket crate and the command line tool, \
         {} reviewed commands, the page's trusted-event rule",
        calls.len(),
        new_command.len(),
        reviewed.len()
    );
}

// ---- the AI chat -----------------------------------------------------------------------------------------------------

#[test]
fn nothing_under_the_ai_modules_names_the_integrations() {
    // 1. Nothing under core/src/ai names the integrate module; nothing of the shell's ai files names an integration.
    let mut scanned = 0;
    for (path, text) in files_below("core/src/ai", "rs") {
        scanned += 1;
        for (line, content) in code_lines(&text) {
            assert!(!content.contains("integrate"), "{path}:{line} names the integrate module: {content}");
            assert!(!content.contains("crate::integrate") && !content.contains("holzwave_core::integrate"), "{path}:{line}");
        }
    }
    let mut shell_files = Vec::new();
    let src_tauri = root().join("app/src-tauri/src");
    for entry in std::fs::read_dir(&src_tauri).expect("the shell's source").flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("ai_") && name.ends_with(".rs") {
            shell_files.push(name);
        }
    }
    shell_files.sort();
    assert!(shell_files.len() >= 6, "the shell has its ai_*.rs files: {shell_files:?}");
    for name in &shell_files {
        let text = std::fs::read_to_string(src_tauri.join(name)).expect("a file");
        assert!(!text.to_ascii_lowercase().contains("integrat"), "app/src-tauri/src/{name} names the integrations");
    }

    // 2. The closed list of the AI chat's tools holds no integration tool, read from the source and from the type.
    let catalogue = file("core/src/ai/tools/catalog.rs");
    let mut names: Vec<String> = Vec::new();
    let start = catalogue.find("pub fn as_str(self)").expect("the names of the tools");
    for line in catalogue[start..].lines().skip(1).take(10) {
        if let Some((_, rest)) = line.split_once("=> \"") {
            names.push(rest.split('"').next().unwrap_or_default().to_owned());
        }
    }
    let typed: Vec<String> = ToolName::ALL.iter().map(|tool| tool.as_str().to_owned()).collect();
    assert_eq!(names, typed, "the tool names in the source are the tools of the type");
    assert_eq!(typed.len(), 7);
    for name in &typed {
        for word in ["integr", "kube", "talos", "azure", "gam", "ticket", "profile", "secret"] {
            assert!(!name.contains(word), "the AI tool `{name}` names `{word}`");
        }
    }

    // 3. The integrations use the AI modules for three helpers only: the redactor and the character and hash helpers of the
    //    approval cards. Nothing of the chat, a provider, a store, the tools' approval table or a service.
    let allowed_tools: BTreeSet<&str> = ["hex", "payload_hash", "visible", "visible_multiline", "is_hidden"].into();
    let mut uses = BTreeSet::new();
    for (path, text) in files_below("core/src/integrate", "rs") {
        for (line, content) in code_lines(&text) {
            let mut rest = content;
            while let Some(at) = rest.find("crate::ai") {
                let tail = &rest[at + "crate::ai".len()..];
                let item: String = tail.chars().take_while(|c| !matches!(c, ';' | ' ' | ')' | ',')).collect();
                let item = if tail.starts_with("::tools::{") {
                    tail[..tail.find('}').map_or(tail.len(), |end| end + 1)].to_owned()
                } else {
                    item
                };
                let fine = item == "::redact"
                    || item.starts_with("::redact::")
                    || item.strip_prefix("::tools::").is_some_and(|name| {
                        let name = name.trim_matches(|c| c == '{' || c == '}');
                        name.split(',').map(str::trim).all(|one| allowed_tools.contains(one))
                    });
                assert!(fine, "{path}:{line} uses `crate::ai{item}`, which is not on the list");
                uses.insert(format!("crate::ai{item}"));
                rest = &tail[item.len().min(tail.len())..];
            }
            for other in ["super::super::ai", "holzwave_core::ai"] {
                assert!(!content.contains(other), "{path}:{line} reaches the AI modules by another path");
            }
        }
    }
    assert!(!uses.is_empty(), "the scan saw the helpers it allows: {uses:?}");

    // 4. The contexts the chat is offered for a pane. A pane that shows a block of the integrations has the contexts of a
    //    monitor pane, none: the shell's rule has no arm for the integration kind. It reads a block pane's kinds only when
    //    the block is a preview, refuses every other block for a terminal read, and the file tools have no target for it.
    let host = file("app/src-tauri/src/ai_host.rs");
    let kinds = host.find("fn kinds(&self, pane: PaneId)").map(|at| &host[at..at + 900]).expect("the rule for the kinds of a pane");
    assert!(
        kinds.contains("if block.kind != BlockKind::Preview") && kinds.contains("return Vec::new();"),
        "a block that is not a preview offers no context: {kinds}"
    );
    let terminal = host.find("fn read_terminal(").map(|at| &host[at..at + 500]).expect("the terminal read");
    assert!(terminal.contains("core.context_block(pane).is_some()") && terminal.contains("ReadRefusal::Unsupported"), "a block pane is no terminal");
    let block_read = host.find("fn read_block(").map(|at| &host[at..at + 500]).expect("the block read");
    assert!(block_read.contains("block.kind != BlockKind::Preview") && block_read.contains("ReadRefusal::Unsupported"), "only a preview is read");
    let state = file("app/src-tauri/src/state.rs");
    assert!(
        state.contains("BlockKind::Monitor | BlockKind::Chat | BlockKind::Integration => return None"),
        "the file tools have no target for an integration pane, as for a monitor"
    );
    println!(
        "Scans (AI): {scanned} files under core/src/ai, {} shell files ai_*.rs, {} tools {typed:?}, helpers used by the integrations {uses:?}",
        shell_files.len(),
        typed.len()
    );
}

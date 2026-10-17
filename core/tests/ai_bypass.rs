//! The approval-bypass corpus: a hostile model, a hostile file system and a hostile page try to make the host run
//! something that nobody approved.
//!
//! The corpus runs against the approval table and the agent loop, over a stand-in tool host that mirrors the contract of
//! the shell's host (it opens only the path of the ticket, looks at files through `inspect`, writes only when the stamp
//! still holds, and runs commands through the real runner). What is proved: no hostile call gets a card, a decision must
//! repeat the hash of its own card, a changed file, path, pane, chat or trust level voids the approval, nothing is
//! remembered or reused, an undecided or cancelled call does nothing, a command runs as it was shown, a process the agent
//! starts finds no session, and every refusal code of the table is reached or exempt with a reason. A failing assertion
//! names the attack, never its bytes. Test code only.

mod common;
mod security;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use holzwave_core::ai::agent::Limits;
use holzwave_core::ai::provider::{CancelToken, Detail};
use holzwave_core::ai::scripted::Script;
use holzwave_core::ai::tools::{
    Decision, TaintSource, ToolError, ToolName, ToolOutput, report, quote, remote_command, visible,
};
use holzwave_core::ai::types::StopReason;
use holzwave_core::exec::LocalExec;
use holzwave_core::ssh::{ExecEnd, ExecResult, MAX_EXEC_COMMAND};
use serde_json::{Value, json};

use common::Xorshift64;
use security::hostile::{ch, forbidden_in};
use security::world::{Bench, Outcome, PANE_A, PANE_B, World, WorldOptions, malformed_attacks, proposal_attacks, run_attacks};

/// The words a call that was denied and a call nobody decided in time are answered with, written out here.
const DENIED_TEXT: &str = "The user denied this action.";
const TIMED_OUT: &str = "No decision was made in time, so this call did not run.";

/// The 17 refusal codes of the table, in the order of its variants, written out here and not read from it.
const CODES: [&str; 17] = [
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

/// The code that no corpus reaches and why: only the shell's host produces it.
const EXEMPT: (&str, &str) = (
    "not_shared",
    "only the shell host produces it, for a read_pane call on a pane that is not shared with the chat; the shell has its own test",
);

fn outcome(id: &str, code: &str) -> Outcome {
    Outcome { id: id.to_owned(), code: code.to_owned() }
}

/// The code of a refusal.
fn code_of<T>(result: Result<T, ToolError>, what: &str) -> &'static str {
    match result {
        Ok(_) => panic!("{what}: expected a refusal"),
        Err(error) => error.code(),
    }
}

/// A card for a read of `proj/lib.rs`-like file `relative`.
fn read_card(bench: &Bench, id: &str, relative: &str) -> Value {
    bench.propose(id, "read_file", json!({ "path": bench.tree().path(relative) })).expect("a read card")
}

fn write_args(bench: &Bench, relative: &str, content: &str) -> Value {
    json!({ "path": bench.tree().path(relative), "content": content })
}

// ---- refused before a card -----------------------------------------------------------------------------------------------

fn denied_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let attacks = proposal_attacks(bench.tree());
    let least = if cfg!(unix) { 24 } else { 20 };
    assert!(attacks.len() >= least, "the corpus has {} stateless attacks with a rule", attacks.len());
    let outcomes = run_attacks(&bench, &attacks);
    assert!(outcomes.iter().all(|outcome| outcome.code == "denied"));

    // The controls in the same bench: the four calls that are allowed get a card.
    for (id, name, args) in [
        ("control-1", "read_file", json!({ "path": bench.tree().path("proj/src/lib.rs") })),
        ("control-2", "write_file", write_args(&bench, "proj/notes.txt", "new notes\n")),
        ("control-3", "read_file", json!({ "path": bench.tree().path("home/.zshrc") })),
        ("control-4", "read_file", json!({ "path": bench.tree().path("proj/swap") })),
    ] {
        if cfg!(not(unix)) && id == "control-4" {
            continue;
        }
        let card = bench.propose(id, name, args);
        assert!(card.is_ok(), "{id}: an allowed call got no card");
    }
    outcomes
}

#[test]
fn denied_requests_never_get_a_card_and_never_touch_the_disk() {
    let outcomes = denied_outcomes();
    assert!(outcomes.len() >= 20);
}

fn malformed_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let attacks = malformed_attacks(bench.tree());
    assert!(attacks.len() >= 19, "the corpus has {} malformed attacks", attacks.len());
    let mut outcomes = run_attacks(&bench, &attacks);
    let file = bench.tree().path("proj/notes.txt");

    // A call id that is empty, too long, or holds a line feed.
    for (id, label) in [(String::new(), "empty id"), ("x".repeat(129), "long id"), ("a\nb".to_owned(), "id with a line feed")] {
        let code = code_of(bench.propose(&id, "read_file", json!({ "path": file })), label);
        assert_eq!(code, "invalid_arguments", "{label}");
        outcomes.push(outcome(label, code));
    }

    // The 16 open calls stay valid; a repeated id and the 17th are refused.
    bench.propose("open-0", "read_file", json!({ "path": file })).expect("the first call");
    let code = code_of(bench.propose("open-0", "read_file", json!({ "path": file })), "a repeated call id");
    assert_eq!(code, "duplicate_call");
    outcomes.push(outcome("a repeated call id", code));
    for n in 1..16 {
        bench.propose(&format!("open-{n}"), "read_file", json!({ "path": file })).unwrap_or_else(|_| panic!("open call {n} was refused"));
    }
    let code = code_of(bench.propose("open-16", "read_file", json!({ "path": file })), "the 17th open call");
    assert_eq!(code, "too_many");
    outcomes.push(outcome("the 17th open call", code));

    // 64 turns with a ledger stay valid; the 65th is refused.
    let busy = Bench::new();
    for turn in 1..=64u64 {
        let scope = busy.scope_of("c1", turn, PANE_A);
        busy.table().taint(&scope, TaintSource::Context).unwrap_or_else(|_| panic!("turn {turn} was refused"));
    }
    let scope = busy.scope_of("c1", 65, PANE_A);
    let code = code_of(busy.propose_in(&scope, "late", "read_file", json!({ "path": busy.tree().path("proj/notes.txt") })), "the 65th turn");
    assert_eq!(code, "too_many");
    outcomes.push(outcome("the 65th turn", code));
    outcomes
}

#[test]
fn malformed_requests_are_refused_without_a_card() {
    let outcomes = malformed_outcomes();
    assert!(outcomes.len() >= 25, "{} outcomes", outcomes.len());
}

// ---- a decision must repeat the hash of its own card ----------------------------------------------------------------------

#[test]
fn a_decision_that_does_not_repeat_the_card_hash_is_refused_and_the_right_one_runs_only_its_own_call() {
    let turns = vec![
        Script::new().tool_call("w1", "write_file", json!({ "path": "a.txt", "content": "new a\n" })).stop(StopReason::ToolUse),
        Script::new().tool_call("w2", "write_file", json!({ "path": "b.txt", "content": "new b\n" })).stop(StopReason::ToolUse),
        Script::new().tool_call("w3", "write_file", json!({ "path": "c.txt", "content": "new c\n" })).stop(StopReason::ToolUse),
        Script::reply("done").stop(StopReason::EndTurn),
    ];
    let world = World::new(WorldOptions { turns, ..WorldOptions::default() });
    let before = world.digest();
    let chat = world.chat();
    world.ask(&chat, "write three files");

    // Card 1: the control. It runs with its own hash.
    let first = world.card(&chat, 1, Duration::from_secs(10)).expect("card 1");
    world.allow(&chat, &first).expect("card 1 is allowed with its own hash");
    assert!(common::wait_until(Duration::from_secs(10), || world.runs().len() == 1), "card 1 did not run");

    // Card 2: four wrong hashes, then its own.
    let second = world.card(&chat, 2, Duration::from_secs(10)).expect("card 2");
    let own = second["hash"].as_str().unwrap_or("").to_owned();
    let mut last_changed = own.clone();
    let flipped = if last_changed.pop() == Some('0') { '1' } else { '0' };
    last_changed.push(flipped);
    for (label, hash) in [
        ("the hash of card 1", first["hash"].as_str().unwrap_or("").to_owned()),
        ("an empty hash", String::new()),
        ("the own hash in capitals", own.to_uppercase()),
        ("the own hash changed in its last character", last_changed),
    ] {
        let result = world.decide(&chat, &second, Decision::Allow, &hash);
        assert!(matches!(result, Err(ToolError::HashMismatch)), "{label}: the decision was not refused as a mismatch");
    }
    assert_eq!(world.runs().len(), 1, "a wrong hash ran card 2");
    world.decide(&chat, &second, Decision::Allow, &own).expect("card 2 is allowed with its own hash");
    assert!(common::wait_until(Duration::from_secs(10), || world.runs().len() == 2), "card 2 did not run");

    // Card 3: denied with its own hash.
    let third = world.card(&chat, 3, Duration::from_secs(10)).expect("card 3");
    let hash = third["hash"].as_str().unwrap_or("").to_owned();
    world.decide(&chat, &third, Decision::Deny, &hash).expect("card 3 is denied");
    assert_eq!(world.wait_end(&chat, Duration::from_secs(20)).expect("an end")["end"], "done");
    let results = world.results(&chat);
    assert_eq!(results[2]["content"], DENIED_TEXT, "the result of a denied card");
    assert_eq!(world.tree().read("proj/a.txt").as_deref(), Some("new a\n"));
    assert_eq!(world.tree().read("proj/b.txt").as_deref(), Some("new b\n"));
    assert!(!world.tree().exists("proj/c.txt"), "the denied file was made");
    assert_eq!(world.runs().len(), 2, "the host ran exactly two writes");
    assert_ne!(world.digest(), before, "the two writes changed nothing");
}

fn others_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let mut outcomes = Vec::new();
    let a = bench.propose("call-a", "write_file", write_args(&bench, "proj/a.txt", "new a\n")).expect("card a");
    let b = bench.propose("call-b", "write_file", write_args(&bench, "proj/b.txt", "new b\n")).expect("card b");
    let (hash_a, hash_b) = (a["hash"].as_str().unwrap_or("").to_owned(), b["hash"].as_str().unwrap_or("").to_owned());

    // The hash of another call.
    let code = code_of(bench.decide("call-a", Decision::Allow, &hash_b), "another call's hash");
    assert_eq!(code, "hash_mismatch");
    outcomes.push(outcome("another call's hash", code));
    assert_eq!(bench.pending().len(), 2, "a refused decision changed the open calls");

    // A call id that only another chat holds.
    let other = bench.scope_of("c2", 1, PANE_A);
    let card = bench.propose_in(&other, "call-c2", "read_file", json!({ "path": bench.tree().path("proj/notes.txt") })).expect("a card of another chat");
    let hash_c2 = card["hash"].as_str().unwrap_or("").to_owned();
    let code = code_of(bench.table().decide("c1", "call-c2", Decision::Allow, &hash_c2), "a call of another chat");
    assert_eq!(code, "unknown_call");
    outcomes.push(outcome("a call of another chat", code));
    assert_eq!(code_of(bench.table().decide("c2", "call-a", Decision::Allow, &hash_a), "a call of this chat decided in another"), "unknown_call");

    // Twice, and the other way round.
    bench.decide("call-a", Decision::Allow, &hash_a).expect("the first allow");
    for (label, decision) in [("a second allow", Decision::Allow), ("a deny after an allow", Decision::Deny)] {
        let code = code_of(bench.decide("call-a", decision, &hash_a), label);
        assert!(code == "not_pending" || code == "unknown_call", "{label}: {code}");
        outcomes.push(outcome(label, code));
    }
    bench.decide("call-b", Decision::Deny, &hash_b).expect("a deny");
    let code = code_of(bench.decide("call-b", Decision::Allow, &hash_b), "an allow after a deny");
    assert!(code == "not_pending" || code == "unknown_call", "an allow after a deny: {code}");
    outcomes.push(outcome("an allow after a deny", code));

    // The host runs the call once at most, and never a denied call.
    bench.run(bench.scope(), "call-a").expect("the allowed call runs");
    let code = code_of(bench.run(bench.scope(), "call-a"), "a second run");
    assert_eq!(code, "unknown_call");
    outcomes.push(outcome("a second run", code));
    let code = code_of(bench.run(bench.scope(), "call-b"), "the run of a denied call");
    assert_eq!(code, "not_approved");
    outcomes.push(outcome("the run of a denied call", code));
    assert_eq!(bench.host().run_count(), 1, "the host ran more than the one approved call");
    outcomes
}

#[test]
fn a_decision_for_another_chat_or_call_or_twice_does_nothing_more() {
    assert!(others_outcomes().len() >= 6);
}

// ---- a world that changes after the card ------------------------------------------------------------------------------------

fn retarget_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let tree = bench.tree();
    let (a, b) = (tree.path("proj/a.txt"), tree.path("proj/b.txt"));
    let mut outcomes = Vec::new();

    // A path the host resolves to another file at the second look.
    bench.host().retarget_on_second_resolve(&a, &b);
    let card = bench.propose("rt-path", "read_file", json!({ "path": a })).expect("a card");
    bench.allow(&card).expect("allowed");
    let code = code_of(bench.run(bench.scope(), "rt-path"), "a retargeted path");
    assert_eq!(code, "payload_changed");
    outcomes.push(outcome("a retargeted path", code));

    // A link that is swapped between the card and the run.
    #[cfg(unix)]
    {
        let swap = tree.path("proj/swap");
        bench.host().retarget_on_second_resolve(&swap, &tree.path("proj/notes.txt"));
        let card = bench.propose("rt-link", "read_file", json!({ "path": swap })).expect("a card");
        bench.allow(&card).expect("allowed");
        let code = code_of(bench.run(bench.scope(), "rt-link"), "a swapped link");
        assert_eq!(code, "payload_changed");
        outcomes.push(outcome("a swapped link", code));
    }

    // A working directory that is another folder at the second look.
    let proj = tree.path("proj");
    bench.host().retarget_on_second_resolve(&proj, &tree.path("proj/src"));
    let card = bench.propose("rt-cwd", "run_command", json!({ "command": "ls" })).expect("a card");
    bench.allow(&card).expect("allowed");
    let code = code_of(bench.run(bench.scope(), "rt-cwd"), "a retargeted folder");
    assert_eq!(code, "payload_changed");
    outcomes.push(outcome("a retargeted folder", code));
    assert_eq!(bench.host().run_count(), 0, "the host ran after a retarget");
    assert!(bench.digest_unchanged());
    outcomes
}

#[test]
fn a_path_retargeted_after_the_card_is_refused() {
    assert!(retarget_outcomes().len() >= 2);
}

fn changed_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let tree = bench.tree();
    let mut outcomes = Vec::new();
    let attempt = |id: &str, args: Value, name: &str, change: &dyn Fn()| -> &'static str {
        let card = bench.propose(id, name, args).expect("a card");
        change();
        bench.allow(&card).expect("allowed");
        code_of(bench.run(bench.scope(), id), id)
    };

    // The file got new text and a new change time.
    let code = attempt("changed-text", write_args(&bench, "proj/a.txt", "from the model\n"), "write_file", &|| {
        tree.write("proj/a.txt", "from the outside\n");
    });
    assert_eq!(code, "payload_changed");
    assert_eq!(tree.read("proj/a.txt").as_deref(), Some("from the outside\n"), "the changed file was overwritten");
    outcomes.push(outcome("changed text", code));

    // The file got new text and its change time was put back.
    #[cfg(unix)]
    {
        let path = tree.path("proj/b.txt");
        let code = attempt("changed-text-same-time", write_args(&bench, "proj/b.txt", "from the model\n"), "write_file", &|| {
            let before = fs::metadata(&path).and_then(|meta| meta.modified()).expect("a change time");
            tree.write("proj/b.txt", "from the outside\n");
            let file = fs::OpenOptions::new().write(true).open(&path).expect("the file opens");
            file.set_modified(before).expect("the change time is put back");
        });
        assert_eq!(code, "payload_changed", "a restored change time hid a change");
        assert_eq!(tree.read("proj/b.txt").as_deref(), Some("from the outside\n"));
        outcomes.push(outcome("changed text with the change time restored", code));
    }

    // The file was deleted.
    let notes = tree.path("proj/notes.txt");
    let code = attempt("deleted", write_args(&bench, "proj/notes.txt", "from the model\n"), "write_file", &|| {
        fs::remove_file(&notes).expect("the file goes");
    });
    assert_eq!(code, "payload_changed");
    assert!(!tree.exists("proj/notes.txt"), "a deleted file was made again");
    outcomes.push(outcome("deleted", code));

    // A file that appeared after a card that was made for a file that did not exist.
    let code = attempt("appeared", write_args(&bench, "proj/fresh.txt", "from the model\n"), "write_file", &|| {
        tree.write("proj/fresh.txt", "from the outside\n");
    });
    assert_eq!(code, "payload_changed");
    assert_eq!(tree.read("proj/fresh.txt").as_deref(), Some("from the outside\n"));
    outcomes.push(outcome("appeared", code));

    // An edit whose old text is gone.
    let args = json!({ "path": tree.path("proj/src/lib.rs"), "old_text": "pub fn one", "new_text": "pub fn two" });
    let code = attempt("edit-gone", args, "edit_file", &|| tree.write("proj/src/lib.rs", "something else\n"));
    assert_eq!(code, "payload_changed");
    assert_eq!(tree.read("proj/src/lib.rs").as_deref(), Some("something else\n"));
    outcomes.push(outcome("old text gone", code));
    assert_eq!(bench.host().run_count(), 0, "the host ran after a change");

    // The control: with no change the write runs once.
    let card = bench.propose("control", "write_file", write_args(&bench, "proj/control.txt", "from the model\n")).expect("a card");
    bench.allow(&card).expect("allowed");
    bench.run(bench.scope(), "control").expect("the unchanged write runs");
    assert_eq!(bench.host().run_count(), 1);
    assert_eq!(tree.read("proj/control.txt").as_deref(), Some("from the model\n"));
    outcomes
}

#[test]
fn a_file_changed_after_the_card_is_never_overwritten() {
    assert!(changed_outcomes().len() >= 4);
}

fn pane_outcomes() -> Vec<Outcome> {
    let bench = Bench::new();
    let tree = bench.tree();
    let mut outcomes = Vec::new();
    let call = |id: &str| -> Value {
        let card = read_card(&bench, id, "proj/src/lib.rs");
        bench.allow(&card).expect("allowed");
        card
    };

    // An approval for pane A, run for pane B, another turn and another chat.
    call("p-pane");
    let code = code_of(bench.run(&bench.scope_of("c1", 1, PANE_B), "p-pane"), "another pane");
    assert_eq!(code, "scope_mismatch");
    outcomes.push(outcome("another pane", code));
    call("p-turn");
    let code = code_of(bench.run(&bench.scope_of("c1", 2, PANE_A), "p-turn"), "another turn");
    assert_eq!(code, "scope_mismatch");
    outcomes.push(outcome("another turn", code));
    call("p-chat");
    let code = code_of(bench.run(&bench.scope_of("c2", 1, PANE_A), "p-chat"), "another chat");
    assert_eq!(code, "unknown_call");
    outcomes.push(outcome("another chat", code));
    assert_eq!(bench.host().run_count(), 0, "the host ran for the wrong scope");
    // The control: the right scope executes.
    bench.run(bench.scope(), "p-chat").expect("the right scope runs");
    assert_eq!(bench.host().run_count(), 1);

    // A waiting call is voided when its pane closes, and the thread blocked on it returns.
    let closing = Bench::new();
    let card = read_card(&closing, "waiting", "proj/notes.txt");
    assert_eq!(card["call_id"], "waiting");
    let table = closing.table().clone();
    let waiter = thread::spawn(move || table.wait("c1", "waiting", &CancelToken::new()));
    thread::sleep(Duration::from_millis(150));
    assert!(closing.table().cancel_pane(PANE_A) >= 1, "no call was voided");
    let waited = waiter.join().expect("the waiter did not panic");
    let code = code_of(waited, "a closed pane");
    assert_eq!(code, "cancelled");
    outcomes.push(outcome("a closed pane", code));
    assert_eq!(closing.host().run_count(), 0);
    let _ = tree;
    outcomes
}

#[test]
fn an_approval_for_one_pane_cannot_run_in_another_and_a_closed_pane_voids_its_cards() {
    assert!(pane_outcomes().len() >= 4);
}

fn taint_outcomes() -> Vec<Outcome> {
    let mut outcomes = Vec::new();

    // New untrusted text after a write card was proposed: the allow is refused.
    let bench = Bench::new();
    let card = bench.propose("w1", "write_file", write_args(&bench, "proj/a.txt", "x\n")).expect("a card");
    bench.table().taint(bench.scope(), TaintSource::Context).expect("a taint");
    let code = code_of(bench.allow(&card), "an allow after new untrusted text");
    assert_eq!(code, "taint_changed");
    outcomes.push(outcome("an allow after new untrusted text", code));

    // A write card that was allowed and then tainted by the output of a read: the run is refused.
    let bench = Bench::new();
    let card = bench.propose("w2", "write_file", write_args(&bench, "proj/a.txt", "x\n")).expect("a card");
    bench.allow(&card).expect("allowed");
    bench.table().taint(bench.scope(), TaintSource::ToolOutput(ToolName::ReadFile)).expect("a taint");
    let code = code_of(bench.run(bench.scope(), "w2"), "a run after new untrusted text");
    assert_eq!(code, "taint_changed");
    outcomes.push(outcome("a run after new untrusted text", code));
    assert_eq!(bench.host().run_count(), 0);
    assert!(bench.digest_unchanged());

    // A card proposed after the taint shows it, can be allowed and runs.
    let bench = Bench::new();
    bench.table().taint(bench.scope(), TaintSource::Context).expect("a taint");
    let card = bench.propose("w3", "write_file", write_args(&bench, "proj/a.txt", "x\n")).expect("a card");
    assert_eq!(card["tainted"], true);
    assert_eq!(card["taint"], json!(["attached context"]));
    bench.allow(&card).expect("allowed");
    bench.run(bench.scope(), "w3").expect("runs");
    assert_eq!(bench.host().run_count(), 1);

    // A read card proposed before a taint is still approvable.
    let bench = Bench::new();
    let card = read_card(&bench, "r1", "proj/notes.txt");
    bench.table().taint(bench.scope(), TaintSource::Context).expect("a taint");
    bench.allow(&card).expect("a read approval survives new text");
    bench.run(bench.scope(), "r1").expect("the read runs");
    outcomes
}

#[test]
fn new_untrusted_text_voids_an_older_write_approval() {
    assert!(taint_outcomes().len() >= 2);
}

fn once_outcomes() -> Vec<Outcome> {
    let mut outcomes = Vec::new();

    // The same call under a new id is awaiting again.
    let bench = Bench::new();
    let first = read_card(&bench, "o1", "proj/notes.txt");
    bench.allow(&first).expect("allowed");
    bench.run(bench.scope(), "o1").expect("runs once");
    read_card(&bench, "o2", "proj/notes.txt");
    assert_eq!(bench.pending().len(), 1, "the same call under a new id is not awaiting");
    let code = code_of(bench.run(bench.scope(), "o2"), "the same call under a new id");
    assert_eq!(code, "not_approved");
    outcomes.push(outcome("the same call under a new id", code));

    // A second run of a call while the first is held open inside the host.
    let held = Bench::new();
    let card = read_card(&held, "h1", "proj/notes.txt");
    held.allow(&card).expect("allowed");
    let hold = held.host().hold_next_run();
    let (table, host, scope) = (held.table().clone(), held.host().clone(), held.scope().clone());
    let runner = thread::spawn(move || table.run(&*host, &scope, "h1", &CancelToken::new()));
    assert!(hold.wait_started(Duration::from_secs(10)), "the run did not start");
    let code = code_of(held.run(held.scope(), "h1"), "a second run while the first is open");
    assert_eq!(code, "already_used");
    outcomes.push(outcome("a second run while the first is open", code));
    hold.release();
    runner.join().expect("the runner did not panic").expect("the first run ends ok");
    assert_eq!(held.host().run_count(), 1);

    // A host that fails: the answer is `host` and the call is gone.
    let failing = Bench::new();
    let card = read_card(&failing, "f1", "proj/notes.txt");
    failing.allow(&card).expect("allowed");
    failing.host().fail_next_run();
    let code = code_of(failing.run(failing.scope(), "f1"), "a failing host");
    assert_eq!(code, "host");
    outcomes.push(outcome("a failing host", code));
    let code = code_of(failing.run(failing.scope(), "f1"), "a call after a failed run");
    assert_eq!(code, "unknown_call");
    outcomes.push(outcome("a call after a failed run", code));
    outcomes
}

#[test]
fn an_approval_is_single_use_and_never_remembered() {
    assert!(once_outcomes().len() >= 4);
}

// ---- no decision in time, a stop, a closed pane, a deleted chat ------------------------------------------------------------------

#[test]
fn no_decision_in_time_cancel_pane_close_or_delete_mean_no_action() {
    let script = || {
        vec![
            Script::new().tool_call("w1", "write_file", json!({ "path": "out.txt", "content": "x\n" })).stop(StopReason::ToolUse),
            Script::reply("done").stop(StopReason::EndTurn),
        ]
    };

    // No decision within the deadline: the fixed text of an undecided call, and no run.
    let limits = Limits { approval_timeout: Duration::from_millis(400), ..Limits::default() };
    let world = World::new(WorldOptions { turns: script(), limits, ..WorldOptions::default() });
    let before = world.digest();
    let chat = world.chat();
    world.ask(&chat, "write the file");
    let card = world.card(&chat, 1, Duration::from_secs(10)).expect("a card");
    assert_eq!(world.wait_end(&chat, Duration::from_secs(20)).expect("an end")["end"], "done");
    assert_eq!(world.results(&chat)[0]["content"], TIMED_OUT);
    assert!(matches!(world.allow(&chat, &card), Err(ToolError::UnknownCall)), "a late decision was taken");
    assert!(world.runs().is_empty() && world.digest() == before, "the timeout ran something");

    // A stop while the card waits.
    let world = World::new(WorldOptions { turns: script(), ..WorldOptions::default() });
    let before = world.digest();
    let chat = world.chat();
    world.ask(&chat, "write the file");
    let card = world.card(&chat, 1, Duration::from_secs(10)).expect("a card");
    assert!(world.service().cancel(&chat), "the chat had no turn");
    assert_eq!(world.wait_end(&chat, Duration::from_secs(20)).expect("an end")["end"], "cancelled");
    assert!(world.service().approvals().pending(chat.as_str()).is_empty(), "a card is left after a stop");
    assert!(matches!(world.allow(&chat, &card), Err(ToolError::UnknownCall)));
    assert!(world.runs().is_empty() && world.digest() == before, "a stop ran something");

    // The pane of the turn closes.
    let world = World::new(WorldOptions { turns: script(), ..WorldOptions::default() });
    let before = world.digest();
    let chat = world.chat();
    world.ask(&chat, "write the file");
    let card = world.card(&chat, 1, Duration::from_secs(10)).expect("a card");
    assert_eq!(world.service().pane_closed(PANE_A), 1, "the closed pane voided no call");
    assert_eq!(world.wait_end(&chat, Duration::from_secs(20)).expect("an end")["end"], "cancelled");
    assert!(matches!(world.allow(&chat, &card), Err(ToolError::UnknownCall)));
    assert!(world.runs().is_empty() && world.digest() == before, "a closed pane ran something");

    // The chat is deleted.
    let world = World::new(WorldOptions { turns: script(), ..WorldOptions::default() });
    let before = world.digest();
    let chat = world.chat();
    world.ask(&chat, "write the file");
    let card = world.card(&chat, 1, Duration::from_secs(10)).expect("a card");
    world.service().delete_conversation(&chat).expect("a deletion");
    assert!(common::wait_until(Duration::from_secs(20), || world.service().active_turns() == 0), "the turn did not end");
    assert!(world.service().list_conversations().expect("a listing").chats.is_empty(), "the store still has the chat");
    assert!(matches!(world.allow(&chat, &card), Err(ToolError::UnknownCall)));
    assert!(world.runs().is_empty() && world.digest() == before, "a deleted chat ran something");
}

// ---- two words only ----------------------------------------------------------------------------------------------------------------

#[test]
fn a_decision_has_two_words_only() {
    assert_eq!(serde_json::from_value::<Decision>(json!("allow")).expect("allow"), Decision::Allow);
    assert_eq!(serde_json::from_value::<Decision>(json!("deny")).expect("deny"), Decision::Deny);
    let refused = [
        json!("always"),
        json!("remember"),
        json!("session"),
        json!("Allow"),
        json!("ALLOW"),
        json!("yes"),
        json!(true),
        json!(1),
        Value::Null,
        json!(""),
        json!("allow "),
        json!({ "decision": "allow" }),
    ];
    for value in refused {
        assert!(serde_json::from_value::<Decision>(value.clone()).is_err(), "a decision read from {value}");
    }
}

// ---- a command is shown as it is and runs as it was shown -----------------------------------------------------------------------

fn command_row(card: &Value) -> String {
    card["payload"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "command"))
        .and_then(|row| row["value"].as_str())
        .expect("a command row")
        .to_owned()
}

#[test]
fn a_hostile_command_is_shown_escaped_and_hashed_as_written() {
    let bench = Bench::new();
    let bs = '\\';
    let (rlo, zwsp, tag) = (ch(0x202e), ch(0x200b), ch(0xe0041));
    let commands = [
        "echo 'single' \"double\"".to_owned(),
        "echo `whoami`".to_owned(),
        "echo $(id)".to_owned(),
        "a; b && c".to_owned(),
        "line one\nline two".to_owned(),
        "tab\tseparated".to_owned(),
        format!("echo {rlo}evil"),
        format!("echo a{zwsp}b"),
        format!("echo tag{tag}"),
    ];
    let mut hashes = BTreeSet::new();
    let mut rows = BTreeSet::new();
    for (index, command) in commands.iter().enumerate() {
        let card = bench.propose(&format!("shown-{index}"), "run_command", json!({ "command": command })).expect("a card");
        let row = command_row(&card);
        assert!(forbidden_in(&row).is_empty(), "command {index}: a hidden character is on the card");
        assert!(!row.contains('\n') && !row.contains('\t'), "command {index}: a raw line break or tab is on the card");
        hashes.insert(card["hash"].as_str().unwrap_or("").to_owned());
        rows.insert(row.clone());
        if command.contains(rlo) {
            assert!(row.contains(&format!("{bs}u{{202e}}")), "the override is not shown as an escape");
        }
        if command.contains('\n') {
            assert!(row.contains(&format!("{bs}n")), "the line feed is not shown as an escape");
        }
    }
    assert_eq!((hashes.len(), rows.len()), (commands.len(), commands.len()), "two commands share a hash or a row");

    // Two commands that differ only by a zero-width space differ in hash and in row text.
    let plain = bench.propose("zw-1", "run_command", json!({ "command": "echo ab" })).expect("a card");
    let split = bench.propose("zw-2", "run_command", json!({ "command": format!("echo a{zwsp}b") })).expect("a card");
    assert_ne!(plain["hash"], split["hash"]);
    assert_ne!(command_row(&plain), command_row(&split));

    // A command with a NUL, an escape, DEL or a C1 character is refused.
    for (label, code) in [("a NUL", 0), ("an escape", 0x1b), ("DEL", 0x7f), ("a C1 character", 0x9b)] {
        let command = format!("echo{}x", ch(code));
        let refused = code_of(bench.propose("bad", "run_command", json!({ "command": command })), label);
        assert_eq!(refused, "invalid_arguments", "{label}");
    }

    // The escaping is injective over every string of up to three characters from a small hostile alphabet.
    let alphabet = ['a', '\'', bs, rlo, zwsp, '\n'];
    let mut all: Vec<String> = vec![String::new()];
    let mut frontier: Vec<String> = vec![String::new()];
    for _ in 0..3 {
        let mut next = Vec::new();
        for text in &frontier {
            for c in alphabet {
                next.push(format!("{text}{c}"));
            }
        }
        all.extend(next.iter().cloned());
        frontier = next;
    }
    assert_eq!(all.len(), 1 + 6 + 36 + 216);
    let shown: BTreeSet<String> = all.iter().map(|text| visible(text)).collect();
    assert_eq!(shown.len(), all.len(), "two different texts are shown the same");
}

/// A runner over the `PATH` of this process and nothing else, and a bench that runs commands through it.
fn runner_bench(extra: Vec<(OsString, OsString)>) -> Bench {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    let mut base = vec![(OsString::from("PATH"), path)];
    base.extend(extra);
    Bench::with_exec(LocalExec::new(base))
}

/// Allows the card of a command and runs it; the text the host answered.
fn run_command_text(bench: &Bench, id: &str, command: &str) -> String {
    let card = bench.propose(id, "run_command", json!({ "command": command })).expect("a card");
    bench.allow(&card).expect("allowed");
    let output: ToolOutput = bench.run(bench.scope(), id).expect("the command ran");
    output.text().to_owned()
}

#[cfg(unix)]
#[test]
fn a_command_runs_exactly_as_the_card_showed_it() {
    let bench = runner_bench(Vec::new());
    let commands: Vec<String> = vec![
        "echo 'it'\"'\"'s quoted'".to_owned(),
        "echo '$(echo X)'".to_owned(),
        "echo \"$(echo X)\"".to_owned(),
        "echo `echo Y`".to_owned(),
        "echo a; echo b".to_owned(),
        "true && echo and".to_owned(),
        "echo one\necho two".to_owned(),
        "cat <<EOF\nline $(echo Z)\nEOF".to_owned(),
        format!("echo 'Gr{}e {}'", ch(0xf6), ch(0x65e5)),
        "-n".to_owned(),
    ];
    for (index, command) in commands.iter().enumerate() {
        let shown = run_command_text(&bench, &format!("as-written-{index}"), command);
        // The oracle: the same text handed to the same interpreter directly.
        let direct = std::process::Command::new("/bin/sh")
            .args(["-c", "--", command])
            .current_dir(bench.tree().path("proj"))
            .output()
            .expect("the oracle runs");
        let result = ExecResult {
            stdout: direct.stdout,
            stderr: direct.stderr,
            status: direct.status.code().and_then(|code| u32::try_from(code).ok()),
            end: ExecEnd::Finished,
            truncated: false,
            dropped: 0,
        };
        assert_eq!(shown, report(&result, 60).text(), "command {index}: the host did not run what /bin/sh runs");
    }
}

#[cfg(unix)]
#[test]
fn remote_quoting_round_trips_hostile_text_through_a_real_shell() {
    let letters: Vec<char> = {
        let mut all: Vec<char> = "'\"`$\\;&|<>(){}[]*?!~# \t\nab-=".chars().collect();
        all.extend([ch(0xe9), ch(0x65e5), ch(0x1f600), ch(0x200b)]);
        all
    };
    assert_eq!(quote(""), "''");
    for seed in [5u64, 6, 7] {
        let mut rng = Xorshift64::new(seed);
        for case in 0..300 {
            let length = rng.below(41);
            let text: String = (0..length).map(|_| letters[rng.below(letters.len())]).collect();
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("printf %s {}", quote(&text))])
                .output()
                .expect("the shell runs");
            assert_eq!(out.stdout, text.as_bytes(), "seed {seed} case {case}: the text did not round-trip");
        }
    }

    // A folder whose name holds quotes, blanks, `$` and `;`, entered by the string for a remote host.
    let scratch = security::world::Scratch::new("quoting");
    let root = fs::canonicalize(scratch.path()).expect("a root");
    let names: Vec<char> = "'\" $;&`(a b-".chars().collect();
    let mut rng = Xorshift64::new(5);
    for case in 0..100 {
        let length = 1 + rng.below(12);
        let name: String = format!("d{case}-{}", (0..length).map(|_| names[rng.below(names.len())]).collect::<String>());
        let folder = root.join(&name);
        fs::create_dir(&folder).expect("a folder");
        let remote = remote_command("pwd -P", Some(folder.to_str().expect("a UTF-8 path"))).expect("a short string");
        let out = std::process::Command::new("/bin/sh").args(["-c", &remote]).output().expect("the shell runs");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'), folder.to_str().unwrap_or(""), "case {case}: the folder");
    }
    assert!(remote_command(&"a".repeat(MAX_EXEC_COMMAND + 1), None).is_none(), "a string over the limit was made");
}

#[cfg(unix)]
#[test]
fn an_agent_started_process_has_no_session_to_inherit() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = security::world::Scratch::new("spawned");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("a bin folder");
    let stand_in = bin.join("wsh");
    fs::write(&stand_in, "#!/bin/sh\nprintf 'token=%s socket=%s\\n' \"${HOLZWAVE_TOKEN:-none}\" \"${HOLZWAVE_IPC:-none}\"\n").expect("a script");
    fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o755)).expect("a mode");
    // The real binary, when this workspace was built with it: next to the folder of the test executables.
    let real: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(Path::parent).map(|debug| debug.join("wsh")))
        .filter(|path| path.is_file());
    let mut search = vec![bin.clone()];
    if let Some(real) = &real {
        search.extend(real.parent().map(Path::to_path_buf));
    }
    search.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    let path = std::env::join_paths(&search).expect("a path");

    let secret = |name: &str| format!("zq-spawn-secret-{name}");
    let given: Vec<(&str, String)> = vec![
        ("HOLZWAVE_IPC", secret("ipc")),
        ("HOLZWAVE_TOKEN", secret("token")),
        ("HOLZWAVE_PANE", "7".to_owned()),
        ("SSH_AUTH_SOCK", secret("agent")),
        ("SSH_AGENT_PID", secret("pid")),
        ("ANTHROPIC_API_KEY", secret("anthropic")),
        ("OPENAI_API_KEY", secret("openai")),
        ("GEMINI_API_KEY", secret("gemini")),
        ("GITHUB_TOKEN", secret("github")),
        ("AWS_SECRET_ACCESS_KEY", secret("aws")),
        ("DATABASE_URL", secret("database")),
        ("KUBECONFIG", secret("kube")),
        ("DBUS_SESSION_BUS_ADDRESS", secret("dbus")),
        ("XDG_RUNTIME_DIR", secret("xdg")),
        ("SHELL", secret("shell")),
        ("EDITOR", secret("editor")),
        ("LC_AUTH", secret("lcauth")),
        ("TMPDIR_KEY", secret("tmpkey")),
        ("HOME", "/home/zq".to_owned()),
        ("USER", "zquser".to_owned()),
        ("LANG", "C".to_owned()),
    ];
    let mut base: Vec<(OsString, OsString)> = given.iter().map(|(name, value)| (OsString::from(name), OsString::from(value))).collect();
    base.push((OsString::from("PATH"), path.clone()));
    let runner = LocalExec::new(base);
    assert_eq!(runner.env_names(), ["HOME", "LANG", "PATH", "USER"], "the names the runner keeps");
    let bench = Bench::with_exec(runner);

    let printed = run_command_text(&bench, "env", "env");
    for line in printed.lines().filter(|line| line.contains('=') && !line.starts_with("Exit status") && !line.starts_with("Standard")) {
        let name = line.split('=').next().unwrap_or("");
        let known = ["HOME", "LANG", "PATH", "USER", "PWD", "TERM", "SHLVL", "OLDPWD", "_"];
        let locale = name.starts_with("LC_") && !["KEY", "TOKEN", "SECRET", "PASSWORD", "AUTH"].iter().any(|word| name.contains(word));
        assert!(known.contains(&name) || locale, "the process was given the variable {name}");
    }
    assert!(!printed.contains("zq-spawn-secret-"), "a value of the session reached the process");

    let from_stand_in = run_command_text(&bench, "wsh", "wsh");
    assert!(from_stand_in.contains("token=none socket=none"), "the stand-in wsh saw the session: {from_stand_in}");
    match real {
        Some(_) => {
            let asked = run_command_text(&bench, "wsh-ai", &format!("{} ai hi", real_name(&search)));
            assert!(asked.contains("Exit status: 1."), "the real wsh did not exit with status 1");
            assert!(asked.contains("not a holzWave terminal"), "the real wsh found the app");
        }
        None => println!("skipped: the real wsh binary is not built next to the tests"),
    }
}

/// The command that starts the real `wsh` through the path of the runner: the stand-in comes first on that path, so the
/// real one is named by its folder.
#[cfg(unix)]
fn real_name(search: &[PathBuf]) -> String {
    let folder = search.get(1).map_or_else(|| "/nowhere".to_owned(), |folder| folder.display().to_string());
    quote(&format!("{folder}/wsh"))
}

// ---- the service never holds or emits a decision --------------------------------------------------------------------------------

/// Every key of every object in `value`.
fn keys_of(value: &Value, into: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                into.insert(key.clone());
                keys_of(inner, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|inner| keys_of(inner, into)),
        _ => {}
    }
}

fn production_text(source: &str) -> String {
    let mut out = String::new();
    for line in source.lines() {
        if line.trim() == "#[cfg(test)]" {
            break;
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The lines of `text` that hold `needle`.
fn lines_with<'a>(text: &'a str, needle: &str) -> Vec<&'a str> {
    text.lines().filter(|line| line.contains(needle)).collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_service_never_holds_or_emits_a_decision() {
    let turns = vec![
        Script::new().tool_call("w1", "write_file", json!({ "path": "a.txt", "content": "new a\n" })).stop(StopReason::ToolUse),
        Script::reply("done").stop(StopReason::EndTurn),
    ];
    let world = World::new(WorldOptions { turns, ..WorldOptions::default() });
    let chat = world.chat();
    world.ask(&chat, "write a file");
    let card = world.card(&chat, 1, Duration::from_secs(10)).expect("a card");

    // No event and no snapshot has a member that could carry a decision.
    let mut keys = BTreeSet::new();
    for event in world.sink().events(chat.as_str()) {
        keys_of(&event, &mut keys);
    }
    keys_of(&serde_json::to_value(world.service().snapshot(&chat).expect("a snapshot")).expect("json"), &mut keys);
    for word in ["decision", "allow", "approved", "verdict", "consent"] {
        assert!(!keys.contains(word), "a member named {word} is in an event or a snapshot");
    }
    // The card has exactly the pinned members, and `diff` when the host looked at the file.
    let pinned: BTreeSet<&str> =
        ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint", "diff"].into_iter().collect();
    let members: BTreeSet<String> = card.as_object().expect("a card object").keys().cloned().collect();
    assert!(members.iter().all(|member| pinned.contains(member.as_str())), "the card has a member that is not pinned: {members:?}");
    for required in ["conversation", "call_id", "tool", "effect", "pane", "target", "remote", "payload", "hash", "tainted", "taint"] {
        assert!(members.contains(required), "the card lacks {required}");
    }

    // A snapshot shows the same card with the same hash, and a decision after the snapshot works.
    let snapshot = world.service().snapshot(&chat).expect("a snapshot");
    assert_eq!(snapshot.pending.len(), 1);
    assert_eq!(snapshot.pending[0].hash, card["hash"].as_str().unwrap_or(""));
    world.allow(&chat, &card).expect("a decision after a snapshot");
    assert_eq!(world.wait_end(&chat, Duration::from_secs(20)).expect("an end")["end"], "done");

    // The sources: the agent and the service never allow; the loop denies a card nobody answered and nothing else decides.
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&core, &mut files);
    assert!(files.len() > 50, "the scan finds {} files", files.len());
    let mut decide_sites = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(&core).unwrap_or(file).to_string_lossy().into_owned();
        let text = production_text(&fs::read_to_string(file).expect("a source file"));
        if relative.starts_with("ai/agent") || relative.starts_with("ai/service") {
            assert!(!text.contains("Decision::Allow"), "{relative}: the agent or the service names Decision::Allow");
        }
        if relative != "ai/tools/approval.rs" {
            for line in lines_with(&text, ".decide(") {
                decide_sites.push((relative.clone(), line.trim().to_owned()));
            }
        }
    }
    assert_eq!(decide_sites.len(), 1, "a call of decide outside the approval table: {:?}", decide_sites.iter().map(|(file, _)| file).collect::<Vec<_>>());
    assert!(decide_sites[0].0.ends_with("agent.rs") && decide_sites[0].1.contains("Decision::Deny"), "the one call is not the denial of an unanswered card");

    // The scanner has its own self-test.
    assert_eq!(lines_with(&production_text("fn a() { t.decide(c, Decision::Allow); }\n"), ".decide(").len(), 1);
    assert!(lines_with(&production_text("// t.decide(x)\nfn a() {}\n#[cfg(test)]\nmod t { fn b() { t.decide(); } }\n"), ".decide(").is_empty());
}

// ---- accounting -----------------------------------------------------------------------------------------------------------------------

fn error_of_every_kind() -> Vec<ToolError> {
    vec![
        ToolError::UnknownTool(Detail::new("delete_file")),
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
        ToolError::Host(Detail::new("the file cannot be read")),
        ToolError::NotShared,
    ]
}

#[test]
fn the_attack_corpus_reaches_every_refusal_the_table_can_give() {
    let mut reached: BTreeSet<String> = BTreeSet::new();
    for outcomes in [
        denied_outcomes(),
        malformed_outcomes(),
        others_outcomes(),
        retarget_outcomes(),
        changed_outcomes(),
        pane_outcomes(),
        taint_outcomes(),
        once_outcomes(),
    ] {
        reached.extend(outcomes.into_iter().map(|outcome| outcome.code));
    }
    let exempt: BTreeSet<&str> = [EXEMPT.0].into_iter().collect();
    for code in CODES {
        assert!(reached.contains(code) || exempt.contains(code), "no attack of the corpus reaches the refusal {code}");
    }
    for code in &reached {
        assert!(CODES.contains(&code.as_str()), "the corpus reached a code that is not in the table: {code}");
    }
    assert!(!reached.contains(EXEMPT.0), "the exempt code was reached: drop the exemption");
    assert!(EXEMPT.1.len() > 40, "the exemption has no reason");
}

#[test]
fn the_refusal_table_is_pinned_and_never_echoes_the_attack() {
    let errors = error_of_every_kind();
    assert_eq!(errors.len(), 17);
    let codes: Vec<&str> = errors.iter().map(ToolError::code).collect();
    assert_eq!(codes, CODES, "the codes of the table or their order changed");
    for error in &errors {
        let result = error.to_result("call_1");
        assert!(result.is_error, "{}: the result is not an error", error.code());
        assert!(!result.content.is_empty() && result.content.len() <= 300, "{}: the text is {} bytes", error.code(), result.content.len());
        assert!(!result.content.contains('/') && !result.content.contains('\\'), "{}: the text holds a path separator", error.code());
        assert_eq!(format!("{error:?}"), error.code(), "the debug text is the code and nothing else");
    }
}

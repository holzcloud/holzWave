//! The audit log: the line, the file name, the modes and the closed vocabulary.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use holzwave_server::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use holzwave_server::clock::ManualClock;
use holzwave_server::store::AccountId;
use scratch::Scratch;

/// 2026-03-01T08:30:00Z
const NOW: i64 = 1_772_353_800;

#[test]
fn one_event_is_one_json_line_in_the_file_of_the_day() {
    let scratch = Scratch::new("audit-tracer");
    let clock = Arc::new(ManualClock::new(NOW));
    let audit = Audit::open(scratch.path(), clock).expect("open");
    let addr: IpAddr = "203.0.113.9".parse().expect("address");
    let event = Event::new(Action::BootstrapRedeem, Outcome::Ok, Actor::Account(AccountId::from_raw(1)), Some(addr))
        .target(AccountId::from_raw(1))
        .detail(DetailKey::Method, DetailValue::Text("password_totp"));
    audit.record(&event).expect("record");

    let path = scratch.path().join("server/audit/audit-2026-03-01.jsonl");
    let text = std::fs::read_to_string(&path).expect("the file of the day");
    println!("Audit line: {}", text.trim_end());
    assert_eq!(text.lines().count(), 1);
    assert!(text.ends_with('\n'));
    let value: serde_json::Value = serde_json::from_str(text.trim_end()).expect("valid JSON");
    assert_eq!(value["ts"], "2026-03-01T08:30:00Z");
    assert_eq!(value["action"], "bootstrap.redeem");
    assert_eq!(value["result"], "ok");
    assert_eq!(value["actor"], "1");
    assert_eq!(value["addr"], "203.0.113.9");
    assert_eq!(value["target"], "1");
    assert_eq!(value["detail"]["method"], "password_totp");

    let mode = |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o7777;
    println!("Audit modes: dir {:04o}, file {:04o}", mode(path.parent().expect("dir")), mode(&path));
    assert_eq!(mode(path.parent().expect("dir")), 0o700);
    assert_eq!(mode(&path), 0o600);
}

fn audit_in(scratch: &Scratch, start: i64) -> (Arc<ManualClock>, Audit) {
    let clock = Arc::new(ManualClock::new(start));
    let audit = Audit::open(scratch.path(), clock.clone()).expect("open");
    (clock, audit)
}

fn lines_of(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .expect("read the audit file")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("a line is not JSON ({e}): {l}")))
        .collect()
}

fn plain(action: Action) -> Event {
    Event::new(action, Outcome::Ok, Actor::Anonymous, None)
}

/// The actions of the audit vocabulary, as a literal: a new action must change this list.
const ACTIONS: [&str; 26] = [
    "bootstrap.redeem",
    "signin.ok",
    "signin.fail",
    "signin.locked",
    "signout",
    "session.expired",
    "session.step_up",
    "account.create",
    "account.invite",
    "account.redeem",
    "account.disable",
    "account.enable",
    "account.delete",
    "account.role",
    "account.unlock",
    "oidc.create",
    "auth.password.change",
    "auth.totp.enrol",
    "auth.passkey.add",
    "auth.passkey.remove",
    "auth.passphrase.set",
    "recovery.use",
    "recovery.replace",
    "secrets.reset",
    "secrets.unlock",
    "secrets.lock",
];

#[test]
fn the_action_vocabulary_is_the_literal_list() {
    let found: Vec<&str> = Action::ALL.iter().map(|a| a.as_str()).collect();
    assert_eq!(found, ACTIONS);
    let mut sorted = found.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ACTIONS.len(), "no identifier appears twice");
    // Every action writes exactly one line with its identifier.
    let scratch = Scratch::new("audit-actions");
    let (_clock, audit) = audit_in(&scratch, NOW);
    for action in Action::ALL {
        audit.record(&plain(action)).expect("record");
    }
    let lines = lines_of(&scratch.join("server/audit/audit-2026-03-01.jsonl"));
    let written: Vec<&str> = lines.iter().map(|l| l["action"].as_str().expect("action")).collect();
    assert_eq!(written, ACTIONS);
}

#[test]
fn the_detail_keys_are_a_closed_list_and_values_are_typed() {
    let keys: Vec<&str> = DetailKey::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(keys, ["method", "step", "reason", "role", "state", "provider", "count", "days", "slot"]);
    let scratch = Scratch::new("audit-detail");
    let (_clock, audit) = audit_in(&scratch, NOW);
    let event = plain(Action::SigninFail)
        .detail(DetailKey::Reason, DetailValue::Text("bad_code"))
        .detail(DetailKey::Count, DetailValue::Number(3))
        .detail(DetailKey::Step, DetailValue::Bool(true))
        .detail(DetailKey::Count, DetailValue::Number(4));
    audit.record(&event).expect("record");
    let line = &lines_of(&scratch.join("server/audit/audit-2026-03-01.jsonl"))[0];
    assert_eq!(line["detail"], serde_json::json!({"reason": "bad_code", "count": 4, "step": true}));
    assert_eq!(line["detail"].as_object().expect("object").len(), 3, "a second value replaces the first");
}

#[test]
fn text_in_a_value_cannot_break_the_line() {
    let scratch = Scratch::new("audit-escape");
    let (_clock, audit) = audit_in(&scratch, NOW);
    audit
        .record(&plain(Action::SigninFail).detail(DetailKey::Reason, DetailValue::Text("quo\"te\nnew\u{0}line\\")))
        .expect("record");
    let path = scratch.join("server/audit/audit-2026-03-01.jsonl");
    assert_eq!(std::fs::read_to_string(&path).expect("read").lines().count(), 1);
    assert_eq!(lines_of(&path)[0]["detail"]["reason"], "quo\"te\nnew\u{0}line\\");
}

#[test]
fn addresses_actors_and_targets_are_written_by_type() {
    let scratch = Scratch::new("audit-addr");
    let (_clock, audit) = audit_in(&scratch, NOW);
    let v6: IpAddr = "2001:db8::1".parse().expect("v6");
    let v4: IpAddr = "192.0.2.44".parse().expect("v4");
    audit.record(&Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(AccountId::from_raw(7)), Some(v6))).expect("v6");
    audit.record(&Event::new(Action::SigninFail, Outcome::Fail, Actor::Anonymous, Some(v4))).expect("v4");
    audit.record(&plain(Action::SessionExpired).target(AccountId::from_raw(9))).expect("target");
    audit.record(&plain(Action::Signout)).expect("none");
    let lines = lines_of(&scratch.join("server/audit/audit-2026-03-01.jsonl"));
    let fields = |i: usize| {
        let l = &lines[i];
        (
            l["actor"].as_str().expect("actor").to_owned(),
            l["addr"].as_str().expect("addr").to_owned(),
            l["target"].as_str().expect("target").to_owned(),
            l["result"].as_str().expect("result").to_owned(),
        )
    };
    assert_eq!(fields(0), ("7".into(), "2001:db8::1".into(), "".into(), "ok".into()));
    assert_eq!(fields(1), ("anonymous".into(), "192.0.2.44".into(), "".into(), "fail".into()));
    assert_eq!(fields(2), ("anonymous".into(), "".into(), "9".into(), "ok".into()));
    assert_eq!(fields(3), ("anonymous".into(), "".into(), "".into(), "ok".into()));
    for line in &lines {
        let mut keys: Vec<&str> = line.as_object().expect("object").keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["action", "actor", "addr", "detail", "result", "target", "ts"]);
    }
}

#[test]
fn a_day_change_opens_the_next_file() {
    let scratch = Scratch::new("audit-rotate");
    // 2026-03-01T23:59:59Z
    let (clock, audit) = audit_in(&scratch, 1_772_409_599);
    audit.record(&plain(Action::SigninOk)).expect("before midnight");
    clock.advance(2);
    audit.record(&plain(Action::Signout)).expect("after midnight");
    let day1 = scratch.join("server/audit/audit-2026-03-01.jsonl");
    let day2 = scratch.join("server/audit/audit-2026-03-02.jsonl");
    let (one, two) = (lines_of(&day1), lines_of(&day2));
    assert_eq!((one.len(), two.len()), (1, 1));
    assert_eq!(one[0]["ts"], "2026-03-01T23:59:59Z");
    assert_eq!(two[0]["ts"], "2026-03-02T00:00:01Z");
    let mode = |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o7777;
    assert_eq!((mode(&day1), mode(&day2)), (0o600, 0o600));
}

#[test]
fn eight_threads_write_sixteen_hundred_intact_lines_through_one_log() {
    let scratch = Scratch::new("audit-shared");
    let (_clock, audit) = audit_in(&scratch, NOW);
    write_in_parallel(&scratch, |_| &audit);
}

#[test]
fn eight_threads_with_a_log_each_still_write_sixteen_hundred_intact_lines() {
    let scratch = Scratch::new("audit-separate");
    let clock = Arc::new(ManualClock::new(NOW));
    let logs: Vec<Audit> = (0..8).map(|_| Audit::open(scratch.path(), clock.clone()).expect("open")).collect();
    write_in_parallel(&scratch, |t| &logs[t]);
}

/// 8 threads x 200 lines; every (thread, number) pair must appear exactly once and every line must be whole.
fn write_in_parallel<'a>(scratch: &Scratch, log_of: impl Fn(usize) -> &'a Audit + Sync) {
    std::thread::scope(|scope| {
        for t in 0..8usize {
            let log_of = &log_of;
            scope.spawn(move || {
                let audit = log_of(t);
                for n in 0..200i64 {
                    let event = Event::new(Action::SigninOk, Outcome::Ok, Actor::Account(AccountId::from_raw(t as i64)), None)
                        .detail(DetailKey::Slot, DetailValue::Number(t as i64))
                        .detail(DetailKey::Count, DetailValue::Number(n));
                    audit.record(&event).expect("record");
                }
            });
        }
    });
    let lines = lines_of(&scratch.join("server/audit/audit-2026-03-01.jsonl"));
    println!("Audit lines: expected 1600, found {}", lines.len());
    assert_eq!(lines.len(), 1600);
    let mut seen = std::collections::BTreeSet::new();
    for line in &lines {
        assert_eq!(line["action"], "signin.ok");
        let pair = (line["detail"]["slot"].as_i64().expect("slot"), line["detail"]["count"].as_i64().expect("count"));
        assert!(seen.insert(pair), "a line appeared twice: {pair:?}");
    }
    assert_eq!(seen.len(), 1600);
}

#[test]
fn a_failed_write_is_an_error_not_a_panic_and_the_next_call_tries_again() {
    if rustix::process::geteuid().is_root() {
        println!("NOT RUN as root: a read-only directory does not stop root");
        return;
    }
    let scratch = Scratch::new("audit-readonly");
    let (_clock, audit) = audit_in(&scratch, NOW);
    let dir = scratch.join("server/audit");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("read-only");
    let failed = audit.record(&plain(Action::Signout)).expect_err("the directory is read-only");
    let text = failed.to_string();
    assert!(!text.contains(scratch.path().to_str().expect("utf-8 path")), "no path in the error: {text}");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("writable again");
    audit.record(&plain(Action::Signout)).expect("the next call works");
    assert_eq!(lines_of(&dir.join("audit-2026-03-01.jsonl")).len(), 1);
}

#[test]
fn a_link_in_place_of_the_day_file_is_refused() {
    let scratch = Scratch::new("audit-link");
    let (_clock, audit) = audit_in(&scratch, NOW);
    let target = scratch.write_file("target.txt", b"", 0o600);
    std::os::unix::fs::symlink(&target, scratch.join("server/audit/audit-2026-03-01.jsonl")).expect("link");
    assert!(audit.record(&plain(Action::Signout)).is_err());
    assert_eq!(std::fs::read(&target).expect("read"), b"", "nothing was written through the link");
}

#[test]
fn a_day_file_with_a_loose_mode_is_made_private_before_a_line_goes_in() {
    let scratch = Scratch::new("audit-loose");
    let (_clock, audit) = audit_in(&scratch, NOW);
    let file = scratch.join("server/audit/audit-2026-03-01.jsonl");
    std::fs::write(&file, b"").expect("file");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("mode");
    audit.record(&plain(Action::Signout)).expect("record");
    assert_eq!(std::fs::metadata(&file).expect("metadata").permissions().mode() & 0o7777, 0o600);
}

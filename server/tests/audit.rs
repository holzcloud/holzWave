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

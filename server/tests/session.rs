//! Sessions: the cookie value, the lifetimes and what ends a session.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use holzwave_server::auth::session::{Method, SessionError, SessionManager};
use holzwave_server::clock::ManualClock;
use holzwave_server::config::Sessions;
use holzwave_server::store::{AccountId, NewAccount, Role, Store};
use scratch::Scratch;

const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

struct Fixture {
    _scratch: Scratch,
    store: Arc<Store>,
    clock: Arc<ManualClock>,
    manager: SessionManager,
}

fn fixture(tag: &str) -> (Fixture, AccountId) {
    let scratch = Scratch::new(tag);
    let store = Arc::new(Store::open(scratch.path()).expect("open"));
    let clock = Arc::new(ManualClock::new(NOW));
    let manager = SessionManager::new(store.clone(), clock.clone(), Sessions::default());
    let alice =
        store.write(|tx| tx.account_create(&NewAccount::new("alice", Role::Member), RANGE, NOW)).expect("account").id;
    (Fixture { _scratch: scratch, store, clock, manager }, alice)
}

#[test]
fn tracer_a_session_is_found_until_it_has_been_idle_too_long() {
    let (f, alice) = fixture("session-tracer");
    let created = f.manager.create(alice, Method::PasswordTotp, NOW, "Firefox on Linux", Some(ADDR)).expect("create");
    let value = created.value.expose().to_owned();
    println!("Tracer: value length = {}", value.len());
    assert_eq!(value.len(), 43);
    let found = f.manager.lookup(&value, NOW).expect("lookup");
    assert_eq!(found.account_id, alice);
    assert_eq!(f.store.read(|tx| tx.session_list(alice)).expect("list").len(), 1);
    f.clock.advance(1_801);
    let late = f.manager.lookup(&value, f.manager.now());
    println!("Tracer: after the idle time = {late:?}");
    assert_eq!(late.expect_err("idle"), SessionError::Expired);
}

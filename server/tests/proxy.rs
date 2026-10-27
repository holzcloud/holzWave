//! The trusted-proxy rule, the client address and the connection admission.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use holzwave_server::config::Cidr;
use holzwave_server::limits;
use holzwave_server::proxy::{
    Admission, MAX_FORWARDED_BYTES, MAX_FORWARDED_ENTRIES, Mode, ProxyError, Refusal, TrustedProxies,
};

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap_or_else(|_| panic!("{text} is an address"))
}

fn trusted(list: &[&str]) -> TrustedProxies {
    let nets: Vec<Cidr> = list.iter().map(|t| Cidr::parse(t).expect("a network")).collect();
    TrustedProxies::new(&nets).expect("trusted proxies")
}

/// The usual setting: the proxies live in 10.0.0.0/8.
fn proxies() -> TrustedProxies {
    trusted(&["10.0.0.0/8"])
}

/// One row of the table: the mode, the peer, the lines of the two headers and the expected answer.
struct Row {
    mode: Mode,
    peer: &'static str,
    xff: &'static [&'static str],
    xfp: &'static [&'static str],
    want: Result<&'static str, Refusal>,
}

fn run_table(trusted: &TrustedProxies, rows: &[Row]) {
    for row in rows {
        let got = trusted.client_address(row.mode, ip(row.peer), row.xff, row.xfp);
        let want = row.want.map(ip);
        let shown = |lines: &[&str]| {
            let text = format!("{lines:?}");
            if text.len() > 70 { format!("{}... ({} bytes)", &text[..60], text.len()) } else { text }
        };
        println!("Forwarded table: {:?} peer={} xff={} xfp={} -> {:?}", row.mode, row.peer, shown(row.xff), shown(row.xfp), got);
        assert_eq!(got, want, "{:?} peer={} xff={:?} xfp={:?}", row.mode, row.peer, row.xff, row.xfp);
    }
}

#[test]
fn files_and_acme_modes_ignore_the_forwarded_headers_completely() {
    let rows = [
        Row { mode: Mode::Files, peer: "203.0.113.9", xff: &["198.51.100.7"], xfp: &["https"], want: Ok("203.0.113.9") },
        Row { mode: Mode::Acme, peer: "203.0.113.9", xff: &["198.51.100.7"], xfp: &["http"], want: Ok("203.0.113.9") },
        Row { mode: Mode::Files, peer: "10.1.2.3", xff: &["198.51.100.7"], xfp: &["https"], want: Ok("10.1.2.3") },
        // Not even a malformed header is looked at.
        Row { mode: Mode::Files, peer: "203.0.113.9", xff: &["not-an-ip"], xfp: &["a", "b"], want: Ok("203.0.113.9") },
    ];
    // The list of trusted proxies makes no difference outside the proxy mode.
    run_table(&TrustedProxies::none(), &rows);
    run_table(&proxies(), &rows);
}

#[test]
fn a_trusted_proxy_names_the_client() {
    run_table(
        &proxies(),
        &[
            Row { mode: Mode::Proxy, peer: "10.1.2.3", xff: &["198.51.100.7"], xfp: &["https"], want: Ok("198.51.100.7") },
            Row { mode: Mode::Proxy, peer: "10.1.2.3", xff: &[" 198.51.100.7 "], xfp: &["HTTPS"], want: Ok("198.51.100.7") },
            // No address header: the proxy itself is the client (a health check).
            Row { mode: Mode::Proxy, peer: "10.1.2.3", xff: &[], xfp: &["https"], want: Ok("10.1.2.3") },
        ],
    );
}

#[test]
fn only_the_right_side_of_the_list_is_believed() {
    run_table(
        &proxies(),
        &[
            // The client wrote "1.1.1.1"; the proxy appended the address it saw and then another proxy added itself.
            Row {
                mode: Mode::Proxy,
                peer: "10.1.2.3",
                xff: &["1.1.1.1, 198.51.100.7, 10.9.9.9"],
                xfp: &["https"],
                want: Ok("198.51.100.7"),
            },
            // Every entry is a trusted proxy: the left-most.
            Row { mode: Mode::Proxy, peer: "10.1.2.3", xff: &["10.9.9.9, 10.8.8.8"], xfp: &["https"], want: Ok("10.9.9.9") },
            // A forged trusted-looking entry on the left changes nothing.
            Row {
                mode: Mode::Proxy,
                peer: "10.1.2.3",
                xff: &["10.0.0.1, 203.0.113.50, 198.51.100.7"],
                xfp: &["https"],
                want: Ok("198.51.100.7"),
            },
            // Several header lines are one list, in order.
            Row {
                mode: Mode::Proxy,
                peer: "10.1.2.3",
                xff: &["1.1.1.1", "198.51.100.7, 10.9.9.9"],
                xfp: &["https"],
                want: Ok("198.51.100.7"),
            },
        ],
    );
}

#[test]
fn an_untrusted_peer_in_the_proxy_mode_is_refused_at_admission_and_its_headers_are_never_believed() {
    let proxies = proxies();
    // Called anyway, the answer is the peer, never the header value.
    run_table(
        &proxies,
        &[Row {
            mode: Mode::Proxy,
            peer: "192.0.2.50",
            xff: &["198.51.100.7"],
            xfp: &["https"],
            want: Ok("192.0.2.50"),
        }],
    );
    let admission = Admission::with_defaults(Mode::Proxy, proxies).expect("admission");
    assert_eq!(admission.try_admit(ip("192.0.2.50")).err(), Some(Refusal::UntrustedPeer));
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn unusable_forwarded_values_from_a_trusted_proxy_are_refused() {
    let long = "1".repeat(MAX_FORWARDED_BYTES + 1);
    let seventeen = vec!["198.51.100.7"; MAX_FORWARDED_ENTRIES + 1].join(", ");
    let kib_plus_one = format!("{}198.51.100.7", " ".repeat(MAX_FORWARDED_BYTES + 1 - "198.51.100.7".len()));
    assert_eq!(kib_plus_one.len(), 1025);
    let seventeen: &'static str = Box::leak(seventeen.into_boxed_str());
    let kib_plus_one: &'static str = Box::leak(kib_plus_one.into_boxed_str());
    let long: &'static str = Box::leak(long.into_boxed_str());
    let row = |xff: &'static [&'static str], xfp: &'static [&'static str], want: Refusal| Row {
        mode: Mode::Proxy,
        peer: "10.1.2.3",
        xff,
        xfp,
        want: Err(want),
    };
    let xff_17: &'static [&'static str] = Box::leak(Box::new([seventeen]));
    let xff_1025: &'static [&'static str] = Box::leak(Box::new([kib_plus_one]));
    let xff_long: &'static [&'static str] = Box::leak(Box::new([long]));
    run_table(
        &proxies(),
        &[
            row(&["not-an-ip"], &["https"], Refusal::BadForwardedFor),
            row(&["198.51.100.7, not-an-ip"], &["https"], Refusal::BadForwardedFor),
            row(&["198.51.100.7:443"], &["https"], Refusal::BadForwardedFor),
            row(&["198.51.100.7,"], &["https"], Refusal::BadForwardedFor),
            row(&[""], &["https"], Refusal::BadForwardedFor),
            row(xff_17, &["https"], Refusal::BadForwardedFor),
            row(xff_1025, &["https"], Refusal::BadForwardedFor),
            row(xff_long, &["https"], Refusal::BadForwardedFor),
            row(&["198.51.100.7"], &[], Refusal::NotSecure),
            row(&["198.51.100.7"], &["http"], Refusal::NotSecure),
            row(&["198.51.100.7"], &["https, http"], Refusal::NotSecure),
            row(&["198.51.100.7"], &["https", "https"], Refusal::NotSecure),
            row(&["198.51.100.7"], &[""], Refusal::NotSecure),
            row(&[], &[], Refusal::NotSecure),
        ],
    );
}

#[test]
fn the_bounds_themselves_are_accepted() {
    let sixteen = vec!["198.51.100.7"; MAX_FORWARDED_ENTRIES].join(",");
    assert!(sixteen.len() < MAX_FORWARDED_BYTES);
    let exact = format!("{}198.51.100.7", " ".repeat(MAX_FORWARDED_BYTES - "198.51.100.7".len()));
    assert_eq!(exact.len(), MAX_FORWARDED_BYTES);
    let proxies = proxies();
    assert_eq!(
        proxies.client_address(Mode::Proxy, ip("10.1.2.3"), &[&sixteen], &["https"]),
        Ok(ip("198.51.100.7"))
    );
    assert_eq!(proxies.client_address(Mode::Proxy, ip("10.1.2.3"), &[&exact], &["https"]), Ok(ip("198.51.100.7")));
}

#[test]
fn an_ipv4_mapped_address_is_the_ipv4_address_and_ipv6_clients_work() {
    run_table(
        &proxies(),
        &[
            Row {
                mode: Mode::Proxy,
                peer: "10.1.2.3",
                xff: &["::ffff:198.51.100.7"],
                xfp: &["https"],
                want: Ok("198.51.100.7"),
            },
            // A mapped peer is the trusted IPv4 proxy, and a mapped trusted entry is skipped like the IPv4 one.
            Row {
                mode: Mode::Proxy,
                peer: "::ffff:10.1.2.3",
                xff: &["198.51.100.7, ::ffff:10.9.9.9"],
                xfp: &["https"],
                want: Ok("198.51.100.7"),
            },
            Row {
                mode: Mode::Proxy,
                peer: "10.1.2.3",
                xff: &["2001:db8::7"],
                xfp: &["https"],
                want: Ok("2001:db8::7"),
            },
            Row { mode: Mode::Files, peer: "::ffff:203.0.113.9", xff: &[], xfp: &[], want: Ok("203.0.113.9") },
        ],
    );
    // An IPv6 proxy network works the same way.
    let v6 = trusted(&["fd00::/8"]);
    assert_eq!(v6.client_address(Mode::Proxy, ip("fd00::1"), &["2001:db8::7, fd00::2"], &["https"]), Ok(ip("2001:db8::7")));
}

#[test]
fn the_list_of_trusted_proxies_is_checked() {
    assert_eq!(TrustedProxies::new(&[]).err(), Some(ProxyError::NoTrustedProxies));
    let many: Vec<Cidr> = (0..=256u32).map(|i| Cidr::parse(&format!("10.{}.{}.1", i / 256, i % 256)).expect("address")).collect();
    assert_eq!(TrustedProxies::new(&many).err(), Some(ProxyError::TooManyTrustedProxies));
    assert!(TrustedProxies::new(&many[..256]).is_ok());
    assert!(!TrustedProxies::none().contains(ip("10.0.0.1")));
    // The proxy mode without a proxy is a mistake that no admission is built from.
    assert_eq!(Admission::with_defaults(Mode::Proxy, TrustedProxies::none()).err(), Some(ProxyError::NoTrustedProxies));
    assert!(Admission::with_defaults(Mode::Files, TrustedProxies::none()).is_ok());
}

// ---------------------------------------------------------------------------------------------------------------------
// Admission
// ---------------------------------------------------------------------------------------------------------------------

fn files() -> Admission {
    Admission::with_defaults(Mode::Files, TrustedProxies::none()).expect("admission")
}

#[test]
fn sixty_four_connections_from_one_address_succeed_and_the_next_is_refused() {
    let cap = limits::MAX_CONNECTIONS_PER_ADDRESS as usize;
    assert_eq!(cap, 64);
    let admission = files();
    let mut permits: Vec<_> = (0..cap).map(|_| admission.try_admit(ip("192.0.2.1")).expect("admitted")).collect();
    assert_eq!(admission.open_connections(), 64);
    assert_eq!(admission.try_admit(ip("192.0.2.1")).err(), Some(Refusal::TooManyFromAddress));
    // The refusal took no slot.
    assert_eq!(admission.open_connections(), 64);
    // Another address is not affected.
    let other = admission.try_admit(ip("192.0.2.2")).expect("another address");
    drop(other);
    // One slot given back allows one more.
    permits.pop();
    let again = admission.try_admit(ip("192.0.2.1")).expect("one slot was free");
    assert_eq!(admission.try_admit(ip("192.0.2.1")).err(), Some(Refusal::TooManyFromAddress));
    drop(again);
    drop(permits);
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn an_ipv6_address_counts_by_its_64_bit_prefix() {
    let admission = files();
    let held: Vec<_> = (0..64).map(|_| admission.try_admit(ip("2001:db8:1:2::1")).expect("admitted")).collect();
    // The same /64, another interface identifier.
    assert_eq!(admission.try_admit(ip("2001:db8:1:2:ffff::9")).err(), Some(Refusal::TooManyFromAddress));
    assert_eq!(admission.try_admit(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")).err(), Some(Refusal::TooManyFromAddress));
    // The next /64 is a different client.
    let other = admission.try_admit(ip("2001:db8:1:3::1")).expect("another /64");
    drop(other);
    drop(held);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn an_ipv4_mapped_address_is_counted_as_the_ipv4_address() {
    let admission = files();
    let held: Vec<_> = (0..64).map(|_| admission.try_admit(ip("192.0.2.1")).expect("admitted")).collect();
    assert_eq!(admission.try_admit(ip("::ffff:192.0.2.1")).err(), Some(Refusal::TooManyFromAddress));
    drop(held);
}

#[test]
fn the_total_is_capped_across_addresses() {
    let cap = limits::MAX_CONNECTIONS;
    assert_eq!(cap, 1024);
    let admission = files();
    // 1,024 connections from 128 addresses with 8 each.
    let mut held = Vec::new();
    for host in 0..128u32 {
        for _ in 0..8 {
            held.push(admission.try_admit(ip(&format!("198.51.100.{host}"))).expect("admitted"));
        }
    }
    assert_eq!(admission.open_connections(), cap);
    assert_eq!(admission.try_admit(ip("203.0.113.1")).err(), Some(Refusal::TooManyConnections));
    assert_eq!(admission.open_connections(), cap);
    assert_eq!(admission.tracked_addresses(), 128, "the refused address left no entry");
    held.pop();
    drop(admission.try_admit(ip("203.0.113.1")).expect("a slot was free"));
    drop(held);
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn in_the_proxy_mode_only_the_proxies_connect_and_the_per_address_cap_does_not_apply_to_them() {
    let admission = Admission::with_defaults(Mode::Proxy, proxies()).expect("admission");
    // 100 connections from one proxy are more than the per-address cap of 64.
    let held: Vec<_> = (0..100).map(|_| admission.try_admit(ip("10.1.2.3")).expect("a proxy is admitted")).collect();
    assert_eq!(admission.open_connections(), 100);
    assert_eq!(admission.tracked_addresses(), 0);
    assert_eq!(admission.try_admit(ip("192.0.2.50")).err(), Some(Refusal::UntrustedPeer));
    assert_eq!(admission.try_admit(ip("2001:db8::1")).err(), Some(Refusal::UntrustedPeer));
    // The mapped form of a proxy is the proxy.
    drop(admission.try_admit(ip("::ffff:10.1.2.3")).expect("mapped proxy"));
    // The total still applies to the proxies.
    let mut more: Vec<_> = (100..limits::MAX_CONNECTIONS).map(|_| admission.try_admit(ip("10.1.2.3")).expect("admitted")).collect();
    assert_eq!(admission.try_admit(ip("10.1.2.3")).err(), Some(Refusal::TooManyConnections));
    more.clear();
    drop(held);
    assert_eq!(admission.open_connections(), 0);
}

#[test]
fn a_counter_at_zero_leaves_no_entry() {
    let admission = files();
    let one = admission.try_admit(ip("192.0.2.1")).expect("admitted");
    let two = admission.try_admit(ip("192.0.2.1")).expect("admitted");
    assert_eq!(admission.tracked_addresses(), 1);
    drop(one);
    assert_eq!(admission.tracked_addresses(), 1);
    drop(two);
    assert_eq!(admission.tracked_addresses(), 0);
    // A refused attempt on a full table leaves nothing either.
    let tiny = Admission::new(Mode::Files, TrustedProxies::none(), 1, 1).expect("admission");
    let held = tiny.try_admit(ip("192.0.2.1")).expect("admitted");
    assert_eq!(tiny.try_admit(ip("192.0.2.9")).err(), Some(Refusal::TooManyConnections));
    assert_eq!(tiny.try_admit(ip("192.0.2.1")).err(), Some(Refusal::TooManyConnections));
    assert_eq!(tiny.tracked_addresses(), 1);
    drop(held);
    assert_eq!(tiny.tracked_addresses(), 0);
}

#[test]
fn a_permit_moved_to_another_thread_is_released_there() {
    let admission = files();
    let permit = admission.try_admit(ip("192.0.2.1")).expect("admitted");
    std::thread::spawn(move || drop(permit)).join().expect("thread");
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn thirty_two_threads_admit_and_drop_ten_thousand_permits_each_and_end_at_zero() {
    const THREADS: usize = 32;
    const CYCLES: usize = 10_000;
    let admission = files();
    let admitted = Arc::new(AtomicU32::new(0));
    let handles: Vec<_> = (0..THREADS)
        .map(|n| {
            let admission = admission.clone();
            let admitted = Arc::clone(&admitted);
            std::thread::spawn(move || {
                // Eight threads share one address, four share one /64, the rest have their own, so the counters of
                // the same entry are fought over.
                let address = match n % 4 {
                    0 => ip("192.0.2.1"),
                    1 => ip(&format!("2001:db8:0:1:{n:x}::1")),
                    2 => ip("192.0.2.2"),
                    _ => ip(&format!("198.51.100.{n}")),
                };
                for _ in 0..CYCLES {
                    // At most one slot per thread is held, far below every cap, so every attempt must be admitted.
                    let permit = admission.try_admit(address).expect("admitted");
                    admitted.fetch_add(1, Ordering::Relaxed);
                    drop(permit);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no thread panicked");
    }
    println!(
        "Admission race: {THREADS} threads x {CYCLES} cycles, admitted {}, final total {}, map size {}",
        admitted.load(Ordering::Relaxed),
        admission.open_connections(),
        admission.tracked_addresses()
    );
    assert_eq!(admitted.load(Ordering::Relaxed) as usize, THREADS * CYCLES);
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

#[test]
fn threads_that_fight_for_the_last_slots_never_exceed_the_cap() {
    let admission = Admission::new(Mode::Files, TrustedProxies::none(), 1024, 8).expect("admission");
    let most = Arc::new(AtomicU32::new(0));
    let current = Arc::new(AtomicU32::new(0));
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let (admission, most, current) = (admission.clone(), Arc::clone(&most), Arc::clone(&current));
            std::thread::spawn(move || {
                for _ in 0..5_000 {
                    if let Ok(permit) = admission.try_admit(ip("192.0.2.1")) {
                        let now = current.fetch_add(1, Ordering::AcqRel) + 1;
                        most.fetch_max(now, Ordering::AcqRel);
                        current.fetch_sub(1, Ordering::AcqRel);
                        drop(permit);
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no thread panicked");
    }
    assert!(most.load(Ordering::Acquire) <= 8, "{} permits were held at once for one address", most.load(Ordering::Acquire));
    assert_eq!(admission.open_connections(), 0);
    assert_eq!(admission.tracked_addresses(), 0);
}

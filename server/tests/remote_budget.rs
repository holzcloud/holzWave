//! What the client of the device socket costs: the time one typed byte needs more than it needs without the client, and the CPU
//! that a silent attached session uses. The budget is 20 ms of added median latency and 1 % of one core.
//!
//! The numbers are measured against the real server on a loopback network and printed (run with `--nocapture`). The file holds
//! one test on purpose: the CPU of the process is read in a quiet window, and a second test in the same process would fill it.
//! Everything here needs the `in-process-workers` feature, and the page must have been built.
#![cfg(feature = "in-process-workers")]

#[path = "support/device_socket.rs"]
mod device_socket;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/remote_env.rs"]
mod remote_env;
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::time::{Duration, Instant};

use device_socket::Device;
use holzwave_core::remote::hub::SubEvent;
use remote_env::{Remote, blocking, open_pane, until_prompt};

const SAMPLES: usize = 60;
const BUDGET_LATENCY: Duration = Duration::from_millis(20);
const BUDGET_CPU: f64 = 0.01;
const WINDOW: Duration = Duration::from_secs(15);

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn percentile(values: &[Duration], p: usize) -> Duration {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted[(sorted.len() * p / 100).min(sorted.len() - 1)]
}

/// The CPU seconds this process has used, user and system together.
fn cpu_seconds() -> f64 {
    let pid = std::process::id().to_string();
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/self/stat").expect("the stat of the process");
        // After the name in parentheses: state, then 11 more fields, then utime and stime in clock ticks (100 a second).
        let rest = &stat[stat.rfind(')').expect("a name") + 2..];
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
        let _ = pid;
        ticks / 100.0
    }
    #[cfg(not(target_os = "linux"))]
    {
        // `[[dd-]hh:]mm:ss.cc`
        let out = std::process::Command::new("ps").args(["-o", "time=", "-p", &pid]).output().expect("ps runs");
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mut seconds = 0.0;
        for part in text.split([':', '-']) {
            seconds = seconds * 60.0 + part.parse::<f64>().expect("a number");
        }
        seconds
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_adds_little_latency_to_a_typed_byte_and_a_silent_session_costs_next_to_nothing() {
    let remote = Remote::start().await;
    let (hub, laptop, _endpoint) = remote.hub("laptop");

    // A comment line keeps every typed byte a harmless one: the shell echoes it and runs nothing.
    let new_session = |hub: holzwave_core::remote::hub::Hub| {
        blocking(move || {
            let (session, _) = hub.new_session(120, 24).expect("a new session");
            session
        })
    };

    // 1. The server without the client: a device socket of the test rig types a byte and waits for its echo.
    let mut device = Device::connect(&remote.env, &laptop.token).await;
    let session = device.new_session().await;
    device.attach(session, None).await;
    device.until("the prompt", Duration::from_secs(20), |d| d.live_text(session).contains('$') || !d.snapshot_text(session).trim().is_empty()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    while device.pump(Duration::from_millis(50)).await {}
    device.type_into(session, b"#").await;
    let mut baseline = Vec::new();
    for n in 0..SAMPLES {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let seen = device.live.get(&session).map_or(0, Vec::len);
        let started = Instant::now();
        device.type_into(session, &[b'a' + (n % 26) as u8]).await;
        device.until("the echo", Duration::from_secs(5), |d| d.live.get(&session).map_or(0, Vec::len) > seen).await;
        baseline.push(started.elapsed());
    }
    drop(device);

    // 2. The hub alone: an input byte of the subscription and the live bytes that come back.
    let session = new_session(hub.clone()).await;
    let hub_times = blocking({
        let hub = hub.clone();
        move || {
            let sub = hub.attach(session, None);
            // The grid, the snapshot and the prompt: until the subscription has been quiet for a while.
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut attached = false;
            while Instant::now() < deadline {
                match sub.recv(Duration::from_millis(500)) {
                    Some(SubEvent::Attached { .. }) => attached = true,
                    Some(_) => {}
                    None if attached => break,
                    None => {}
                }
            }
            assert!(attached, "the subscription attached");
            let handle = sub.handle();
            handle.input(b"#").expect("input");
            std::thread::sleep(Duration::from_millis(100));
            while sub.recv(Duration::from_millis(50)).is_some() {}
            let mut times = Vec::new();
            for n in 0..SAMPLES {
                std::thread::sleep(Duration::from_millis(30));
                while sub.recv(Duration::from_millis(1)).is_some() {}
                let started = Instant::now();
                handle.input(&[b'a' + (n % 26) as u8]).expect("input");
                loop {
                    match sub.recv(Duration::from_secs(5)) {
                        Some(SubEvent::Live(_)) => break,
                        Some(_) => {}
                        None => panic!("no echo"),
                    }
                }
                times.push(started.elapsed());
            }
            handle.close();
            times
        }
    })
    .await;

    // 3. The whole pane: from the call that types a byte to the frame that shows its echo.
    let session = new_session(hub.clone()).await;
    let (pane, probe) = open_pane(&hub, session, (120, 24));
    until_prompt(&pane).await;
    pane.text("#".to_owned()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let frames = probe.stamps();
    let mut pane_times = Vec::new();
    for n in 0..SAMPLES {
        tokio::time::sleep(Duration::from_millis(40)).await;
        while frames.try_recv().is_ok() {}
        let started = Instant::now();
        pane.text(((b'a' + (n % 26) as u8) as char).to_string()).unwrap();
        let frame = frames.recv_timeout(Duration::from_secs(5)).expect("a frame with the echo");
        pane_times.push(frame.duration_since(started));
    }

    // The first byte after a pause: the thread rests while nothing moves and is quick again when something does.
    let mut paused_times = Vec::new();
    for n in 0..12 {
        tokio::time::sleep(Duration::from_millis(1400)).await;
        while frames.try_recv().is_ok() {}
        let started = Instant::now();
        pane.text(((b'a' + (n % 26) as u8) as char).to_string()).unwrap();
        let frame = frames.recv_timeout(Duration::from_secs(5)).expect("a frame with the echo");
        paused_times.push(frame.duration_since(started));
    }
    let paused_median = median(paused_times.clone());
    println!(
        "  pane, the first byte after a pause of 1.4 s, median / worst of 12: {:?} / {:?}",
        paused_median,
        paused_times.iter().max().unwrap()
    );
    assert!(paused_median <= BUDGET_LATENCY, "the first byte after a pause needs {paused_median:?}");

    let base = median(baseline.clone());
    let hub_median = median(hub_times.clone());
    let pane_median = median(pane_times.clone());
    let added_hub = hub_median.saturating_sub(base);
    let added_pane = pane_median.saturating_sub(base);
    println!(
        "latency of a typed byte, median / 95th / worst of {SAMPLES}:\n  server only (a test device on the same server): {:?} / {:?} / {:?}\n  hub (input of the subscription to the live bytes): {:?} / {:?} / {:?}\n  pane (typed in the pane to the frame that shows it): {:?} / {:?} / {:?}\n  added by the client: hub {:?}, pane {:?} (budget {:?})",
        base,
        percentile(&baseline, 95),
        baseline.iter().max().unwrap(),
        hub_median,
        percentile(&hub_times, 95),
        hub_times.iter().max().unwrap(),
        pane_median,
        percentile(&pane_times, 95),
        pane_times.iter().max().unwrap(),
        added_hub,
        added_pane,
        BUDGET_LATENCY,
    );
    assert!(added_pane <= BUDGET_LATENCY, "the pane adds {added_pane:?} to a typed byte, over the budget of {BUDGET_LATENCY:?}");

    // 4. A silent attached session: the CPU of the process in a quiet window with the pane attached, against the same window
    // with the hub idle and nothing attached (its socket closed, its thread blocked). The server is in this process, so the
    // difference includes what the server does for an attachment: it is an upper bound for the client.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let cpu_before = cpu_seconds();
    let clock = Instant::now();
    tokio::time::sleep(WINDOW).await;
    let attached = (cpu_seconds() - cpu_before) / clock.elapsed().as_secs_f64();
    pane.close();
    assert!(pane.join(Duration::from_secs(10)));
    hub.disconnect();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let cpu_before = cpu_seconds();
    let clock = Instant::now();
    tokio::time::sleep(WINDOW).await;
    let nothing = (cpu_seconds() - cpu_before) / clock.elapsed().as_secs_f64();
    let cost = (attached - nothing).max(0.0);
    println!(
        "CPU of the process in {WINDOW:?} windows (the server runs in it): a silent attached session {:.3} %, nothing attached {:.3} %, difference {:.3} % of one core (budget {:.0} %)",
        attached * 100.0,
        nothing * 100.0,
        cost * 100.0,
        BUDGET_CPU * 100.0
    );
    assert!(cost < BUDGET_CPU, "a silent attached session costs {:.2} % of a core", cost * 100.0);
}

//! The server's sync and remote sessions as one scenario through one real server: the real router, store, audit and in-process
//! workers behind a real TLS listener, the apps' own runtimes (sign-in, sync, the core that shows panes and its `Hub`, the one
//! connection of a device to the server's sessions) on scratch folders with keychain stand-ins, the person's page on the page
//! socket, and real `/bin/sh` shells on real pseudo terminals.
//!
//! Each plan of the work proved its own part against the server; this file proves that the parts fit. In order: both switches
//! off hide every route of the feature; an administrator turns them on; the apps sign in through the grant the person approves
//! on the page; two devices of one person sync through the server and end with the same files; a session made on one device is
//! watched and taken over on the other and in the browser, with one writer at any time; a second person sees nothing of the
//! first; a revocation ends exactly the revoked device; the remote switch ends the sockets and not the session; the sync
//! switch hides the routes and keeps the files; and a byte sweep of everything the server and the apps wrote finds no secret.
//! The steps are functions named by what they prove, called by the one test in that order.
//!
//! Everything here needs the `in-process-workers` feature, and the page must have been built (`npm --prefix app run build`)
//! because the listener embeds nothing else.
#![cfg(feature = "in-process-workers")]

#[path = "support/device_socket.rs"]
mod device_socket;
#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/phase_env.rs"]
mod phase_env;
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use device_socket::{Device, refused, revoke, upgrade};
use edge::Signed;
use holzwave_core::layout::{MAIN_WINDOW, PaneId};
use holzwave_core::remote::proto::{Answer, ErrorWord, Op};
use holzwave_core::server_link::Fault;
use holzwave_core::sync::engine::promote_incoming;
use holzwave_host::sync::Transport;
use holzwave_server::device::POLL_INTERVAL;
use holzwave_server::store::Role;
use holzwave_server::worker::runtime::TestHooks;
use phase_env::{
    Desk, Home, HttpRelay, approve, blocking, code_of, derived_key, entries, find, has_line, hex, link_and_state_files, listing,
    native, pane_of, switches, until,
};
use serde_json::{Value, json};
use sweep::{Canary, Category, Counts, Sweep, files_below, unexpected};
use ws_env::{PATIENCE, Person, WsClient, WsEnv, WsOptions, focus};

/// Whether the browser's part of the phase was built: the person's worker attaches its own window's panes to its sessions.
/// Derived from that module itself, so the scenario cannot claim a browser step for a part that is not there.
const BROWSER_PART_BUILT: bool = !holzwave_server::worker::local_attach::THIS_SERVER.is_empty();

// The fixed sentences, written out here so that a change of one is seen.
const REMOTE_OFF: &str = "Remote sessions are off on this server.";
const SESSION_NOT_FOUND: &str = "That session is not on the server any more.";

/// The longest a whole run may take. The waits inside are the rig's short ones: the server's poll interval of a sign-in, the
/// two seconds of a command that prints while the switch is off, and half a second before a dropped input is looked for.
const WHOLE_RUN: Duration = Duration::from_secs(240);

/// A token-shaped text no device holds: a route that reads credentials would refuse it, a gate that hides the route never
/// looks at it.
fn stranger_token() -> String {
    format!("hwd_{}", "A".repeat(43))
}

/// What the scenario keeps for the sweep at the end.
#[derive(Default)]
struct Seen {
    /// Every error sentence and refusal body the apps, the page and the routes produced.
    errors: Vec<String>,
    /// The codes the apps showed the people.
    user_codes: Vec<String>,
}

/// A person's page on the server: the page socket of their browser.
async fn page_of(env: &WsEnv, person: &Person) -> WsClient {
    env.connect_as(&person.signed).await
}

fn page_err(answer: &Value) -> String {
    answer.get("err").and_then(Value::as_str).unwrap_or_else(|| panic!("not an error: {answer}")).to_owned()
}

async fn page_sessions(page: &mut WsClient) -> Vec<u64> {
    let rows = page.ok("server_sessions", "{}").await;
    rows.as_array().expect("a list").iter().map(|row| row["id"].as_u64().expect("an id")).collect()
}

/// The line the page is told for `pane` in `pane_connections`, once `check` holds for it.
async fn page_line(page: &mut WsClient, pane: u64, what: &str, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let lines = page.ok("pane_connections", "{}").await;
        let line = lines.as_array().and_then(|all| all.iter().find(|l| l["pane"] == json!(pane)).cloned());
        if let Some(line) = line.filter(&check) {
            return line;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The sessions a device socket is told.
async fn socket_sessions(device: &mut Device) -> Vec<u32> {
    match device.ok(Op::List).await {
        Answer::List(entries) => entries.into_iter().map(|entry| entry.id).collect(),
        other => panic!("not a list: {other:?}"),
    }
}

/// The writer words of the three views of one session: the first device, the second device and the browser.
fn writers(views: [&str; 3]) -> usize {
    views.iter().filter(|word| **word == "writing").count()
}

// ---------------------------------------------------------------------------------------------------------------------
// The steps
// ---------------------------------------------------------------------------------------------------------------------

/// A request of the probe of hidden routes: method, path, bearer token and body.
type Probe<'a> = (&'a str, &'a str, Option<&'a str>, Option<&'a [u8]>);

/// Both switches off: every route of the feature answers 404 `not_found` before it reads a credential, the device socket
/// included, and the app's sign-in is told the feature is off.
async fn with_both_switches_off_every_route_of_the_feature_is_an_unknown_path(
    env: &WsEnv,
    relay: &HttpRelay,
    probe: &Home,
    seen: &mut Seen,
) {
    let token = stranger_token();
    let start = json!({ "name": "laptop", "platform": "linux" }).to_string();
    let poll = json!({ "device_code": format!("hwg_{}", "A".repeat(43)) }).to_string();
    let code = json!({ "code": "BCDF-GHJK" }).to_string();
    let routes: Vec<Probe> = vec![
        ("POST", "/api/device/code", None, Some(start.as_bytes())),
        ("POST", "/api/device/token", None, Some(poll.as_bytes())),
        ("POST", "/api/device/lookup", None, Some(code.as_bytes())),
        ("POST", "/api/device/approve", None, Some(code.as_bytes())),
        ("POST", "/api/device/deny", None, Some(code.as_bytes())),
        ("GET", "/api/device/me", Some(&token), None),
        ("DELETE", "/api/device/me", Some(&token), None),
        ("POST", "/sync/v1/ensure/-", Some(&token), None),
        ("GET", "/sync/v1/list/files", Some(&token), None),
        ("GET", "/sync/v1/blob/meta.json", Some(&token), None),
        ("PUT", "/sync/v1/blob/files/settings.json.hwx", Some(&token), Some(b"HWX")),
    ];
    let mut bodies = Vec::new();
    for (method, path, bearer, body) in routes {
        let reply = env.plain(native(method, path, bearer, body)).await;
        assert_eq!((reply.status, code_of(&reply).as_str()), (404, "not_found"), "{method} {path} while both switches are off");
        assert!(reply.header("www-authenticate").is_none(), "{method} {path} asks for no credential");
        bodies.push(reply.body.clone());
    }
    assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]), "every hidden route answers the same bytes");
    let socket = refused(env, upgrade(Some(&token))).await;
    assert_eq!(socket.status, 404, "the device socket is hidden as well");

    // The app's own sign-in is told the feature is off, with the client's fixed sentence.
    let said = probe.sign_in_start(&relay.origin).await.expect_err("nothing to sign in to");
    assert_eq!(said, Fault::Off.sentence());
    seen.errors.push(said);
}

/// The person approves on the page the code each app shows; the apps' polls then hold a token in the keychain and the link in
/// their file, and nothing of the token is in the file.
async fn the_apps_sign_in_through_the_grant_the_person_approves_on_the_page(
    env: &WsEnv,
    relay: &HttpRelay,
    devices: &[(&Home, &Signed)],
    seen: &mut Seen,
) {
    let began = Instant::now();
    let mut shown = Vec::new();
    for (home, _) in devices {
        let view = home.sign_in_start(&relay.origin).await.expect("a sign-in starts");
        assert!(view.url.ends_with(&format!("#device={}", view.code)), "the address the app opens names the code: {}", view.url);
        assert_eq!(view.interval, POLL_INTERVAL as u64);
        shown.push(view.code.clone());
        seen.user_codes.push(view.code);
    }
    for ((_, who), code) in devices.iter().zip(&shown) {
        let reply = approve(env, who, code).await;
        assert_eq!(reply.status, 204, "the approval: {}", reply.text());
    }
    // The server's clock moves past the interval; the apps' own clocks hold a poll back until it is over.
    env.test.clock.advance(POLL_INTERVAL);
    let wait = Duration::from_secs(POLL_INTERVAL as u64) + Duration::from_millis(100);
    tokio::time::sleep(wait.saturating_sub(began.elapsed())).await;
    for (home, _) in devices {
        assert_eq!(home.sign_in_poll().await.expect("a poll").state, "approved");
        assert!(home.token().is_some_and(|t| t.starts_with("hwd_")), "the token is in the keychain");
        let file = std::fs::read_to_string(home.dir.join("server_link.json")).expect("the link file");
        assert!(file.contains(&relay.origin) && !file.contains("hwd_"), "the link holds the address and no token");
        let server = home.server.clone();
        let view = blocking(move || server.refresh()).await;
        assert_eq!((view.state, view.sync_on, view.remote_on), ("signed_in", true, true), "{view:?}");
    }
}

/// Two devices of one person, one passphrase: the second ends with the first one's settings, and a wrong passphrase is refused.
async fn two_devices_of_one_person_sync_through_the_server_and_end_with_identical_settings(
    a1: &Home,
    a2: &Home,
    setting: &Canary,
    passphrase: &Canary,
    seen: &mut Seen,
) {
    let settings = format!("{{\"font_size\":14,\"font_family\":\"{}\"}}", setting.text());
    std::fs::write(a1.dir.join("settings.json"), &settings).expect("the settings");
    let sync = a1.sync.clone();
    let words = passphrase.text().to_owned();
    let view = blocking(move || sync.use_server(&words)).await.expect("the first device sets the server up");
    assert_eq!((view.transport, view.state), (Transport::Server, "idle"), "{view:?}");
    assert!(view.files >= 1);

    let sync = a2.sync.clone();
    let wrong = blocking(move || sync.use_server("not the passphrase of this folder")).await;
    seen.errors.push(wrong.expect_err("a wrong passphrase is refused"));
    let sync = a2.sync.clone();
    let words = passphrase.text().to_owned();
    let view = blocking(move || sync.use_server(&words)).await.expect("the second device joins");
    assert_eq!((view.transport, view.state), (Transport::Server, "idle"), "{view:?}");
    // The settings of a running app are applied at its next start; that start is played here.
    promote_incoming(&a2.dir);
    assert_eq!(
        std::fs::read(a2.dir.join("settings.json")).expect("the second device has the settings"),
        settings.as_bytes(),
        "the round trip restores identical settings"
    );
}

/// A session made on the first device is watched on the second; input of the watcher is dropped; Take over moves the right
/// and both labels follow; output reaches both. Returns the session and the two panes.
async fn a_session_of_one_device_is_watched_and_taken_over_on_the_other(
    d1: &Desk,
    d2: &Desk,
    typed: &Canary,
    printed: &Canary,
) -> (u32, PaneId, PaneId) {
    let p1 = pane_of(&d1.new_session().await.expect("a new session"));
    d1.start(p1);
    d1.until_prompt(p1).await;
    let rows = d1.sessions().await.expect("the list");
    assert_eq!(rows.len(), 1);
    let session = rows[0].id;
    d1.until_writer(p1, "writing").await;
    // A canary is typed (a comment the shell echoes) and another is printed by the program only.
    let (first, second) = printed.text().split_at(12);
    d1.typed(p1, &format!("# {}\recho {first}\"\"{second}\r", typed.text()));
    d1.until_line(p1, printed.text()).await;

    // The second device lists it and opens it: the screen and scrollback arrive, and it watches.
    let listed = d2.sessions().await.expect("the second device's list");
    assert_eq!(listed.iter().map(|row| row.id).collect::<Vec<_>>(), vec![session]);
    let p2 = pane_of(&d2.open(session).await.expect("the second device opens it"));
    d2.start(p2);
    d2.until_prompt(p2).await;
    d2.until_line(p2, printed.text()).await;
    d2.until_writer(p2, "watching").await;
    assert_eq!(d1.writer(p1), "writing");

    // What the watcher types goes nowhere.
    d2.typed(p2, "echo hw-dropped-$((2*3))\r");
    tokio::time::sleep(Duration::from_millis(500)).await;
    d1.typed(p1, "echo hw-mark-$((1+1))\r");
    d1.until_line(p1, "hw-mark-2").await;
    d2.until_line(p2, "hw-mark-2").await;
    let dropped = d1.screen(p1).contains("hw-dropped") || has_line(&d2.screen(p2), "hw-dropped-6");
    assert!(!dropped, "the watcher's input was dropped");

    // Take over: the right moves, both labels follow, and the first device's input is now the one that is dropped.
    d2.takeover(p2).await.expect("take over");
    d2.until_writer(p2, "writing").await;
    d1.until_writer(p1, "watching").await;
    d2.typed(p2, "echo hw-$((7*8))\r");
    d1.until_line(p1, "hw-56").await;
    d1.typed(p1, "echo hw-ignored-$((3*3))\r");
    tokio::time::sleep(Duration::from_millis(500)).await;
    d2.typed(p2, "echo hw-after-$((4*4))\r");
    d1.until_line(p1, "hw-after-16").await;
    d2.until_line(p2, "hw-after-16").await;
    let ignored = has_line(&d1.screen(p1), "hw-ignored-9") || d2.screen(p2).contains("hw-ignored");
    assert!(!ignored, "the new watcher's input was dropped");
    (session, p1, p2)
}

/// The browser's pane of the person's worker attaches the same session; the right is shared with the devices (one writer among
/// the three), a takeover from the browser is seen by both devices, and what the browser types reaches the session. The first
/// device takes the right back at the end. Returns the browser's pane.
async fn the_browser_attaches_the_same_session_and_shares_one_writer_with_the_devices(
    page: &mut WsClient,
    session: u32,
    (d1, p1): (&Desk, PaneId),
    (d2, p2): (&Desk, PaneId),
) -> u64 {
    assert_eq!(page_sessions(page).await, vec![u64::from(session)], "the browser lists the session of the devices");
    let view = page.ok("server_session_open", &json!({ "session": session }).to_string()).await;
    let pane = focus(&view);
    page.ok("start_pane", &json!({ "pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16 }).to_string()).await;
    let line = page_line(page, pane, "the browser's attach", |l| l["state"] == "connected" && l["remote"]["phase"] == "").await;
    assert_eq!(line["name"], "This server");
    assert_eq!(line["remote"]["session"], json!(session));
    let line = page_line(page, pane, "the browser's label", |l| l["remote"]["writer"] != "").await;
    assert_eq!(line["remote"]["writer"], "watching", "the second device still writes");
    assert_eq!(writers([d1.writer(p1), d2.writer(p2), "watching"]), 1, "one writer among the three");

    // The browser takes over: both devices are told.
    page.ok("pane_takeover", &json!({ "pane": pane }).to_string()).await;
    page_line(page, pane, "the browser writes", |l| l["remote"]["writer"] == "writing").await;
    d1.until_writer(p1, "watching").await;
    d2.until_writer(p2, "watching").await;
    assert_eq!(writers([d1.writer(p1), d2.writer(p2), "writing"]), 1, "one writer among the three");
    page.ok("text", &json!({ "pane": pane, "text": "echo hw-web-$((8*8))\r" }).to_string()).await;
    d1.until_line(p1, "hw-web-64").await;
    d2.until_line(p2, "hw-web-64").await;

    // The first device takes the right back.
    d1.takeover(p1).await.expect("take over");
    d1.until_writer(p1, "writing").await;
    page_line(page, pane, "the browser watches", |l| l["remote"]["writer"] == "watching").await;
    d2.until_writer(p2, "watching").await;
    pane
}

/// A second person signs in a device of their own: every session number and every file of the first person is unknown to them
/// on the device socket, on the sync routes and in their browser; what they make is theirs and the first person sees none of it.
#[allow(clippy::too_many_arguments)]
async fn a_second_person_sees_only_their_own_sessions_and_files(
    env: &WsEnv,
    bob: &Person,
    b1: &Home,
    a_token: &str,
    session: u32,
    (d1, p1): (&Desk, PaneId),
    passphrase: &Canary,
    seen: &mut Seen,
) {
    let token = b1.token().expect("the second person's token");
    let alices_files = entries(&listing(env, a_token, "files").await);
    assert!(!alices_files.is_empty(), "the first person has files on the server");

    // The device socket.
    let mut socket = Device::connect(env, &token).await;
    assert_eq!(socket_sessions(&mut socket).await, Vec::<u32>::new(), "nothing of the first person is listed");
    for op in [
        Op::Attach { session, tail: None },
        Op::Takeover { session },
        Op::Resize { session, cols: 100, rows: 30 },
        Op::End { session },
        Op::Detach { session },
    ] {
        assert_eq!(socket.err(op).await, ErrorWord::NotFound, "{} of another person's session", op.name());
    }
    socket.type_into(session, b"echo hw-bob-$((5+5))\r").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    d1.typed(p1, "echo hw-mark-$((2+2))\r");
    d1.until_line(p1, "hw-mark-4").await;
    assert!(!d1.screen(p1).contains("hw-bob"), "another person's input never reaches the session");

    // The browser.
    let mut page = page_of(env, bob).await;
    assert_eq!(page_sessions(&mut page).await, Vec::<u64>::new());
    for command in ["server_session_open", "server_session_end"] {
        let said = page_err(&page.call(command, &json!({ "session": session }).to_string()).await);
        assert_eq!(said, SESSION_NOT_FOUND, "{command} of another person's session");
        seen.errors.push(said);
    }

    // The sync routes.
    for dir in ["-", "files", "backups"] {
        assert_eq!(entries(&listing(env, &token, dir).await), Vec::new(), "the second person's {dir} is empty");
    }
    let mut paths: Vec<String> = alices_files.iter().map(|(name, _)| format!("files/{name}")).collect();
    paths.push("meta.json".to_owned());
    for path in &paths {
        let reply = env.plain(native("GET", &format!("/sync/v1/blob/{path}"), Some(&token), None)).await;
        assert_eq!((reply.status, code_of(&reply).as_str()), (404, "not_found"), "another person's {path}");
        seen.errors.push(reply.text());
    }

    // What the second person makes is theirs.
    std::fs::write(b1.dir.join("settings.json"), br#"{"font_size":11}"#).expect("the settings");
    let sync = b1.sync.clone();
    let words = passphrase.text().to_owned();
    assert_eq!(blocking(move || sync.use_server(&words)).await.expect("the second person's sync").state, "idle");
    let theirs = entries(&listing(env, &token, "files").await);
    assert!(!theirs.is_empty());
    assert!(theirs.iter().all(|(_, tag)| alices_files.iter().all(|(_, other)| other != tag)), "no stored file is shared");
    assert_eq!(entries(&listing(env, a_token, "files").await), alices_files, "the first person's files are untouched");
    let own = socket.new_session().await;
    assert_eq!(socket_sessions(&mut socket).await, vec![own], "the second person lists their own only");
    assert_eq!(page_sessions(&mut page).await, vec![u64::from(own)]);
    let alices = d1.sessions().await.expect("the first person's list");
    assert_eq!(alices.iter().map(|row| row.id).collect::<Vec<_>>(), vec![session], "the first person lists theirs only");
    socket.ok(Op::End { session: own }).await;
    page.send_close(1000).await;
}


/// The person revokes the second device in their list: its socket closes with the revocation code within a second, its pane
/// stops, its token is forgotten, its next sync is refused as signed out; the first device goes on as before.
#[allow(clippy::too_many_arguments)]
async fn a_revocation_ends_the_revoked_device_and_nothing_else(
    env: &WsEnv,
    alice: &Person,
    (a1, d1, p1): (&Home, &Desk, PaneId),
    (a2, d2, p2): (&Home, &Desk, PaneId),
    seen: &mut Seen,
) {
    let token = a2.token().expect("the second device's token");
    let mut socket = Device::connect(env, &token).await;
    assert!(matches!(socket.ok(Op::List).await, Answer::List(_)));

    let began = Instant::now();
    revoke(env, alice, a2.device_id()).await;
    let closed = socket.close_code(Duration::from_secs(1)).await;
    assert_eq!(closed.map(|(code, _)| code), Some(4401), "the revoked device's socket closes with the revocation code");
    assert!(began.elapsed() <= Duration::from_secs(1), "within a second: {:?}", began.elapsed());
    until("the revoked device's pane stops", phase_env::SHELL, || d2.conn(p2).is_some_and(|line| line.state == "failed")).await;
    let reply = env.plain(native("GET", "/api/device/me", Some(&token), None)).await;
    assert_eq!(reply.status, 401, "the revoked token is refused");
    seen.errors.push(reply.text());
    let sync = a2.sync.clone();
    let view = blocking(move || sync.run()).await.expect("a refused token is a state");
    assert_eq!((view.state, view.error), ("signed_out", None), "the revoked device's next sync is refused as signed out");
    assert!(a2.token().is_none(), "the refused token is forgotten");

    // The first device is untouched.
    assert_eq!(d1.conn(p1).map(|line| line.state), Some("connected"));
    d1.typed(p1, "echo hw-still-$((10*10))\r");
    d1.until_line(p1, "hw-still-100").await;
    let sync = a1.sync.clone();
    assert_eq!(blocking(move || sync.run()).await.expect("the first device syncs").state, "idle");
    let server = a1.server.clone();
    assert_eq!(blocking(move || server.refresh()).await.state, "signed_in");
}

/// The remote switch turned off ends every device socket and refuses the browser's session commands, and the session runs on:
/// turned on again, the first device attaches and finds the output made meanwhile. Returns the pane that shows it now.
async fn the_remote_switch_ends_the_sockets_and_not_the_session(
    env: &WsEnv,
    admin: &Signed,
    a1: &Home,
    (d1, p1, session): (&Desk, PaneId, u32),
    page: &mut WsClient,
    seen: &mut Seen,
) -> PaneId {
    let mut socket = Device::connect(env, &a1.token().expect("the first device's token")).await;
    assert!(matches!(socket.ok(Op::List).await, Answer::List(_)));
    // The program prints after two seconds, while the switch is off.
    d1.typed(p1, "sleep 2; echo hw-meanwhile-$((9*9))\r");
    tokio::time::sleep(Duration::from_millis(200)).await;
    switches(env, admin, json!({ "remote": false })).await;
    assert_eq!(socket.close_code(Duration::from_secs(1)).await.map(|(code, _)| code), Some(1001), "the device socket ends");
    let lost = || d1.conn(p1).is_some_and(|line| line.state != "connected");
    until("the first device's pane loses its connection", phase_env::SHELL, lost).await;
    let said = page_err(&page.call("server_sessions", "{}").await);
    assert_eq!(said, REMOTE_OFF, "the browser's session commands are refused");
    seen.errors.push(said);
    let said = d1.sessions().await.expect_err("the app is told the feature is off");
    seen.errors.push(said);
    tokio::time::sleep(Duration::from_millis(2500)).await;

    switches(env, admin, json!({ "remote": true })).await;
    // The pane that was refused gave up; the person closes it and opens the session again.
    d1.call(move |core| core.close_pane_in(MAIN_WINDOW, p1)).await.expect("the pane closes");
    let pane = pane_of(&d1.open(session).await.expect("the first device opens the session again"));
    d1.start(pane);
    d1.until_line(pane, "hw-meanwhile-81").await;
    assert!(page_sessions(page).await.contains(&u64::from(session)), "the browser lists the session again");
    pane
}

/// The sync switch turned off makes the sync routes unknown, and the stored files stay: turned on again, the same files.
async fn the_sync_switch_hides_the_routes_and_keeps_the_files(env: &WsEnv, admin: &Signed, a1: &Home, seen: &mut Seen) {
    let token = a1.token().expect("the first device's token");
    let before = entries(&listing(env, &token, "files").await);
    switches(env, admin, json!({ "sync": false })).await;
    let reply = listing(env, &token, "files").await;
    assert_eq!((reply.status, code_of(&reply).as_str()), (404, "not_found"), "the sync routes are unknown while off");
    let sync = a1.sync.clone();
    match blocking(move || sync.run()).await {
        Ok(view) => {
            assert_ne!(view.state, "idle", "a sync run does not succeed while the switch is off: {view:?}");
            seen.errors.extend(view.error);
        }
        Err(said) => seen.errors.push(said),
    }
    switches(env, admin, json!({ "sync": true })).await;
    assert_eq!(entries(&listing(env, &token, "files").await), before, "the stored files stayed");
}

// ---------------------------------------------------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------------------------------------------------

/// The name of the one test, which the child process runs.
const SCENARIO: &str = "the_whole_feature_through_one_real_server_from_the_switches_to_a_sweep_that_finds_no_secret";
/// Set in the child process that runs the scenario.
const IN_CHILD: &str = "HW_PHASE_FLOW_SCENARIO";
/// The start of a line of the child's standard output that hands the parent one secret of the run, as hexadecimal, so that the
/// parent can look for it in the server's log. The line never reaches a file or the test's output.
const SECRET_LINE: &str = "hw-phase-flow-secret ";

/// The server writes its log to the standard error of its process. The scenario therefore runs in a child process of this test
/// binary, and the parent reads that log whole and sweeps it with every secret of the run. The child is bounded in time and
/// must have ended when the parent looks.
#[test]
fn the_whole_feature_through_one_real_server_from_the_switches_to_a_sweep_that_finds_no_secret() {
    if std::env::var_os(IN_CHILD).is_some() {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("a runtime");
        for canary in runtime.block_on(scenario()) {
            println!("{SECRET_LINE}{} {}", hex(canary.name.as_bytes()), hex(canary.text().as_bytes()));
        }
        return;
    }
    let began = Instant::now();
    let mut child = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([SCENARIO, "--exact", "--nocapture", "--test-threads=1"])
        .env(IN_CHILD, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the child starts");
    let read = |mut from: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = from.read_to_end(&mut bytes);
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().expect("its output")));
    let stderr = read(Box::new(child.stderr.take().expect("its log")));
    let status = loop {
        if let Some(status) = child.try_wait().expect("the child's state") {
            break status;
        }
        if began.elapsed() > WHOLE_RUN {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the scenario did not end within {WHOLE_RUN:?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (stdout, stderr) = (stdout.join().expect("the output"), stderr.join().expect("the log"));
    let log = String::from_utf8_lossy(&stderr).into_owned();
    let end = &log[log.len().saturating_sub(12_000)..];
    assert!(status.success(), "the scenario failed; the end of its standard error:\n{end}");

    // The secrets of the run, as the child handed them over.
    let unhex = |text: &str| -> String {
        let bytes: Vec<u8> = (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("hex")).collect();
        String::from_utf8(bytes).expect("text")
    };
    let canaries: Vec<Canary> = String::from_utf8_lossy(&stdout)
        .lines()
        // The test harness may have written the test's name on the same line before the first one.
        .filter_map(|line| line.find(SECRET_LINE).map(|at| &line[at + SECRET_LINE.len()..]))
        .map(|rest| {
            let (name, text) = rest.split_once(' ').expect("a name and a text");
            Canary::with_text(&unhex(name), Category::Secret, &unhex(text))
        })
        .collect();
    // Five canaries, three tokens, three grant codes, three user codes in two spellings, the key in two.
    assert_eq!(canaries.len(), 19, "the child handed over every secret");

    // The server's log is there and holds none of them, in any form, and no text of the shape of a token or a grant code.
    let lines: Vec<String> = log.lines().map(str::to_owned).collect();
    assert!(log.contains("device_socket.attached") && log.contains("code=4401"), "the server's log was captured");
    let sweep = Sweep::new(&canaries);
    let mut counts = Counts::default();
    let findings = sweep.scan_lines("the server's log", &lines, &mut counts);
    assert!(findings.is_empty(), "{:?}", findings.iter().map(sweep::Finding::line).collect::<Vec<_>>());
    assert!(!sweep.scan_text("a planted line", &format!("info x={}", canaries[0].text())).is_empty(), "the sweep is not blind");
    for prefix in ["hwd_", "hwg_"] {
        let mut at = 0;
        while let Some(start) = find(&stderr[at..], prefix.as_bytes()) {
            let from = at + start + prefix.len();
            let run = stderr[from..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'-' || **b == b'_').count();
            assert!(run < 43, "a text of the shape of a secret ({prefix}) is in the server's log");
            at = from;
        }
    }
    eprintln!("the scenario took {:?}; the server's log has {} lines", began.elapsed(), counts.lines);
}

/// The scenario itself, in the order of the module's description. Returns every secret of the run for the sweep of the log.
async fn scenario() -> Vec<Canary> {
    let began = Instant::now();
    let log = Arc::new(Mutex::new(Vec::new()));
    let hooks = TestHooks { log: Some(log.clone()), ..TestHooks::default() };
    let env = WsEnv::start_with(WsOptions { real_gate: true, hooks, ..WsOptions::default() }).await;
    let relay = HttpRelay::start(&env).await;
    env.test.create_account("root", Role::Admin);
    let admin = env.test.sign_in("root");
    let alice = env.person("alice");
    let bob = env.person("bob");
    let (a1, a2, b1) = (Home::new("a1"), Home::new("a2"), Home::new("b1"));
    let mut seen = Seen::default();

    let setting = Canary::new("a setting", Category::Content { owner: "alice".to_owned() });
    let passphrase = Canary::new("the passphrase", Category::Secret);
    let bobs_passphrase = Canary::new("the second person's passphrase", Category::Secret);
    let typed = Canary::new("typed text", Category::Secret);
    let printed = Canary::new("printed text", Category::Secret);

    // Off, then on.
    with_both_switches_off_every_route_of_the_feature_is_an_unknown_path(&env, &relay, &a1, &mut seen).await;
    let state = switches(&env, &admin, json!({ "sync": true, "remote": true })).await;
    assert_eq!(state, json!({ "sync": { "on": true, "allowed": true }, "remote": { "on": true, "allowed": true } }));

    // Sign-in and sync.
    the_apps_sign_in_through_the_grant_the_person_approves_on_the_page(
        &env,
        &relay,
        &[(&a1, &alice.signed), (&a2, &alice.signed), (&b1, &bob.signed)],
        &mut seen,
    )
    .await;
    two_devices_of_one_person_sync_through_the_server_and_end_with_identical_settings(&a1, &a2, &setting, &passphrase, &mut seen)
        .await;
    let a_token = a1.token().expect("the first device's token");
    let meta = env.plain(native("GET", "/sync/v1/blob/meta.json", Some(&a_token), None)).await;
    assert_eq!(meta.status, 200);
    let key = derived_key(passphrase.text(), &meta.body);

    // Sessions on two devices and in the browser.
    let d1 = a1.start().await;
    let d2 = a2.start().await;
    let (session, p1, p2) = a_session_of_one_device_is_watched_and_taken_over_on_the_other(&d1, &d2, &typed, &printed).await;
    let mut page = page_of(&env, &alice).await;
    if BROWSER_PART_BUILT {
        let (one, two) = ((&d1, p1), (&d2, p2));
        the_browser_attaches_the_same_session_and_shares_one_writer_with_the_devices(&mut page, session, one, two).await;
    } else {
        // Not built: the browser's step is left out, by name, and the rest of the scenario runs as it is.
        eprintln!("the browser's part is not built: the browser attaching the session is left out of this run");
    }

    // A second person, then the endings.
    let first = (&d1, p1);
    a_second_person_sees_only_their_own_sessions_and_files(&env, &bob, &b1, &a_token, session, first, &bobs_passphrase, &mut seen)
        .await;
    a_revocation_ends_the_revoked_device_and_nothing_else(&env, &alice, (&a1, &d1, p1), (&a2, &d2, p2), &mut seen).await;
    let shown = (&d1, p1, session);
    let p1 = the_remote_switch_ends_the_sockets_and_not_the_session(&env, &admin, &a1, shown, &mut page, &mut seen).await;
    assert!(d1.conn(p1).is_some(), "the first device shows the session again");
    the_sync_switch_hides_the_routes_and_keeps_the_files(&env, &admin, &a1, &mut seen).await;

    // What the run leaves: the session ends, the apps close, and nothing stays connected.
    d1.end(session).await.expect("the session ends");
    let deadline = Instant::now() + PATIENCE;
    while !d1.sessions().await.expect("the list").is_empty() {
        assert!(Instant::now() < deadline, "the session did not end");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    page.send_close(1000).await;
    drop(page);
    d1.quit().await;
    d2.quit().await;

    // The sweep.
    let secrets = a_sweep_of_everything_written_finds_no_secret(
        &env,
        &relay,
        &log,
        &[&a1, &a2, &b1],
        &seen,
        &key,
        &[&setting, &passphrase, &bobs_passphrase, &typed, &printed],
        &[alice.key.uid, bob.key.uid],
    );

    // Nothing stays behind: no socket, no socket task, no data folder of an app, and the run was bounded.
    let homes: Vec<PathBuf> = [&a1, &a2, &b1].iter().map(|home| home.dir.clone()).collect();
    drop((a1, a2, b1));
    env.eventually("no socket of the first person stays", |e| e.counts(&alice).sockets == 0).await;
    env.eventually("no socket of the second person stays", |e| e.counts(&bob).sockets == 0).await;
    for dir in &homes {
        let _ = std::fs::remove_dir_all(dir);
        assert!(!dir.exists(), "the data folder is gone");
    }
    env.shutdown.trigger();
    assert!(env.state.tracker.wait_empty(Duration::from_secs(2)).await, "every socket task has ended");
    assert!(began.elapsed() < WHOLE_RUN, "the whole run took {:?}", began.elapsed());
    eprintln!("the whole run took {:?}", began.elapsed());
    secrets
}

/// A byte sweep of the database with its log and shared-memory files, every audit file, the workers' homes, the captured log of
/// the workers, every file of the apps' folders and every error text: no token, no device code, no user code, no passphrase, no
/// derived key, no typed or printed text of the session, and the setting only in the settings file of its owner's devices. The
/// audit holds the switch changes, the approvals and the revocation.
#[allow(clippy::too_many_arguments)]
fn a_sweep_of_everything_written_finds_no_secret(
    env: &WsEnv,
    relay: &HttpRelay,
    log: &Arc<Mutex<Vec<String>>>,
    homes: &[&Home],
    seen: &Seen,
    key: &[u8; 32],
    canaries: &[&Canary],
    uids: &[u32],
) -> Vec<Canary> {
    let tokens = relay.secrets_sent("hwd_");
    let device_codes = relay.secrets_sent("hwg_");
    assert_eq!(tokens.len(), 3, "the three apps were each sent one token");
    assert_eq!(device_codes.len(), 3, "and each one device code");
    let mut all: Vec<Canary> = canaries.iter().map(|c| (*c).clone()).collect();
    for (n, token) in tokens.iter().enumerate() {
        all.push(Canary::with_text(&format!("device token {n}"), Category::Secret, token));
    }
    for (n, code) in device_codes.iter().enumerate() {
        all.push(Canary::with_text(&format!("device code {n}"), Category::Secret, code));
    }
    for (n, code) in seen.user_codes.iter().enumerate() {
        all.push(Canary::with_text(&format!("user code {n}"), Category::Secret, code));
        all.push(Canary::with_text(&format!("user code {n} without its dash"), Category::Secret, &code.replace('-', "")));
    }
    all.push(Canary::with_text("the derived key in hex", Category::Secret, &hex(key)));
    let base64 = base64::engine::general_purpose::STANDARD.encode(key);
    all.push(Canary::with_text("the derived key in base64", Category::Secret, &base64));
    let sweep = Sweep::new(&all);

    let server_dir = env.test.audit_dir.parent().expect("the server's folder").to_path_buf();
    let mut trees: Vec<PathBuf> = vec![server_dir.clone()];
    trees.extend(uids.iter().map(|uid| env.launcher.home(*uid)));
    trees.extend(homes.iter().map(|home| home.dir.clone()));
    let mut counts = Counts::default();
    let mut findings = Vec::new();
    for tree in &trees {
        findings.extend(sweep.scan_tree(tree, &mut counts));
    }
    let lines = log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    findings.extend(sweep.scan_lines("the log of the workers", &lines, &mut counts));
    findings.extend(sweep.scan_text("the error texts", &seen.errors.join("\n")));
    // The setting is the person's own plain data: it lives in the settings file of their two devices and nowhere else.
    let allowed: Vec<(String, String)> = homes[..2]
        .iter()
        .map(|home| ("a setting".to_owned(), home.dir.join("settings.json").display().to_string()))
        .collect();
    let allowed: Vec<(&str, &str)> = allowed.iter().map(|(c, s)| (c.as_str(), s.as_str())).collect();
    let findings = unexpected(findings, &allowed);
    assert!(findings.is_empty(), "{:?}", findings.iter().map(sweep::Finding::line).collect::<Vec<_>>());

    // The raw key, byte for byte, in every file read.
    for tree in &trees {
        for file in files_below(tree) {
            let bytes = std::fs::read(&file).expect("a file the sweep read");
            assert!(find(&bytes, key).is_none(), "the derived key is in {}", file.display());
        }
    }
    // The files of the link and the state of each app were among those read.
    let state_files: usize = homes.iter().map(|home| link_and_state_files(&home.dir).len()).sum();
    assert!(state_files >= 6, "the link and sync files of the apps were swept: {state_files}");
    assert!(counts.files >= 10, "the sweep read the database, the audit, the workers' homes and the apps: {counts:?}");
    // The sweep is not blind.
    assert!(!sweep.scan_text("a planted line", &format!("typed {}", tokens[0])).is_empty());

    // The audit holds the switch changes, the approvals and the revocation, and the scan above found nothing secret in it.
    let mut actions: Vec<String> = Vec::new();
    for file in files_below(&env.test.audit_dir) {
        for line in std::fs::read_to_string(&file).expect("an audit file").lines() {
            let value: Value = serde_json::from_str(line).expect("an audit line is JSON");
            actions.push(value["action"].as_str().unwrap_or_default().to_owned());
        }
    }
    let count = |name: &str| actions.iter().filter(|a| *a == name).count();
    assert_eq!(count("device_access.change"), 6, "both on (a line each), remote off and on, sync off and on: {actions:?}");
    assert_eq!(count("device.approve"), 3, "{actions:?}");
    assert_eq!(count("device.revoke"), 1, "{actions:?}");
    all
}

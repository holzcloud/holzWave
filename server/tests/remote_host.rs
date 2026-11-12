//! The host of the app against the real server: a real `AppCore` whose link points at a real listener (through the relay
//! that stands for the network), with the in-memory secret store, lists, makes, opens and ends sessions of the server, shows
//! them in panes of its layout and remembers which pane shows which session.
//!
//! What is read here is what the page would get: the views of the layout, the frames of a pane (kept as rows of text), the
//! answer of `pane_connections` and the fixed sentences of the errors. The tracer test comes first; the filter `tracer`
//! selects exactly it. Everything here needs the `in-process-workers` feature, and the page must have been built
//! (`npm --prefix app run build`) because the listener embeds nothing else.
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use device_socket::{Device, Minted, env_with, mint, revoke};
use holzwave_core::connections::ConnView;
use holzwave_core::engine::DomKey;
use holzwave_core::layout::PaneId;
use holzwave_core::layout::MAIN_WINDOW;
use holzwave_core::remote::proto::{Answer, Op};
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::server_link::{ServerLinkFile, Token, token_clear, token_load, token_save};
use holzwave_host::seams::{Host, HostLimits, NeutralHost, Platform};
use holzwave_host::server::ServerRuntime;
use holzwave_host::sessions::{ServerParts, SessionRow};
use holzwave_host::state::{AppCore, Out, ShellSpec, Sink, Target, View};
use holzwave_host::windows::{CursorSpot, Place};
use holzwave_server::supervisor::registry::SupervisorSettings;
use remote_env::{Remote, blocking, quick};
use ws_env::WsOptions;

const SHELL: Duration = Duration::from_secs(20);

/// The name of the record of the remote panes in the data directory.
const RECORD: &str = "remote_panes.json";

// The fixed sentences, written out here so that a change of one is seen.
const NOT_SIGNED_IN: &str = "Sign in to the server first.";
const SIGN_IN_ENDED: &str = "The server ended this sign-in. Sign in again.";
const REMOTE_OFF: &str = "Remote sessions are off on this server.";
const NOT_FOUND: &str = "That session is not on the server any more.";
const TOO_MANY: &str = "The server has as many sessions open as it allows.";
const ALREADY_OPEN: &str = "That session is already open on this computer.";
const NOT_A_SESSION_PANE: &str = "This pane does not show a session on a server.";

// ---------------------------------------------------------------------------------------------------------------------
// The rig: a sink that keeps what a page would show, a host with the test's secret store, and a core on a scratch folder
// ---------------------------------------------------------------------------------------------------------------------

/// What the page of a core would show: the rows of every pane, built from its frames, and the exits it was told.
#[derive(Default)]
struct Screens {
    rows: Mutex<HashMap<PaneId, Vec<String>>>,
    exits: Mutex<Vec<(PaneId, Option<u32>)>>,
}

impl Sink for Screens {
    fn send(&self, _target: Target, out: Out) {
        match out {
            Out::Frame(frame) => {
                let mut all = self.rows.lock().unwrap();
                let rows = all.entry(frame.pane).or_default();
                if frame.full {
                    rows.clear();
                }
                rows.resize(usize::from(frame.rows), String::new());
                for row in &frame.changed {
                    if let Some(slot) = rows.get_mut(usize::from(row.y)) {
                        // One code point per cell; the second half of a wide character is a NUL.
                        *slot = row.runs.iter().flat_map(|run| run.0.chars()).filter(|c| *c != '\0').collect();
                    }
                }
            }
            Out::PaneExit { pane, code } => self.exits.lock().unwrap().push((pane, code)),
            _ => {}
        }
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Ok(())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

impl Screens {
    /// The screen of `pane` as text, one line per row, the empty rows at the end left out.
    fn text(&self, pane: PaneId) -> String {
        let all = self.rows.lock().unwrap();
        let rows = all.get(&pane).cloned().unwrap_or_default();
        let used = rows.iter().rposition(|row| !row.trim().is_empty()).map_or(0, |at| at + 1);
        rows[..used].iter().map(|row| row.trim_end()).collect::<Vec<&str>>().join("\n")
    }

    fn exits_of(&self, pane: PaneId) -> Vec<Option<u32>> {
        self.exits.lock().unwrap().iter().filter(|(id, _)| *id == pane).map(|(_, code)| *code).collect()
    }
}

/// The neutral host with the secret store of the test and, when asked, a cap on the panes of the person.
struct DeskHost {
    base: NeutralHost,
    store: Arc<MemoryStore>,
    max_panes: Option<usize>,
}

impl Host for DeskHost {
    fn platform(&self) -> Platform {
        self.base.platform()
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.store.clone()
    }

    fn local_network(&self) -> Arc<dyn holzwave_host::local_network::Source> {
        self.base.local_network()
    }

    fn text_scale(&self) -> u32 {
        self.base.text_scale()
    }

    fn limits(&self) -> HostLimits {
        HostLimits { max_panes: self.max_panes, ..self.base.limits() }
    }
}

/// A data directory of one device and its secret store: what lives through a restart of the app.
struct Home {
    dir: PathBuf,
    store: Arc<MemoryStore>,
}

impl Home {
    fn new(tag: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("hw-remote-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch folder");
        Home { dir, store: Arc::new(MemoryStore::default()) }
    }

    /// The link file of a device that signed in at the relay of `remote`. The token is not part of it.
    fn link(&self, remote: &Remote, device: &Minted) {
        let link = ServerLinkFile::new_with(&remote.relay.origin, &device.id.to_string(), "laptop", true).expect("a link");
        link.save(&self.dir).expect("the link file is written");
    }

    /// The token of `device` in the store, as a sign-in leaves it.
    fn keep(&self, device: &Minted) {
        token_save(&*self.store, &Token::parse(&device.token).expect("a token")).expect("the token is kept");
    }

    /// A home of a new device of alice that is signed in.
    fn signed_in(remote: &Remote, tag: &str) -> (Home, Minted) {
        let home = Home::new(tag);
        let device = mint(&remote.env, &remote.alice, tag);
        home.link(remote, &device);
        home.keep(&device);
        (home, device)
    }

    /// The record of the remote panes as the file holds it.
    fn record(&self) -> serde_json::Value {
        let bytes = std::fs::read(self.dir.join(RECORD)).expect("the record exists");
        serde_json::from_slice(&bytes).expect("the record is JSON")
    }

    /// The app starts on this home. The core is built off the async threads of the test.
    async fn start(&self, max_panes: Option<usize>) -> Desk {
        let (dir, store) = (self.dir.clone(), self.store.clone());
        let screens = Arc::new(Screens::default());
        let sink = screens.clone();
        let core = blocking(move || {
            let runtime = ServerRuntime::new_for_loopback(dir.clone(), store.clone()).with_hub_options(quick());
            let host = Arc::new(DeskHost { base: NeutralHost::new(), store, max_panes });
            let parts = ServerParts { runtime: Some(Arc::new(runtime)), sessions: None };
            AppCore::new_with_server(dir, sink, ShellSpec::new("/bin/sh", &[]), None, None, host, parts)
        })
        .await;
        Desk { core, screens }
    }

    fn remove(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A running app: its core and what its page would show.
struct Desk {
    core: Arc<AppCore>,
    screens: Arc<Screens>,
}

impl Desk {
    /// A call of the core that may block on the network.
    async fn call<T: Send + 'static>(&self, call: impl FnOnce(&AppCore) -> T + Send + 'static) -> T {
        let core = self.core.clone();
        blocking(move || call(&core)).await
    }

    async fn sessions(&self) -> Result<Vec<SessionRow>, String> {
        self.call(|core| core.server_sessions()).await
    }

    async fn new_session(&self) -> Result<View, String> {
        self.call(|core| core.server_session_new_in(MAIN_WINDOW)).await
    }

    async fn open(&self, session: u32) -> Result<View, String> {
        self.call(move |core| core.server_session_open_in(MAIN_WINDOW, session)).await
    }

    async fn end(&self, session: u32) -> Result<(), String> {
        self.call(move |core| core.server_session_end(session)).await
    }

    fn view(&self) -> View {
        self.core.view_in(MAIN_WINDOW).expect("the main window")
    }

    /// How many tabs the window has.
    fn tabs(&self) -> usize {
        self.view().tabs.len()
    }

    /// The page starts the pane, as it does when it sees it in a view.
    fn start(&self, pane: PaneId) {
        self.core.start_pane(pane, 80, 24, 8, 16).expect("the pane starts");
    }

    fn typed(&self, pane: PaneId, text: &str) {
        self.core.text(pane, text.to_owned()).expect("typed");
    }

    fn enter(&self, pane: PaneId) {
        let key = DomKey { code: "Enter".into(), key: "Enter".into(), ..DomKey::default() };
        self.core.key(pane, key).expect("the key");
    }

    fn screen(&self, pane: PaneId) -> String {
        self.screens.text(pane)
    }

    /// The line of `pane` in the answer of `pane_connections`.
    fn conn(&self, pane: PaneId) -> Option<ConnView> {
        self.core.pane_connections().into_iter().find(|view| view.pane == pane)
    }

    /// The word of who writes in `pane`, empty when the pane has no remote view.
    fn writer(&self, pane: PaneId) -> &'static str {
        self.conn(pane).and_then(|view| view.remote).map_or("", |remote| remote.writer)
    }

    async fn until_screen(&self, pane: PaneId, what: &str, check: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + SHELL;
        loop {
            let text = self.screen(pane);
            if check(&text) {
                return;
            }
            assert!(Instant::now() < deadline, "{what} did not happen in time; the screen: {text:?}; the line: {:?}", self.conn(pane));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the pane is attached and shows the prompt of the shell.
    async fn until_prompt(&self, pane: PaneId) {
        until("the attach", SHELL, || {
            self.conn(pane).is_some_and(|v| v.state == "connected" && v.remote.is_some_and(|r| r.phase.is_empty()))
        })
        .await;
        self.until_screen(pane, "a prompt", |text| {
            let last = text.trim_end();
            last.ends_with('$') || last.ends_with('#') || last.ends_with('%')
        })
        .await;
    }

    /// A line of the screen that is exactly `line`: the output of a command, never the echo of what was typed.
    async fn until_line(&self, pane: PaneId, line: &str) {
        self.until_screen(pane, line, |text| has_line(text, line)).await;
    }

    /// The app ends: the layout is written and every pane is closed. The home stays.
    async fn quit(self) {
        blocking(move || {
            self.core.shutdown();
            drop(self);
        })
        .await;
    }
}

fn has_line(text: &str, line: &str) -> bool {
    text.lines().any(|l| l.trim() == line)
}

async fn until(what: &str, within: Duration, check: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !check() {
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The one pane a view of a new tab shows.
fn pane_of(view: &View) -> PaneId {
    assert_eq!(view.panes.len(), 1, "a new tab has one pane");
    assert_eq!(view.panes[0].0, view.focus, "and it has the focus");
    view.focus
}

/// The sessions a test device of the rig is told.
async fn listed(device: &mut Device) -> Vec<u32> {
    match device.ok(Op::List).await {
        Answer::List(entries) => entries.into_iter().map(|entry| entry.id).collect(),
        other => panic!("not a list: {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The tracer
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn tracer_a_signed_in_app_lists_makes_and_shows_a_session_and_what_is_typed_reaches_its_program() {
    let remote = Remote::start().await;
    let (home, _device) = Home::signed_in(&remote, "tracer");
    let desk = home.start(None).await;

    // Nothing yet.
    assert_eq!(desk.sessions().await, Ok(Vec::new()));
    let before = desk.view();

    // A new session: its tab and its pane are in the layout, and in the record, before the page starts anything.
    let view = desk.new_session().await.expect("a new session");
    let pane = pane_of(&view);
    assert_eq!(view.tabs.len(), before.tabs.len() + 1, "one more tab");
    assert!(before.panes.iter().all(|(id, _)| *id != pane), "the pane is new");
    assert_eq!(desk.view().focus, pane, "the layout of the core holds it");
    let rows = desk.sessions().await.expect("the list");
    assert_eq!(rows.len(), 1);
    let session = rows[0].id;
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));
    assert_eq!((rows[0].cols, rows[0].rows), (80, 24), "the size it was made with");
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(rows[0].started <= now + 5 && rows[0].started + 300 >= now, "started now: {} against {now}", rows[0].started);
    assert!(rows[0].held, "the device that made it holds the right to write");
    assert!(desk.conn(pane).is_none(), "a pane that was not started has no line");

    // The page starts the pane: the prompt of the shell on the server is in its frames.
    desk.start(pane);
    desk.until_prompt(pane).await;
    desk.typed(pane, "echo hw-$((6*7))\r");
    desk.until_line(pane, "hw-42").await;

    // What the header, the sidebar and the status line are told.
    let line = desk.conn(pane).expect("the pane has a line");
    assert_eq!(
        serde_json::to_value(&line).unwrap(),
        serde_json::json!({
            "pane": pane, "profile": "", "name": "127.0.0.1", "state": "connected", "attempt": 0, "of": 0, "secs": 0,
            "remote": { "writer": "writing", "session": session, "cols": 80, "rows": 24, "more": false, "phase": "" }
        })
    );
    assert_eq!(desk.core.session_counts(), (0, 1), "one connection and no shell of this computer");
    // The list says what the pane changed: one client is attached.
    let rows = desk.sessions().await.expect("the list");
    assert_eq!((rows.len(), rows[0].id, rows[0].attached, rows[0].held), (1, session, 1, true));
    // A second pane on the same session is refused on this device, and no tab is made for it.
    let tabs = desk.tabs();
    assert_eq!(desk.open(session).await.err().as_deref(), Some(ALREADY_OPEN));
    assert_eq!(desk.tabs(), tabs);
    // A pane that shows no session has no right to take and no older lines.
    let local = before.focus;
    assert_eq!(desk.core.pane_takeover(local).err().as_deref(), Some(NOT_A_SESSION_PANE));
    assert_eq!(desk.core.pane_load_older(local).err().as_deref(), Some(NOT_A_SESSION_PANE));
    assert_eq!(desk.core.pane_load_older(pane), Ok(()), "nothing older: nothing happens");

    desk.quit().await;
    home.remove();
}

// ---------------------------------------------------------------------------------------------------------------------
// Two devices of one person
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_second_device_watches_takes_over_and_the_first_is_told_then_a_close_detaches_and_an_end_ends() {
    let remote = Remote::start().await;
    let (home_a, _a) = Home::signed_in(&remote, "two-laptop");
    let (home_b, _b) = Home::signed_in(&remote, "two-phone");
    let laptop = home_a.start(None).await;
    let phone = home_b.start(None).await;

    let first = pane_of(&laptop.new_session().await.expect("a session"));
    laptop.start(first);
    laptop.until_prompt(first).await;
    let session = laptop.sessions().await.unwrap()[0].id;

    // The other device of the same person sees the session and opens it: it watches.
    let rows = phone.sessions().await.expect("the list of the phone");
    assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<u32>>(), [session]);
    let second = pane_of(&phone.open(session).await.expect("the session opens"));
    phone.start(second);
    phone.until_prompt(second).await;
    until("the phone watches", SHELL, || phone.writer(second) == "watching").await;
    assert_eq!(laptop.writer(first), "writing");
    let rows = laptop.sessions().await.unwrap();
    assert_eq!((rows[0].attached, rows[0].held), (2, true), "two clients, one of them writes");

    // What a watcher types reaches nothing.
    phone.typed(second, "echo nope-$((1+1))\r");
    laptop.typed(first, "echo one-$((40+1))\r");
    laptop.until_line(first, "one-41").await;
    phone.until_line(second, "one-41").await;
    assert!(!laptop.screen(first).contains("nope") && !phone.screen(second).contains("nope"), "the watcher's text was dropped");

    // The phone takes over: it writes and the laptop watches, within a second, and each device's own answer says so.
    let asked = Instant::now();
    assert_eq!(phone.core.pane_takeover(second), Ok(()));
    until("the takeover", Duration::from_secs(1), || phone.writer(second) == "writing" && laptop.writer(first) == "watching").await;
    assert!(asked.elapsed() < Duration::from_secs(1));
    phone.typed(second, "echo two-$((40+2))\r");
    phone.until_line(second, "two-42").await;
    laptop.until_line(first, "two-42").await;
    laptop.typed(first, "echo late-$((1+1))\r");
    phone.typed(second, "echo three-$((40+3))\r");
    phone.until_line(second, "three-43").await;
    assert!(!phone.screen(second).contains("late"), "the laptop only watches now");

    // Closing the pane detaches: the session stays in the list, the record forgets the pane, the phone goes on.
    let tabs = laptop.tabs();
    laptop.core.close_pane_in(MAIN_WINDOW, first).expect("the pane closes");
    assert_eq!(laptop.tabs(), tabs - 1);
    assert_eq!(home_a.record(), serde_json::json!({ "version": 1, "panes": {} }));
    until("the detach", SHELL, || laptop.core.pane_connections().is_empty()).await;
    let deadline = Instant::now() + SHELL;
    loop {
        let rows = laptop.sessions().await.expect("the list");
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<u32>>(), [session], "a closed pane ends nothing");
        if rows[0].attached == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the laptop stayed attached: {rows:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    phone.typed(second, "echo four-$((40+4))\r");
    phone.until_line(second, "four-44").await;

    // A session of another person, and a number nobody has: one sentence, and nothing changes.
    let bob = remote.env.person("bob");
    let bobs = mint(&remote.env, &bob, "bobs-laptop");
    let mut other = Device::connect(&remote.env, &bobs.token).await;
    let foreign = other.new_session().await;
    let unknown = session.max(foreign) + 4000;
    for id in [unknown, foreign].into_iter().filter(|id| *id != session) {
        let tabs = laptop.tabs();
        assert_eq!(laptop.end(id).await.err().as_deref(), Some(NOT_FOUND), "ending {id}");
        assert_eq!(laptop.open(id).await.err().as_deref(), Some(NOT_FOUND), "opening {id}");
        assert_eq!(laptop.tabs(), tabs, "no tab for a session that is not this person's");
    }
    assert_eq!(listed(&mut other).await, [foreign], "the other person's session is untouched");
    assert_eq!(laptop.sessions().await.unwrap().iter().map(|row| row.id).collect::<Vec<u32>>(), [session]);
    assert_eq!(home_a.record(), serde_json::json!({ "version": 1, "panes": {} }));

    // The laptop ends the session: the pane of the phone is told, says so and stays until it is closed.
    assert_eq!(laptop.end(session).await, Ok(()));
    phone.until_screen(second, "the end of the session", |text| text.contains("Session ended")).await;
    let said = phone.screen(second);
    let ending = said.lines().find(|line| line.contains("Session ended")).unwrap_or_default().trim().to_owned();
    assert!(
        ending == "Session ended" || ending.strip_prefix("Session ended · exit ").is_some_and(|code| code.parse::<u32>().is_ok()),
        "the line of the end: {ending:?}"
    );
    assert_eq!(phone.view().focus, second, "the pane stays on the screen");
    assert_eq!(home_b.record(), serde_json::json!({ "version": 1, "panes": { second.to_string(): session } }));
    assert_eq!(laptop.sessions().await, Ok(Vec::new()));
    assert_eq!(phone.sessions().await, Ok(Vec::new()));
    assert_eq!(laptop.end(session).await.err().as_deref(), Some(NOT_FOUND), "ended once");

    // A session that ends by itself says its status.
    let third = pane_of(&laptop.new_session().await.expect("another session"));
    laptop.start(third);
    laptop.until_prompt(third).await;
    laptop.typed(third, "exit 7\r");
    laptop.until_screen(third, "the status", |text| has_line(text, "Session ended · exit 7")).await;

    // Enter closes the pane of an ended session: the tab goes and the record forgets it.
    let tabs = phone.tabs();
    phone.enter(second);
    until("the pane of the ended session goes", SHELL, || phone.tabs() == tabs - 1).await;
    // The layout changes first and the exit is told right after it.
    until("the page is told", SHELL, || !phone.screens.exits_of(second).is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(phone.screens.exits_of(second).len(), 1, "the page is told once");
    assert_eq!(home_b.record(), serde_json::json!({ "version": 1, "panes": {} }));

    laptop.quit().await;
    phone.quit().await;
    home_a.remove();
    home_b.remove();
}

// ---------------------------------------------------------------------------------------------------------------------
// A restart of the app
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn after_a_restart_the_pane_shows_its_session_again_and_a_session_that_is_gone_never_becomes_a_local_shell() {
    let remote = Remote::start().await;
    let (home, _device) = Home::signed_in(&remote, "restart");
    let desk = home.start(None).await;
    let pane = pane_of(&desk.new_session().await.expect("a session"));
    desk.start(pane);
    desk.until_prompt(pane).await;
    desk.typed(pane, "echo before-$((10+1))\r");
    desk.until_line(pane, "before-11").await;
    let session = desk.sessions().await.unwrap()[0].id;
    let tabs = desk.tabs();
    desk.quit().await;

    // The record holds the pane and its session and nothing else: no name of a server, no title, no directory, no token.
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));

    // While the app is not running the session goes on: another device of the person writes into it.
    let phone = mint(&remote.env, &remote.alice, "restart-phone");
    let mut device = Device::connect(&remote.env, &phone.token).await;
    assert_eq!(listed(&mut device).await, [session], "closing the app ended nothing");
    device.attach(session, None).await;
    device.ok(Op::Takeover { session }).await;
    device.type_into(session, b"echo while-away-$((20+3))\r").await;
    device.until_live(session, "while-away-23", SHELL).await;
    drop(device);

    // The app starts again on the same folder and store: the layout has the pane, and starting it attaches to the session.
    let desk = home.start(None).await;
    assert_eq!(desk.tabs(), tabs, "the layout came back");
    assert!(desk.core.pane_connections().is_empty(), "nothing is attached before the page starts the pane");
    desk.start(pane);
    desk.until_prompt(pane).await;
    let shown = desk.screen(pane);
    assert!(has_line(&shown, "before-11") && has_line(&shown, "while-away-23"), "the screen of the session as it is now: {shown:?}");
    assert_eq!(desk.conn(pane).and_then(|line| line.remote).map(|r| r.session), Some(session));
    assert_eq!(desk.core.session_counts(), (0, 1));
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));

    // A second session in a second pane; then the app closes and the session ends on the server.
    let gone_pane = pane_of(&desk.new_session().await.expect("a second session"));
    let gone = desk.sessions().await.unwrap().iter().map(|row| row.id).find(|id| *id != session).expect("the second session");
    // Closing the first pane takes it out of the record; the second stays.
    desk.core.close_pane_in(MAIN_WINDOW, pane).expect("the pane closes");
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { gone_pane.to_string(): gone } }));
    desk.quit().await;
    let mut device = Device::connect(&remote.env, &phone.token).await;
    device.ok(Op::End { session: gone }).await;
    // The program is told to end; the session leaves the list when it has.
    let deadline = Instant::now() + SHELL;
    while listed(&mut device).await != [session] {
        assert!(Instant::now() < deadline, "the ended session stayed in the list");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(device);

    // The pane is still a pane of a session: it says the one sentence, runs nothing here, and stays in the record.
    let desk = home.start(None).await;
    desk.start(gone_pane);
    desk.until_screen(gone_pane, "the sentence", |text| has_line(text, NOT_FOUND)).await;
    until("the pane has ended", SHELL, || desk.conn(gone_pane).is_some_and(|line| line.state == "closed")).await;
    let line = desk.conn(gone_pane).expect("still a line of a session pane");
    assert_eq!((line.name.as_str(), line.remote.map(|r| r.session)), ("127.0.0.1", Some(gone)));
    assert_eq!(desk.core.session_counts(), (0, 0), "no shell was started in its place");
    assert!(desk.view().panes.iter().any(|(id, _)| *id == gone_pane) || desk.core.view_in(MAIN_WINDOW).is_ok());
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { gone_pane.to_string(): gone } }));
    let shown = desk.screen(gone_pane);
    assert!(!shown.contains('$') || shown.contains(NOT_FOUND), "nothing but the sentence and the notice: {shown:?}");
    desk.typed(gone_pane, "echo local-$((2+2))\r");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!desk.screen(gone_pane).contains("local-4"), "what is typed reaches no program");

    desk.quit().await;
    home.remove();
}

// ---------------------------------------------------------------------------------------------------------------------
// Signed out, and the switch of the server off
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn without_a_sign_in_or_with_the_switch_off_every_request_says_one_sentence_and_a_pane_waits_for_enter() {
    let remote = Remote::start().await;

    // No server at all, and a server but no token: nothing is asked of anybody.
    let unlinked = Home::new("signed-out-unlinked");
    let (linked, device) = Home::signed_in(&remote, "signed-out-linked");
    token_clear(&*linked.store).expect("the token is cleared");
    for home in [&unlinked, &linked] {
        let carried = remote.relay.accepted();
        let desk = home.start(None).await;
        let tabs = desk.tabs();
        assert_eq!(desk.sessions().await.err().as_deref(), Some(NOT_SIGNED_IN));
        assert_eq!(desk.new_session().await.err().as_deref(), Some(NOT_SIGNED_IN));
        assert_eq!(desk.open(1).await.err().as_deref(), Some(NOT_SIGNED_IN));
        assert_eq!(desk.end(1).await.err().as_deref(), Some(NOT_SIGNED_IN));
        assert_eq!(desk.tabs(), tabs, "no tab was made");
        assert!(!home.dir.join(RECORD).exists() || home.record() == serde_json::json!({ "version": 1, "panes": {} }));
        assert_eq!(remote.relay.accepted(), carried, "no connection was made");
        desk.quit().await;
    }
    unlinked.remove();

    // Signed in, and the server has its remote sessions off.
    linked.keep(&device);
    let desk = linked.start(None).await;
    device_socket::remote(&remote.env, false);
    let tabs = desk.tabs();
    assert_eq!(desk.sessions().await.err().as_deref(), Some(REMOTE_OFF));
    assert_eq!(desk.new_session().await.err().as_deref(), Some(REMOTE_OFF));
    assert_eq!(desk.open(1).await.err().as_deref(), Some(REMOTE_OFF));
    assert_eq!(desk.tabs(), tabs, "no tab was made");
    assert!(token_load(&*linked.store).unwrap().is_some(), "a switch that is off does not end the sign-in");
    device_socket::remote(&remote.env, true);

    // On again: a session in a pane, and the app closes.
    let pane = pane_of(&desk.new_session().await.expect("a session"));
    desk.start(pane);
    desk.until_prompt(pane).await;
    let session = desk.sessions().await.unwrap()[0].id;
    desk.quit().await;

    // The token is gone when the app starts again: the pane says so, is not a shell, and waits.
    token_clear(&*linked.store).expect("the token is cleared");
    let desk = linked.start(None).await;
    desk.start(pane);
    desk.until_screen(pane, "the signed-out sentence", |text| has_line(text, SIGN_IN_ENDED)).await;
    until("the failed state", SHELL, || desk.conn(pane).is_some_and(|line| line.state == "failed")).await;
    assert_eq!(desk.conn(pane).and_then(|line| line.remote).map(|r| r.session), Some(session));
    assert_eq!(desk.core.session_counts(), (0, 0), "nothing was started on this computer");
    desk.typed(pane, "echo local-$((2+2))");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!desk.screen(pane).contains("local"), "what is typed goes nowhere");
    assert!(desk.screens.exits_of(pane).is_empty(), "the pane stays");
    // Enter without a sign-in asks again and says the same.
    desk.enter(pane);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(desk.conn(pane).map(|line| line.state), Some("failed"));
    assert_eq!(linked.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));

    // The person signs in again; Enter attaches the pane to its session.
    linked.keep(&device);
    desk.enter(pane);
    desk.until_prompt(pane).await;
    // A new connection has no right to write until the person takes it.
    until("the view of the writer", SHELL, || !desk.writer(pane).is_empty()).await;
    if desk.writer(pane) != "writing" {
        assert_eq!(desk.core.pane_takeover(pane), Ok(()));
        until("the takeover", SHELL, || desk.writer(pane) == "writing").await;
    }
    desk.typed(pane, "echo back-$((50+5))\r");
    desk.until_line(pane, "back-55").await;

    // The server takes the device away: the next request says the person is signed out, and the token is forgotten.
    revoke(&remote.env, &remote.alice, device.id).await;
    until("the pane hears of it", SHELL, || desk.conn(pane).is_some_and(|line| line.state == "failed")).await;
    let said = desk.sessions().await.err();
    assert!(
        matches!(said.as_deref(), Some(NOT_SIGNED_IN | SIGN_IN_ENDED)),
        "a refused token reads as signed out: {said:?}"
    );
    until("the token is forgotten", SHELL, || token_load(&*linked.store).is_ok_and(|kept| kept.is_none())).await;
    assert_eq!(desk.sessions().await.err().as_deref(), Some(NOT_SIGNED_IN));
    assert_eq!(desk.core.session_counts(), (0, 0));

    desk.quit().await;
    linked.remove();
}

// ---------------------------------------------------------------------------------------------------------------------
// The caps
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_cap_of_the_person_and_the_cap_of_the_server_are_fixed_sentences_and_leave_no_tab_behind() {
    // The cap of the host: the layout starts with one pane, so one more is allowed.
    let remote = Remote::start().await;
    let (home, _device) = Home::signed_in(&remote, "cap-host");
    let desk = home.start(Some(2)).await;
    let own_cap = "this server allows 2 panes per person; close one to open another";
    let pane = pane_of(&desk.new_session().await.expect("the one pane the cap leaves"));
    let session = desk.sessions().await.unwrap()[0].id;
    let tabs = desk.tabs();
    assert_eq!(desk.new_session().await.err().as_deref(), Some(own_cap));
    assert_eq!(desk.tabs(), tabs, "no tab was made");
    assert_eq!(desk.sessions().await.unwrap().len(), 1, "and no session: the cap is asked before the server is");
    // The same for a session that exists: another device made one.
    let phone = mint(&remote.env, &remote.alice, "cap-phone");
    let mut device = Device::connect(&remote.env, &phone.token).await;
    let theirs = device.new_session().await;
    assert_eq!(desk.open(theirs).await.err().as_deref(), Some(own_cap));
    assert_eq!(desk.tabs(), tabs, "no tab was made");
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));
    // With room again it opens.
    desk.core.close_pane_in(MAIN_WINDOW, pane).expect("the pane closes");
    let opened = pane_of(&desk.open(theirs).await.expect("room for it now"));
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { opened.to_string(): theirs } }));
    drop(device);
    desk.quit().await;
    home.remove();
    drop(remote);

    // The cap of the server: two panes per person, and the worker's own window holds the first, so one session fits.
    let supervisor = SupervisorSettings { max_panes: 2, ..SupervisorSettings::default() };
    let remote = Remote::from(env_with(WsOptions { supervisor: Some(supervisor), ..WsOptions::default() }).await).await;
    let (home, _device) = Home::signed_in(&remote, "cap-server");
    let desk = home.start(None).await;
    let pane = pane_of(&desk.new_session().await.expect("the one session the server allows"));
    let session = desk.sessions().await.unwrap()[0].id;
    let tabs = desk.tabs();
    assert_eq!(desk.new_session().await.err().as_deref(), Some(TOO_MANY));
    assert_eq!(desk.tabs(), tabs, "no tab is left behind");
    assert_eq!(desk.view().focus, pane, "the view is what it was");
    assert_eq!(home.record(), serde_json::json!({ "version": 1, "panes": { pane.to_string(): session } }));
    assert_eq!(desk.sessions().await.unwrap().iter().map(|row| row.id).collect::<Vec<u32>>(), [session]);
    // The pane that exists is not harmed.
    desk.start(pane);
    desk.until_prompt(pane).await;
    desk.quit().await;
    home.remove();
}

// ---------------------------------------------------------------------------------------------------------------------
// What is written, and what the sync copies
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_record_is_not_synced_and_no_file_of_the_app_holds_the_token_or_anything_the_session_said() {
    // The sync's own list: the record is a fact of this device (pane ids are small counters per device).
    assert!(!holzwave_core::sync::engine::allowed(RECORD), "the sync does not copy the record");
    assert!(holzwave_core::sync::engine::allowed("layout.json"), "the list is not empty");
    assert_eq!(holzwave_host::sessions::FILE, RECORD);

    let remote = Remote::start().await;
    let (home, device) = Home::signed_in(&remote, "files");
    let desk = home.start(None).await;
    let pane = pane_of(&desk.new_session().await.expect("a session"));
    desk.start(pane);
    desk.until_prompt(pane).await;

    // The program names its window and moves to a directory; both names are split in what is typed, so the whole text exists
    // only as what the program said and did. The directory is a real one (the server reads it from the process).
    let title = "hw-canary-title-7f3a91c2";
    let folder = "hw-canary-folder-5be208d4";
    let printed = "hw-canary-printed-c41d77e0";
    let parent = std::env::temp_dir().join(format!("hw-remote-host-cwd-{}", std::process::id()));
    std::fs::create_dir_all(parent.join(folder)).expect("the folder of the program");
    let halves = |text: &str| (text[..12].to_owned(), text[12..].to_owned());
    let (t1, t2) = halves(title);
    let (f1, f2) = halves(folder);
    let (p1, p2) = halves(printed);
    desk.typed(pane, &format!("printf '\\033]0;%s%s\\007' {t1} {t2}; cd '{}/{f1}''{f2}'\r", parent.display()));
    desk.typed(pane, &format!("echo {p1}\"\"{p2}\r"));
    desk.until_line(pane, printed).await;
    // The list of the server carries the title: it is shown, and it is not kept.
    let deadline = Instant::now() + SHELL;
    loop {
        let rows = desk.sessions().await.expect("the list");
        if rows[0].title.contains(title) && rows[0].cwd.contains(folder) {
            break;
        }
        assert!(Instant::now() < deadline, "the list never showed the title and the directory: {rows:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    desk.quit().await;
    let _ = std::fs::remove_dir_all(&parent);

    // Every file the app wrote, in every form a text can be written in.
    let files = sweep::files_below(&home.dir);
    let names: Vec<String> = files.iter().filter_map(|path| path.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
    for expected in [RECORD, "layout.json", "server_link.json"] {
        assert!(names.iter().any(|name| name == expected), "{expected} was written: {names:?}");
    }
    let canaries = [("the token", device.token.as_str()), ("the title", title), ("the directory", folder), ("the output", printed)];
    let mut hits = Vec::new();
    for path in &files {
        let bytes = std::fs::read(path).expect("a file of the app is readable");
        for (what, text) in canaries {
            for (form, needle) in sweep::forms_of(text) {
                if bytes.windows(needle.len()).any(|window| window == needle.as_slice()) {
                    hits.push(format!("{what} as {form} in {}", file_name(path)));
                }
            }
        }
    }
    assert!(hits.is_empty(), "{hits:?}");
    // The scan is not blind: the address of the server is in the link file, and the session's number in the record.
    let link = std::fs::read_to_string(home.dir.join("server_link.json")).expect("the link file");
    assert!(link.contains("127.0.0.1"));
    assert_eq!(home.record()["panes"][pane.to_string()].as_u64().is_some(), true);
    home.remove();
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

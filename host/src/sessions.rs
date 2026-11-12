//! The sessions of a holzWave server as the host sees them: a seam that lists, makes, ends and opens them, the desktop's
//! implementation over the one connection of this device, and the per-device record of which pane shows which session.
//!
//! The seam ([`SessionService`]) names no socket and no token, so a build whose sessions live somewhere else (the server's
//! own page) supplies another implementation. Every failure is one of the fixed sentences of [`SessionFault`]: nothing a
//! server wrote is ever passed on as a sentence.
//!
//! The record ([`RemotePanes`], the file `remote_panes.json`) holds pane ids and session numbers and nothing else: no
//! address, no token, no title. It belongs to this device and is never synced, like `server_link.json`.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use holzwave_core::conn::{ConnState, Link};
use holzwave_core::layout::PaneId;
use holzwave_core::pane::{PaneSink, RemoteAttach, RemoteTarget};
use holzwave_core::persist::{read_limited, write_atomic};
use holzwave_core::remote::hub::HubError;
use holzwave_core::remote::link::notice;
use holzwave_core::remote::proto::{ErrorWord, SessionEntry};
use holzwave_core::server_link::Fault;
use serde::{Deserialize, Serialize};

use crate::locked::SECRETS_LOCKED;
use crate::server::{KEYCHAIN, NOT_SIGNED_IN, ServerRuntime, Unavailable};

/// The per-device file of the panes that show a session.
pub const FILE: &str = "remote_panes.json";
/// The version of the file's format.
pub const VERSION: u32 = 1;
/// The most bytes the file may have: larger is not read at all.
pub const MAX_BYTES: u64 = 64 * 1024;
/// The most entries the record keeps.
pub const MAX_ENTRIES: usize = 1024;
/// The most characters of a title or a directory that a row passes on.
pub const MAX_TEXT: usize = 256;
/// The grid a session is made with before the pane that shows it has told its size. The pane that made the session writes,
/// so the session takes the size of the pane's tile as soon as the pane starts.
pub const NEW_SESSION_GRID: (u16, u16) = (80, 24);

/// Said for a pane that does not show a session of a server (or does not run yet).
pub const NOT_A_SESSION_PANE: &str = "This pane does not show a session on a server.";
/// Said when the window cannot take another pane.
pub const NO_ROOM: &str = "There is no room for another pane.";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One session of the person on the server, as the list shows it. Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    /// The number of the session on the server.
    pub id: u32,
    /// The program's own title, bounded and without control characters; empty when it set none.
    pub title: String,
    /// The program's working directory as its shell reported it, bounded and without control characters.
    pub cwd: String,
    /// The grid in cells.
    pub cols: u16,
    pub rows: u16,
    /// The start, in seconds since the Unix epoch.
    pub started: u64,
    /// How many clients are attached.
    pub attached: u16,
    /// Whether a client holds the right to write.
    pub held: bool,
}

/// A text of the server for a row: control characters go and the length is bounded, whatever the server sent.
fn row_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_TEXT).collect()
}

impl From<SessionEntry> for SessionRow {
    fn from(entry: SessionEntry) -> Self {
        SessionRow {
            id: entry.id,
            title: row_text(&entry.title),
            cwd: row_text(&entry.cwd),
            cols: entry.cols,
            rows: entry.rows,
            started: entry.started,
            attached: entry.attached,
            held: entry.writer_held,
        }
    }
}

/// Why something about a session could not be done. A closed set with one fixed sentence each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionFault {
    /// This device holds no sign-in, or the server ended it.
    SignedOut,
    /// The store of the secrets is locked, so the sign-in cannot be read.
    Locked,
    /// The keychain cannot be read.
    Keychain,
    /// The server has remote sessions switched off.
    Off,
    /// The server cannot be reached, or did not answer in time.
    Unreachable,
    /// The session does not exist for this person.
    NotFound,
    /// The server has as many sessions (or attachments) as it allows.
    TooMany,
    /// The server said no.
    Refused,
    /// The server sent something this client does not accept.
    Protocol,
    /// The session is already shown in a pane of this device.
    AlreadyOpen,
}

impl SessionFault {
    /// Every case, in the order of the declaration.
    pub const ALL: [SessionFault; 10] = [
        SessionFault::SignedOut,
        SessionFault::Locked,
        SessionFault::Keychain,
        SessionFault::Off,
        SessionFault::Unreachable,
        SessionFault::NotFound,
        SessionFault::TooMany,
        SessionFault::Refused,
        SessionFault::Protocol,
        SessionFault::AlreadyOpen,
    ];

    /// The one fixed sentence of the fault.
    pub fn sentence(self) -> &'static str {
        match self {
            SessionFault::SignedOut => NOT_SIGNED_IN,
            SessionFault::Locked => SECRETS_LOCKED,
            SessionFault::Keychain => KEYCHAIN,
            SessionFault::Off => "Remote sessions are off on this server.",
            SessionFault::Unreachable => "The server cannot be reached.",
            SessionFault::NotFound => "That session is not on the server any more.",
            SessionFault::TooMany => "The server has as many sessions open as it allows.",
            SessionFault::Refused => "The server refused the request.",
            SessionFault::Protocol => "The server sent something holzWave does not accept.",
            SessionFault::AlreadyOpen => "That session is already open on this computer.",
        }
    }
}

impl std::fmt::Display for SessionFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.sentence())
    }
}

impl From<SessionFault> for String {
    fn from(fault: SessionFault) -> String {
        fault.sentence().to_owned()
    }
}

impl From<Unavailable> for SessionFault {
    fn from(why: Unavailable) -> Self {
        match why {
            Unavailable::SignedOut => SessionFault::SignedOut,
            Unavailable::Locked => SessionFault::Locked,
            Unavailable::Keychain => SessionFault::Keychain,
        }
    }
}

impl From<HubError> for SessionFault {
    fn from(error: HubError) -> Self {
        match error {
            HubError::Fault(Fault::SignedOut) => SessionFault::SignedOut,
            HubError::Fault(Fault::Off) | HubError::Word(ErrorWord::Disabled) => SessionFault::Off,
            HubError::Fault(Fault::Unreachable) | HubError::Timeout | HubError::Disconnected | HubError::Closed => SessionFault::Unreachable,
            HubError::Word(ErrorWord::NotFound) => SessionFault::NotFound,
            HubError::Word(ErrorWord::TooMany) => SessionFault::TooMany,
            HubError::Fault(Fault::Refused) | HubError::Word(ErrorWord::Refused | ErrorWord::Busy) | HubError::Busy | HubError::Slow => {
                SessionFault::Refused
            }
            HubError::Fault(Fault::Protocol) | HubError::Word(ErrorWord::BadRequest) => SessionFault::Protocol,
            HubError::AlreadyAttached => SessionFault::AlreadyOpen,
        }
    }
}

/// What the host asks about the sessions of a server. The desktop answers over its connection to the server it is signed in
/// to ([`ServerSessions`]); another build answers from wherever its sessions live. Every call may wait for a network.
pub trait SessionService: Send + Sync {
    /// The name of the server for the row of a pane: its host and nothing more. Empty when no server is known.
    fn name(&self) -> String;

    /// The sessions of the person, oldest first.
    fn list(&self) -> Result<Vec<SessionRow>, SessionFault>;

    /// Makes a session of `cols` by `rows` cells and answers its number. The caller holds its right to write.
    fn create(&self, cols: u16, rows: u16) -> Result<u32, SessionFault>;

    /// Ends the program of a session.
    fn end(&self, session: u32) -> Result<(), SessionFault>;

    /// What a pane needs to show `session`. Nothing is connected by this call.
    fn target(&self, session: u32) -> Result<RemoteTarget, SessionFault>;
}

/// The link to the server and the source of its sessions, for a caller that builds the core with its own. A part that is
/// `None` is built as the desktop builds it: the runtime of the data directory, and the sessions of that runtime.
#[derive(Default)]
pub struct ServerParts {
    pub runtime: Option<Arc<ServerRuntime>>,
    pub sessions: Option<Arc<dyn SessionService>>,
}

/// The desktop's sessions: the server this device is signed in to, through the one hub of the link.
pub struct ServerSessions {
    runtime: Arc<ServerRuntime>,
}

impl ServerSessions {
    pub fn new(runtime: Arc<ServerRuntime>) -> ServerSessions {
        ServerSessions { runtime }
    }

    /// The hub, when this device holds a sign-in that can be read. Said before any request is made.
    fn ready(&self) -> Result<holzwave_core::remote::hub::Hub, SessionFault> {
        let (token, _) = self.runtime.token_and_origin()?;
        drop(token);
        Ok(self.runtime.hub())
    }

    /// The fault of a failed call. A token the server refused is forgotten here as everywhere: the link is asked once who
    /// this device is, which forgets a refused token, so the row of the server says `not signed in` afterwards.
    fn fault(&self, error: HubError) -> SessionFault {
        let fault = SessionFault::from(error);
        if fault == SessionFault::SignedOut {
            let _ = self.runtime.refresh();
        }
        fault
    }
}

impl SessionService for ServerSessions {
    fn name(&self) -> String {
        self.runtime.host_name()
    }

    fn list(&self) -> Result<Vec<SessionRow>, SessionFault> {
        let hub = self.ready()?;
        let mut rows: Vec<SessionRow> = hub.list().map_err(|e| self.fault(e))?.into_iter().map(SessionRow::from).collect();
        rows.sort_by_key(|row| (row.started, row.id));
        Ok(rows)
    }

    fn create(&self, cols: u16, rows: u16) -> Result<u32, SessionFault> {
        let hub = self.ready()?;
        hub.new_session(cols, rows).map(|(session, _)| session).map_err(|e| self.fault(e))
    }

    fn end(&self, session: u32) -> Result<(), SessionFault> {
        let hub = self.ready()?;
        hub.end(session).map_err(|e| self.fault(e))
    }

    /// Always a target: a pane whose sign-in is gone or whose server cannot be reached shows that state itself and tries
    /// again on Enter, with the token the keychain holds then.
    fn target(&self, session: u32) -> Result<RemoteTarget, SessionFault> {
        Ok(RemoteTarget { session, tail: None, attach: Arc::new(self.runtime.hub()) })
    }
}

/// The target of a pane whose session cannot be reached at all: the pane shows `fault`'s sentence in the failed state and
/// runs nothing. It is what a pane of a session becomes instead of a local shell.
pub fn failed_target(session: u32, fault: SessionFault, host: String) -> RemoteTarget {
    RemoteTarget { session, tail: None, attach: Arc::new(FailedAttach { fault, host }) }
}

struct FailedAttach {
    fault: SessionFault,
    host: String,
}

impl RemoteAttach for FailedAttach {
    fn link(&self, _: u32, _: Option<u32>, _: (u16, u16), _: PaneSink) -> holzwave_core::Result<Box<dyn Link>> {
        let (end, ended) = channel();
        Ok(Box::new(FailedLink { reader: Some(Box::new(Said { text: notice(self.fault.sentence()), at: 0, ended })), end: Some(end) }))
    }

    fn host(&self) -> String {
        self.host.clone()
    }
}

/// A link that says one sentence and then is silent until the pane is closed. Nothing is started and what is typed is dropped.
struct FailedLink {
    reader: Option<Box<dyn Read + Send>>,
    /// Held so the reader blocks; dropping it (on close) ends the output.
    end: Option<Sender<()>>,
}

struct Said {
    text: Vec<u8>,
    at: usize,
    ended: Receiver<()>,
}

impl Read for Said {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at < self.text.len() {
            let n = buf.len().min(self.text.len() - self.at);
            buf[..n].copy_from_slice(&self.text[self.at..self.at + n]);
            self.at += n;
            return Ok(n);
        }
        // Blocks until the sender is dropped, which is the end of the output.
        let _ = self.ended.recv();
        Ok(0)
    }
}

struct Dropped;

impl Write for Dropped {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Link for FailedLink {
    fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        Some(Box::new(Dropped))
    }

    fn process_id(&self) -> Option<u32> {
        None
    }

    fn resize_px(&self, _: u16, _: u16, _: u32, _: u32) -> holzwave_core::Result<()> {
        Ok(())
    }

    fn try_wait(&mut self) -> holzwave_core::Result<Option<u32>> {
        Ok(None)
    }

    fn close(mut self: Box<Self>) -> holzwave_core::Result<u32> {
        self.end.take();
        Ok(0)
    }

    fn state(&self) -> ConnState {
        ConnState::Failed
    }
}

/// The file as it is written: the version and the panes, keyed by the pane id as text (JSON has no number keys).
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    panes: BTreeMap<String, u32>,
}

/// Reads the text of the file strictly: the version, no unknown member, pane ids that are numbers, a bounded count. Anything
/// else is `None`.
fn parse(text: &str) -> Option<BTreeMap<PaneId, u32>> {
    let stored: Stored = serde_json::from_str(text).ok()?;
    if stored.version != VERSION || stored.panes.len() > MAX_ENTRIES {
        return None;
    }
    let mut map = BTreeMap::new();
    for (pane, session) in stored.panes {
        // The id as this program writes it: digits only, no sign, no leading zero.
        let id: PaneId = pane.parse().ok()?;
        if id.to_string() != pane {
            return None;
        }
        map.insert(id, session);
    }
    Some(map)
}

/// Which pane of this device shows which session. Kept in `remote_panes.json` in the data directory; a file that cannot be
/// read, or that breaks a rule, reads as empty.
pub struct RemotePanes {
    path: PathBuf,
    map: Mutex<BTreeMap<PaneId, u32>>,
}

impl RemotePanes {
    /// The record of the device whose data directory is `dir`. Entries for panes that are not in `live` (the panes of the
    /// layout) are dropped, and the file is written again when that changed it.
    pub fn load(dir: &Path, live: &[PaneId]) -> RemotePanes {
        let path = dir.join(FILE);
        let read = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => String::from_utf8(bytes).ok().and_then(|text| parse(&text)),
            Ok(None) => Some(BTreeMap::new()),
            Err(_) => None,
        };
        let unreadable = read.is_none();
        let mut map = read.unwrap_or_default();
        let before = map.len();
        map.retain(|pane, _| live.contains(pane));
        let panes = RemotePanes { path, map: Mutex::new(map) };
        if unreadable || lock(&panes.map).len() != before {
            panes.save();
        }
        panes
    }

    /// The session `pane` shows, when it shows one.
    pub fn session_of(&self, pane: PaneId) -> Option<u32> {
        lock(&self.map).get(&pane).copied()
    }

    /// The pane of this device that shows `session`, when one does.
    pub fn pane_of(&self, session: u32) -> Option<PaneId> {
        lock(&self.map).iter().find(|(_, s)| **s == session).map(|(pane, _)| *pane)
    }

    /// Every pane that shows a session, with its session.
    pub fn all(&self) -> Vec<(PaneId, u32)> {
        lock(&self.map).iter().map(|(pane, session)| (*pane, *session)).collect()
    }

    /// `pane` shows `session` from now on. `false` when the record is full; nothing is written then.
    pub fn assign(&self, pane: PaneId, session: u32) -> bool {
        {
            let mut map = lock(&self.map);
            if map.len() >= MAX_ENTRIES && !map.contains_key(&pane) {
                return false;
            }
            map.insert(pane, session);
        }
        self.save();
        true
    }

    /// `pane` is gone, or shows no session any more.
    pub fn forget(&self, pane: PaneId) {
        let had = lock(&self.map).remove(&pane).is_some();
        if had {
            self.save();
        }
    }

    fn save(&self) {
        let stored = Stored { version: VERSION, panes: lock(&self.map).iter().map(|(pane, session)| (pane.to_string(), *session)).collect() };
        let text = serde_json::to_string_pretty(&stored).unwrap_or_else(|_| "{}".into());
        let dir = self.path.parent().unwrap_or(Path::new("."));
        if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| write_atomic(&self.path, text.as_bytes())) {
            eprintln!("holzwave: cannot save {FILE}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("holzwave-sessions-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_fault_has_one_sentence_of_its_own_that_ends_with_a_full_stop_and_holds_no_placeholder() {
        let sentences: Vec<&str> = SessionFault::ALL.iter().map(|fault| fault.sentence()).collect();
        for (at, sentence) in sentences.iter().enumerate() {
            // The locked store says the one word every part of the host says for it, which the page knows as such.
            let shared = SessionFault::ALL[at] == SessionFault::Locked;
            assert!((shared || sentence.ends_with('.')) && !sentence.contains('{') && sentence.len() > 10, "{sentence}");
            assert!(!sentences[..at].contains(sentence), "{sentence} is said for two faults");
            assert_eq!(String::from(SessionFault::ALL[at]), *sentence);
            assert_eq!(SessionFault::ALL[at].to_string(), *sentence);
        }
        assert_eq!(SessionFault::Off.sentence(), "Remote sessions are off on this server.", "the sentence of the design record");
        assert_eq!(SessionFault::Locked.sentence(), SECRETS_LOCKED);
    }

    #[test]
    fn every_error_of_the_hub_becomes_a_fault_and_the_words_of_the_server_are_never_passed_on() {
        use SessionFault as F;
        let cases = [
            (HubError::Fault(Fault::SignedOut), F::SignedOut),
            (HubError::Fault(Fault::Off), F::Off),
            (HubError::Word(ErrorWord::Disabled), F::Off),
            (HubError::Fault(Fault::Unreachable), F::Unreachable),
            (HubError::Timeout, F::Unreachable),
            (HubError::Disconnected, F::Unreachable),
            (HubError::Closed, F::Unreachable),
            (HubError::Word(ErrorWord::NotFound), F::NotFound),
            (HubError::Word(ErrorWord::TooMany), F::TooMany),
            (HubError::Fault(Fault::Refused), F::Refused),
            (HubError::Word(ErrorWord::Refused), F::Refused),
            (HubError::Word(ErrorWord::Busy), F::Refused),
            (HubError::Busy, F::Refused),
            (HubError::Slow, F::Refused),
            (HubError::Fault(Fault::Protocol), F::Protocol),
            (HubError::Word(ErrorWord::BadRequest), F::Protocol),
            (HubError::AlreadyAttached, F::AlreadyOpen),
        ];
        for (error, fault) in cases {
            assert_eq!(SessionFault::from(error), fault, "{error:?}");
        }
        assert_eq!(SessionFault::from(Unavailable::Locked), F::Locked);
        assert_eq!(SessionFault::from(Unavailable::Keychain), F::Keychain);
        assert_eq!(SessionFault::from(Unavailable::SignedOut), F::SignedOut);
    }

    #[test]
    fn a_row_takes_the_numbers_of_the_server_and_cleans_and_bounds_its_texts() {
        let entry = SessionEntry {
            id: 7,
            title: format!("vim\u{1b}[31m\n{}", "x".repeat(600)),
            cwd: "/home/\u{7}alice".into(),
            cols: 120,
            rows: 40,
            started: 1_700_000_000,
            attached: 2,
            writer_held: true,
        };
        let row = SessionRow::from(entry);
        assert_eq!((row.id, row.cols, row.rows, row.started, row.attached, row.held), (7, 120, 40, 1_700_000_000, 2, true));
        assert_eq!(row.cwd, "/home/alice");
        assert!(row.title.starts_with("vim[31mx") && row.title.chars().count() == MAX_TEXT, "{}", row.title.len());
        let json = serde_json::to_value(&row).unwrap();
        let mut members: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
        members.sort_unstable();
        assert_eq!(members, ["attached", "cols", "cwd", "held", "id", "rows", "started", "title"]);
    }

    #[test]
    fn the_record_keeps_a_pane_and_its_session_across_a_load_and_writes_ids_and_nothing_else() {
        let dir = Scratch::new("keeps");
        let panes = RemotePanes::load(&dir.0, &[3, 9]);
        assert!(panes.all().is_empty() && !dir.0.join(FILE).exists(), "nothing is written for an empty start");
        assert!(panes.assign(3, 41));
        assert!(panes.assign(9, 42));
        assert_eq!((panes.session_of(3), panes.session_of(9), panes.session_of(4)), (Some(41), Some(42), None));
        assert_eq!((panes.pane_of(42), panes.pane_of(1)), (Some(9), None));
        let text = fs::read_to_string(dir.0.join(FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value, serde_json::json!({ "version": 1, "panes": { "3": 41, "9": 42 } }));

        let again = RemotePanes::load(&dir.0, &[3, 9]);
        assert_eq!(again.all(), vec![(3, 41), (9, 42)]);
        again.forget(3);
        again.forget(3);
        assert_eq!(RemotePanes::load(&dir.0, &[3, 9]).all(), vec![(9, 42)]);
    }

    #[test]
    fn entries_for_panes_that_are_not_in_the_layout_are_dropped_at_load_and_the_file_says_so() {
        let dir = Scratch::new("drops");
        fs::write(dir.0.join(FILE), r#"{"version":1,"panes":{"3":41,"9":42}}"#).unwrap();
        let panes = RemotePanes::load(&dir.0, &[9]);
        assert_eq!(panes.all(), vec![(9, 42)]);
        let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.0.join(FILE)).unwrap()).unwrap();
        assert_eq!(value, serde_json::json!({ "version": 1, "panes": { "9": 42 } }));
    }

    #[test]
    fn a_file_that_breaks_a_rule_reads_as_empty_and_is_replaced() {
        let big = format!(r#"{{"version":1,"panes":{{{}}}}}"#, (0..MAX_ENTRIES + 1).map(|n| format!("\"{n}\":1")).collect::<Vec<_>>().join(","));
        let huge = " ".repeat(MAX_BYTES as usize + 1);
        let cases: [&str; 11] = [
            "",
            "not json",
            r#"{"version":2,"panes":{"3":41}}"#,
            r#"{"version":1,"panes":{"3":41},"url":"https://example.org"}"#,
            r#"{"version":1,"panes":{"3":{"session":41,"token":"x"}}}"#,
            r#"{"version":1,"panes":{"three":41}}"#,
            r#"{"version":1,"panes":{"03":41}}"#,
            r#"{"version":1,"panes":{"-3":41}}"#,
            r#"{"version":1,"panes":{"3":4294967296}}"#,
            &big,
            &huge,
        ];
        for (at, case) in cases.iter().enumerate() {
            let dir = Scratch::new(&format!("rule-{at}"));
            fs::write(dir.0.join(FILE), case).unwrap();
            let panes = RemotePanes::load(&dir.0, &[3]);
            assert!(panes.all().is_empty(), "case {at}");
            let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.0.join(FILE)).unwrap()).unwrap();
            assert_eq!(value, serde_json::json!({ "version": 1, "panes": {} }), "case {at}: the unusable file is replaced");
        }
    }

    #[test]
    fn the_record_is_bounded() {
        let dir = Scratch::new("bounded");
        let live: Vec<PaneId> = (0..MAX_ENTRIES as u64 + 1).collect();
        let panes = RemotePanes::load(&dir.0, &live);
        {
            let mut map = lock(&panes.map);
            for pane in 0..MAX_ENTRIES as u64 {
                map.insert(pane, 1);
            }
        }
        assert!(!panes.assign(MAX_ENTRIES as u64, 2), "a full record takes no new pane");
        assert!(panes.assign(0, 2), "a pane it holds can be given another session");
    }

    #[test]
    fn a_failed_target_says_its_sentence_in_the_failed_state_and_runs_nothing() {
        let target = failed_target(5, SessionFault::Off, "example.org".into());
        assert_eq!(format!("{target:?}"), r#"RemoteTarget { session: 5, host: "example.org" }"#);
        let (end, ended) = channel();
        let mut link: Box<dyn Link> =
            Box::new(FailedLink { reader: Some(Box::new(Said { text: notice(SessionFault::Off.sentence()), at: 0, ended })), end: Some(end) });
        assert_eq!(link.state(), ConnState::Failed);
        assert_eq!((link.process_id(), link.try_wait().unwrap()), (None, None));
        let mut reader = link.take_reader().unwrap();
        let mut writer = link.take_writer().unwrap();
        assert_eq!(writer.write(b"ls\r").unwrap(), 3, "typed text is dropped");
        let mut said = vec![0u8; 256];
        let n = reader.read(&mut said).unwrap();
        assert!(String::from_utf8_lossy(&said[..n]).contains("Remote sessions are off on this server."));
        let waiting = std::thread::spawn(move || reader.read(&mut [0u8; 8]).unwrap());
        assert_eq!(link.close().unwrap(), 0);
        assert_eq!(waiting.join().unwrap(), 0, "the output ends when the pane is closed");
    }
}

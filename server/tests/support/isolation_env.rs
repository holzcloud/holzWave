//! The isolation environment: the real supervisor over the real spawn helper, real workers and real shells, and a way to look
//! at each person's home only through that person's own worker.
//!
//! Everything here runs inside the container that `scripts/web-container-test.sh isolation` starts: user 10001, no capability,
//! the helper at `HW_HELPER` carrying the file capabilities, the server binary at `HW_WORKER` (the program the helper starts
//! as each person) and a writable data volume at `HW_DATA`. The test process is the supervisor. It makes people through the
//! accounts service (the first two get the first two user ids of the range), attaches sockets to their workers and talks to
//! them as a browser would, so a probe result travels the real path: socket, command, worker, person's user id.
//!
//! The test process never reads a person's home. The modes of the directories that hold the homes would forbid it, and the
//! only path into a home is the person's own worker: [`World::shell`] types a command into the person's pane, waits for the
//! result file the command wrote below the person's home and reads it back with `file_open` as that person.
//!
//! Tests pull this file in with `#[path = "support/isolation_env.rs"] mod isolation_env;` next to `proc.rs`.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use holzwave_server::accounts::{Accounts, NewAccount};
use holzwave_server::audit::Audit;
use holzwave_server::clock::{Clock, SystemClock};
use holzwave_server::store::{Account, Role, Store, StoreError, Tx};
use holzwave_server::supervisor::{
    AccountKey, HelperClient, HelperLauncher, HelperSettings, Outbound, SocketHandle, SocketRx, Supervisor, SupervisorSettings,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::proc::{self, Expect, Status};

/// The range of user ids the people get (the helper's default and the accounts service's range).
pub const RANGE: (u32, u32) = (20_000, 29_999);

/// How long a probe waits for its result file.
pub const RESULT_WAIT: Duration = Duration::from_secs(15);

/// How long one command waits for its answer.
const REPLY_WAIT: Duration = Duration::from_secs(30);

/// The process limit of each person in the test configuration.
pub const NPROC: u64 = 64;

/// The open-file limit of each person in the test configuration.
pub const NOFILE: u64 = 256;

/// The value of an environment variable the script sets.
pub fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: run these tests through scripts/web-container-test.sh isolation"))
}

/// Random text for a canary: built at run time, typed only into commands and compared by search. Never written outside a
/// person's own home.
pub fn canary(tag: &str) -> String {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random).expect("the operating system gives random bytes");
    let random: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("canary-{tag}-{random}")
}

/// The texts the operating system uses for a refusal, in one place. The comparison ignores case. `Permission denied` is
/// EACCES, `Operation not permitted` is EPERM; the other two are the error names themselves.
const PERMISSION_TEXTS: [&str; 4] = ["Permission denied", "Operation not permitted", "EACCES", "EPERM"];

/// Whether the text is the operating system's permission error.
pub fn is_permission_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    PERMISSION_TEXTS.iter().any(|word| lower.contains(&word.to_lowercase()))
}

/// Whether the text says the thing does not exist. A refusal that comes out like this is a failed probe.
pub fn is_missing(text: &str) -> bool {
    text.to_lowercase().contains("no such file")
}

// ---- sockets ----------------------------------------------------------------------------------------------------------

/// What a command answered.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Ok(Value),
    Err(Value),
}

impl Reply {
    pub fn is_ok(&self) -> bool {
        matches!(self, Reply::Ok(_))
    }

    /// The answer as text: a string as it is, anything else as JSON.
    pub fn text(&self) -> String {
        match self {
            Reply::Ok(value) | Reply::Err(value) => value.as_str().map_or_else(|| value.to_string(), str::to_owned),
        }
    }
}

/// Every message every socket of a person received, as text.
pub type Recorder = Arc<Mutex<Vec<String>>>;

fn record(recorder: &Recorder, message: &Outbound) {
    let text = match message {
        Outbound::Text(text) => text.clone(),
        Outbound::Binary(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Outbound::Close { code, reason } => format!("close {code} {reason}"),
    };
    recorder.lock().unwrap_or_else(PoisonError::into_inner).push(text);
}

/// One attached socket. A task reads the socket as it fills and keeps a copy of every message in the person's recorder, so
/// that a search of the recorder covers everything the socket ever received, whether or not a test looked at it.
pub struct Sock {
    handle: SocketHandle,
    inbox: mpsc::UnboundedReceiver<Outbound>,
    backlog: VecDeque<Outbound>,
    next: u64,
    pump: tokio::task::JoinHandle<()>,
}

impl Drop for Sock {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

impl Sock {
    fn new(handle: SocketHandle, mut rx: SocketRx, recorder: Recorder) -> Sock {
        let (tx, inbox) = mpsc::unbounded_channel();
        let pump = tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                record(&recorder, &message);
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Sock { handle, inbox, backlog: VecDeque::new(), next: 1, pump }
    }

    /// Submits a command and returns its request number.
    pub fn send(&mut self, command: &str, args: &Value) -> u64 {
        let request = self.next;
        self.next += 1;
        self.handle.submit(request, command, &args.to_string()).unwrap_or_else(|e| panic!("submit {command}: {e:?}"));
        request
    }

    async fn take(&mut self, wanted: impl Fn(&Outbound) -> bool, wait: Duration) -> Option<Outbound> {
        if let Some(at) = self.backlog.iter().position(&wanted) {
            return self.backlog.remove(at);
        }
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.inbox.recv()).await {
                Ok(Some(message)) if wanted(&message) => return Some(message),
                Ok(Some(message)) => self.backlog.push_back(message),
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// The answer to request `request`.
    pub async fn reply(&mut self, request: u64) -> Reply {
        let found = self
            .take(
                |m| matches!(m, Outbound::Text(t) if serde_json::from_str::<Value>(t).ok().and_then(|v| v.get("id")?.as_u64()) == Some(request)),
                REPLY_WAIT,
            )
            .await
            .unwrap_or_else(|| panic!("no answer to request {request} within {REPLY_WAIT:?}"));
        let Outbound::Text(text) = found else { unreachable!() };
        let value: Value = serde_json::from_str(&text).expect("an answer is JSON");
        match (value.get("ok"), value.get("err")) {
            (Some(ok), None) => Reply::Ok(ok.clone()),
            (None, Some(err)) => Reply::Err(err.clone()),
            _ => panic!("an answer has exactly one of ok and err"),
        }
    }

    /// Sends a command and waits for its answer.
    pub async fn call(&mut self, command: &str, args: &Value) -> Reply {
        let request = self.send(command, args);
        self.reply(request).await
    }

    /// The next event with this name whose payload `wanted` accepts, as `{"event","payload"}`.
    pub async fn event_where(&mut self, name: &str, wait: Duration, wanted: impl Fn(&Value) -> bool) -> Option<Value> {
        let found = self
            .take(
                |m| match m {
                    Outbound::Text(t) => serde_json::from_str::<Value>(t)
                        .ok()
                        .is_some_and(|v| v.get("event").and_then(Value::as_str) == Some(name) && wanted(&v["payload"])),
                    _ => false,
                },
                wait,
            )
            .await?;
        let Outbound::Text(text) = found else { unreachable!() };
        serde_json::from_str(&text).ok()
    }
}

// ---- people -----------------------------------------------------------------------------------------------------------

/// What a person's command did, for the table of attempts.
#[derive(Clone, Debug)]
pub struct Attempt {
    /// The command succeeded (a worker command answered `ok`, a shell command exited 0).
    pub ok: bool,
    /// The exit status of a shell command.
    pub status: Option<i32>,
    /// The answer or the output.
    pub text: String,
}

/// How an attempt is made.
#[derive(Clone, Debug)]
pub enum How<'a> {
    /// `file_open` of a path, through the person's worker.
    Open(&'a str),
    /// `file_stat` of a path.
    Stat(&'a str),
    /// `files_list` of a path.
    List(&'a str),
    /// `file_save` of text into a path.
    Save(&'a str, &'a str),
    /// A command typed into the person's shell.
    Shell(&'a str),
}

/// The output and exit status of a command typed into a person's shell.
#[derive(Clone, Debug)]
pub struct Out {
    pub status: i32,
    pub text: String,
}

/// One person: an account, the user id the helper gives their worker, a socket and a pane or more.
pub struct Person {
    pub name: String,
    pub account: Account,
    pub key: AccountKey,
    pub uid: u32,
    pub home: PathBuf,
    recorder: Recorder,
    sock: tokio::sync::Mutex<Sock>,
    shell_lock: tokio::sync::Mutex<()>,
    panes: Mutex<Vec<u64>>,
    seq: AtomicU64,
}

impl Person {
    /// Asks the person's worker through the person's first socket.
    pub async fn ask(&self, command: &str, args: Value) -> Reply {
        self.sock.lock().await.call(command, &args).await
    }

    /// `file_open` as this person: the text of the file, or the error text.
    pub async fn open(&self, path: &str) -> Result<Value, String> {
        match self.ask("file_open", json!({"source": "local", "path": path})).await {
            Reply::Ok(value) => Ok(value),
            Reply::Err(value) => Err(value.as_str().map_or_else(|| value.to_string(), str::to_owned)),
        }
    }

    /// The text of a file, read as this person; a missing text member (an empty file) is the empty string.
    pub async fn read(&self, path: &str) -> Result<String, String> {
        self.open(path).await.map(|opened| opened["text"].as_str().unwrap_or("").to_owned())
    }

    /// `file_stat` as this person.
    pub async fn stat(&self, path: &str) -> Result<Value, String> {
        match self.ask("file_stat", json!({"source": "local", "path": path})).await {
            Reply::Ok(value) => Ok(value),
            Reply::Err(value) => Err(value.as_str().map_or_else(|| value.to_string(), str::to_owned)),
        }
    }

    /// The panes this person has started, in the order they were started.
    pub fn panes(&self) -> Vec<u64> {
        self.panes.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every message every socket of this person received so far, after a short settle for the messages still in flight.
    pub async fn received(&self) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.recorder.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Whether any message any socket of this person received holds `needle`.
    pub async fn saw(&self, needle: &str) -> bool {
        self.received().await.iter().any(|text| text.contains(needle))
    }
}

// ---- the world --------------------------------------------------------------------------------------------------------

/// What a test may change about the supervisor and the helper.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// `RLIMIT_NPROC` of each person.
    pub nproc: u64,
    /// `RLIMIT_NOFILE` of each person.
    pub nofile: u64,
    /// How long a worker lives with no socket attached.
    pub idle: Duration,
}

impl Default for Options {
    fn default() -> Options {
        Options { nproc: NPROC, nofile: NOFILE, idle: Duration::from_secs(24 * 3600) }
    }
}

fn ok_enrol(_: &Tx<'_>, _: &Account) -> Result<(), StoreError> {
    Ok(())
}

/// The supervisor, the helper, the accounts and the data directory of one test.
pub struct World {
    /// `<HW_DATA>/<run>`: mode 0711, so that a person's worker can reach its home below it.
    pub data: PathBuf,
    pub supervisor: Supervisor,
    pub accounts: Arc<Accounts>,
    pub store: Arc<Store>,
    pub options: Options,
    admin: Mutex<Option<Account>>,
    sweeper: tokio::task::JoinHandle<()>,
    people: Mutex<Vec<Arc<Person>>>,
}

fn private_dir(path: &Path, mode: u32) {
    std::fs::DirBuilder::new().mode(mode).create(path).unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("the mode of the directory");
}

impl World {
    /// The supervisor with the helper launcher (never the in-process one) over a fresh data directory.
    pub async fn start() -> World {
        World::start_with(Options::default()).await
    }

    pub async fn start_with(options: Options) -> World {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        assert!(!cfg!(feature = "in-process-workers"), "the isolation tests run without the in-process workers");
        let run = format!("iso-{}-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed), &canary("run")[11..]);
        let data = PathBuf::from(var("HW_DATA")).join(run);
        // The data directory is traversable by others (mode 0711): a worker reaches its own home below it. The server
        // directory the store makes inside is 0700 and the directory of the homes is made here with the mode the image gives it.
        private_dir(&data, 0o711);
        private_dir(&data.join("homes"), 0o711);

        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let store = Arc::new(Store::open(&data).expect("the store opens"));
        let audit = Arc::new(Audit::open(&data, clock.clone()).expect("the audit log opens"));
        let settings = HelperSettings {
            program: PathBuf::from(var("HW_HELPER")),
            worker_path: PathBuf::from(var("HW_WORKER")),
            home_base: data.join("homes"),
            uid_range: Some(RANGE),
            nproc: Some(options.nproc),
            nofile: Some(options.nofile),
            ..HelperSettings::default()
        };
        let client = HelperClient::start(&settings).await.unwrap_or_else(|e| panic!("the spawn helper does not start: {e}"));
        let launcher = Arc::new(HelperLauncher::new(client));
        let supervisor_settings = SupervisorSettings {
            idle_timeout: options.idle,
            sweep_interval: Duration::from_millis(500),
            ..SupervisorSettings::default()
        };
        let supervisor = Supervisor::new(launcher, supervisor_settings, clock.clone());
        let accounts = Arc::new(
            Accounts::new(store.clone(), clock, audit, RANGE.0..=RANGE.1).with_observer(Arc::new(supervisor.clone())),
        );
        let sweeper = supervisor.start_sweeper();
        World { data, supervisor, accounts, store, options, admin: Mutex::new(None), sweeper, people: Mutex::new(Vec::new()) }
    }

    /// The directory that holds the homes.
    pub fn homes(&self) -> PathBuf {
        self.data.join("homes")
    }

    /// The server directory (database, audit files).
    pub fn server_dir(&self) -> PathBuf {
        self.data.join("server")
    }

    /// Makes an account through the accounts service and starts the person's worker with a first socket. The first person
    /// is the administrator (the bootstrap token), every other one is a member invited by that administrator. Accounts
    /// get the user ids of the range in order.
    pub async fn person(&self, name: &str) -> Arc<Person> {
        let account = {
            let mut admin = self.admin.lock().unwrap_or_else(PoisonError::into_inner);
            let new = NewAccount { username: name, display_name: name, handle: None };
            match admin.as_ref() {
                None => {
                    let issued = self.accounts.bootstrap_issue().expect("a bootstrap token");
                    let first = self.accounts.redeem_bootstrap(issued.token.expose(), &new, None, ok_enrol).expect("the first account");
                    *admin = Some(first.clone());
                    first
                }
                Some(admin) => {
                    let issued = self.accounts.invite_create(admin.id, None, None, Role::Member).expect("an invitation");
                    self.accounts.redeem_invite(issued.token.expose(), &new, None, ok_enrol).expect("a member account")
                }
            }
        };
        let key = AccountKey { id: account.id, uid: account.uid };
        let recorder: Recorder = Arc::new(Mutex::new(Vec::new()));
        let (handle, rx) = self.supervisor.attach(key).await.unwrap_or_else(|e| panic!("attach {name}: {e:?}"));
        let person = Arc::new(Person {
            name: name.to_owned(),
            uid: account.uid,
            home: self.homes().join(account.uid.to_string()),
            account,
            key,
            recorder: recorder.clone(),
            sock: tokio::sync::Mutex::new(Sock::new(handle, rx, recorder)),
            shell_lock: tokio::sync::Mutex::new(()),
            panes: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
        });
        // The first command makes sure the worker is up and answering.
        let layout = person.ask("get_layout", json!({})).await;
        assert!(layout.is_ok(), "{name}'s worker answers get_layout: {layout:?}");
        self.people.lock().unwrap_or_else(PoisonError::into_inner).push(person.clone());
        person
    }

    /// The administrator's account (the first person made).
    pub fn admin(&self) -> Account {
        self.admin.lock().unwrap_or_else(PoisonError::into_inner).clone().expect("a person was made")
    }

    /// Another socket for the person, attached straight to the supervisor. Its messages go into the person's recorder.
    pub async fn socket(&self, person: &Person) -> Sock {
        let (handle, rx) = self.supervisor.attach(person.key).await.unwrap_or_else(|e| panic!("attach a socket: {e:?}"));
        Sock::new(handle, rx, person.recorder.clone())
    }

    /// Starts `count` panes for the person (a shell each) and returns their numbers. The first is the one the layout
    /// starts with, every other one comes from a split.
    pub async fn open_panes(&self, person: &Person, count: usize) -> Vec<u64> {
        let mut started = Vec::new();
        for index in 0..count {
            let view = if index == 0 {
                match person.ask("get_layout", json!({})).await {
                    Reply::Ok(view) => view,
                    Reply::Err(why) => panic!("get_layout: {why}"),
                }
            } else {
                match person.ask("split", json!({"column": false})).await {
                    Reply::Ok(view) => view,
                    Reply::Err(why) => panic!("split: {why}"),
                }
            };
            let pane = view["focus"].as_u64().expect("the view names the focused pane");
            let answer = person.ask("start_pane", json!({"pane": pane, "cols": 80, "rows": 24, "cell_w": 8, "cell_h": 16})).await;
            assert!(answer.is_ok(), "start_pane {pane}: {answer:?}");
            person.panes.lock().unwrap_or_else(PoisonError::into_inner).push(pane);
            started.push(pane);
        }
        started
    }

    /// Types a command into the person's first pane (starting one when there is none), waits for the result file the
    /// command wrote below the person's home and reads it with `file_open` as that person.
    pub async fn shell(&self, person: &Person, command: &str) -> Out {
        self.shell_waiting(person, command, RESULT_WAIT).await
    }

    pub async fn shell_waiting(&self, person: &Person, command: &str, wait: Duration) -> Out {
        let _one_at_a_time = person.shell_lock.lock().await;
        if person.panes().is_empty() {
            self.open_panes(person, 1).await;
        }
        let pane = person.panes()[0];
        let n = person.seq.fetch_add(1, Ordering::Relaxed);
        let base = format!("{}/.tmp/hw-{n}", person.home.display());
        let typed = format!("( {command} ) > {base}-out.txt 2>&1; echo $? > {base}-part.txt; mv {base}-part.txt {base}-st.txt\r");
        assert!(typed.len() < 3000, "a typed command stays below the line limit of the terminal");
        let sent = person.ask("text", json!({"pane": pane, "text": typed})).await;
        assert!(sent.is_ok(), "text to pane {pane}: {sent:?}");
        let deadline = Instant::now() + wait;
        let status_file = format!("{base}-st.txt");
        loop {
            let looked = person.stat(&status_file).await;
            if looked.is_ok() {
                break;
            }
            if Instant::now() >= deadline {
                let tmp = person.ask("files_list", json!({"source": "local", "path": format!("{}/.tmp", person.home.display())})).await;
                let home = person.ask("files_list", json!({"source": "local", "path": person.home.display().to_string()})).await;
                panic!(
                    "the command {command:?} left no result for {} within {wait:?}: stat said {looked:?}; .tmp holds {}; the home holds {}",
                    person.name,
                    tmp.text(),
                    home.text()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let status = person.read(&status_file).await.unwrap_or_else(|e| panic!("the status of {command:?}: {e}"));
        let status = status.trim().parse().unwrap_or_else(|_| panic!("the status of {command:?} is a number, not {status:?}"));
        let text = person.read(&format!("{base}-out.txt")).await.unwrap_or_else(|e| panic!("the output of {command:?}: {e}"));
        Out { status, text }
    }

    /// One attempt, made as `person`, in one of the ways of [`How`].
    pub async fn attempt(&self, person: &Person, how: How<'_>) -> Attempt {
        let reply = match how {
            How::Shell(command) => {
                let out = self.shell(person, command).await;
                return Attempt { ok: out.status == 0, status: Some(out.status), text: out.text };
            }
            How::Open(path) => person.ask("file_open", json!({"source": "local", "path": path})).await,
            How::Stat(path) => person.ask("file_stat", json!({"source": "local", "path": path})).await,
            How::List(path) => person.ask("files_list", json!({"source": "local", "path": path})).await,
            How::Save(path, text) => {
                person
                    .ask("file_save", json!({"source": "local", "path": path, "text": text, "expected_modified": null, "expected_digest": null}))
                    .await
            }
        };
        Attempt { ok: reply.is_ok(), status: None, text: reply.text() }
    }

    /// Writes `text` into `<home>/<name>` as the person (through their shell) and proves the owner can read it back.
    pub async fn plant(&self, person: &Person, name: &str, text: &str) -> String {
        let path = format!("{}/{name}", person.home.display());
        let out = self.shell(person, &format!("printf '%s' '{text}' > '{path}'")).await;
        assert_eq!(out.status, 0, "{} writes {name}: {}", person.name, out.text);
        let read = person.read(&path).await.unwrap_or_else(|e| panic!("{} reads their own {name}: {e}", person.name));
        assert_eq!(read, text, "the owner reads back what the shell wrote");
        path
    }

    // ---- the process table --------------------------------------------------------------------------------------------

    /// Every process of the container, read from `/proc`.
    pub fn table(&self) -> Vec<Status> {
        proc::process_table()
    }

    /// The process of the spawn helper: the child of the test process called `holzwave-spawn-`.
    pub fn helper_pid(&self) -> Option<i32> {
        let me = std::process::id() as i32;
        self.table().into_iter().find(|row| row.ppid == me && row.name.starts_with("holzwave-spawn") && !row.is_dead()).map(|row| row.pid)
    }

    /// What the audit expects of this container right now.
    pub fn expect(&self) -> Expect {
        Expect {
            supervisor: std::process::id() as i32,
            own_uid: rustix::process::getuid().as_raw(),
            helper: self.helper_pid(),
            range: RANGE,
        }
    }

    /// The worker of a person: their process whose parent is the helper.
    pub fn worker_pid(&self, person: &Person) -> Option<i32> {
        let helper = self.helper_pid()?;
        self.table().into_iter().find(|row| row.ppid == helper && row.has_uid(person.uid) && !row.is_dead()).map(|row| row.pid)
    }

    /// The pid of the person's shell, asked of the shell itself.
    pub async fn shell_pid(&self, person: &Person) -> i32 {
        let out = self.shell(person, "echo $$").await;
        out.text.trim().parse().unwrap_or_else(|_| panic!("the shell's pid is a number, not {:?}", out.text))
    }

    /// The living processes that have the user id.
    pub fn procs_of(&self, uid: u32) -> Vec<Status> {
        self.table().into_iter().filter(|row| !row.is_dead() && row.has_uid(uid)).collect()
    }

    /// Whether the process exists and is not a zombie.
    pub fn alive(&self, pid: i32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/status")).ok().and_then(|text| proc::parse_status(&text).ok()).is_some_and(|row| !row.is_dead())
    }

    /// Waits until `done` is true; returns how long it took. A timeout is a failure naming what was waited for.
    pub async fn wait_until(&self, what: &str, seconds: u64, mut done: impl FnMut() -> bool) -> Duration {
        let started = Instant::now();
        let deadline = started + Duration::from_secs(seconds);
        while !done() {
            assert!(Instant::now() < deadline, "after {seconds} seconds: {what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        started.elapsed()
    }

    // ---- the end ------------------------------------------------------------------------------------------------------

    /// Ends a person's account through the accounts service, so that the observer runs. The administrator is the actor.
    pub fn delete_account(&self, person: &Person) {
        let admin = self.admin();
        self.accounts.delete(admin.id, None, person.account.id).unwrap_or_else(|e| panic!("delete {}: {e:?}", person.name));
    }

    /// Closes every pane of the person (the worker stays).
    pub async fn close_panes(&self, person: &Person) {
        for pane in person.panes() {
            let answer = person.ask("close_pane", json!({"pane": pane})).await;
            assert!(answer.is_ok(), "close_pane {pane}: {answer:?}");
        }
        person.panes.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// Stops the supervisor in order (every worker told, the stragglers killed), then lets go of everything so that the
    /// helper's socket closes. Returns when the helper process is gone.
    pub async fn finish(self) -> Duration {
        let helper = self.helper_pid();
        self.supervisor.shutdown().await;
        let started = Instant::now();
        let World { supervisor, accounts, store, sweeper, people, .. } = self;
        sweeper.abort();
        drop(people);
        drop((supervisor, accounts, store));
        if let Some(pid) = helper {
            let deadline = Instant::now() + Duration::from_secs(10);
            while std::fs::read_to_string(format!("/proc/{pid}/status")).ok().and_then(|t| proc::parse_status(&t).ok()).is_some_and(|row| !row.is_dead()) {
                assert!(Instant::now() < deadline, "the helper {pid} did not exit after its socket closed");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        started.elapsed()
    }
}

// ---- files the server keeps -------------------------------------------------------------------------------------------

/// Writes a file with exactly `mode` (the umask does not reduce it) and returns its path. The content is random bytes: a
/// stand-in for a key, never printed.
pub fn write_secret_file(dir: &Path, name: &str, mode: u32) -> PathBuf {
    use std::io::Write;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&path).expect("a new secret file");
    file.set_permissions(std::fs::Permissions::from_mode(mode)).expect("the mode of the secret file");
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("random bytes");
    file.write_all(&bytes).expect("write the secret file");
    path
}

/// The mode of a path, without following a link.
pub fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display())).permissions().mode() & 0o7777
}

/// The owner of a path, without following a link.
pub fn owner_of(path: &Path) -> (u32, u32) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
    (meta.uid(), meta.gid())
}

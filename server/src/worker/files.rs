//! The worker's side of the file routes: the granted files it serves and the uploads it writes.
//!
//! Everything here runs as the person, inside the person's own worker; the supervisor never opens a path. Both directions treat
//! what the supervisor sends as untrusted: a token must be exactly 32 lower-case hex digits and a grant of the core, a length
//! must fit the piece size, and an upload's folder and name are checked again here whatever the supervisor already did.
//!
//! # Downloads
//!
//! A grant is made by the core (`file_grant`) and seen here when the worker's dispatch reports that command's answer. The
//! worker keeps the moment each grant was last used; a grant that has not been used for [`limits::GRANT_IDLE`] is refused as
//! unknown and revoked in the core, and each stat or read renews it. A stat is answered at once; a read runs on a small
//! bounded pool, so the thread that reads the link never waits for a disk.
//!
//! # Uploads
//!
//! One thread per open upload owns the file. The folder is opened one component at a time from the home without following a
//! link, the part file is created exclusively with mode 0600 inside it, and at the end it is published under a name that does
//! not exist yet (`name`, `name (1)`, `name (2)`, ...) with a hard link and an unlink, so an upload never overwrites a file,
//! never follows a link and never leaves its folder. The pieces come over a bounded channel, one in flight, each answered
//! after it was written. Every end except a successful commit removes the part file.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use holzwave_host::commands::registry::Outcome;
use holzwave_host::state::AppCore;
use rustix::fs::{AtFlags, Dir, Mode, OFlags};
use rustix::io::Errno;
use serde::Deserialize;

use crate::limits;
use crate::log::Level;
use crate::wire::{FILE_SOCKET, FileWhat, Frame, Res, ResBody, valid_token};
use crate::worker::dispatch::{Observer, Outbound, WorkerLog};

/// The command whose answer carries a new grant. The worker reads the token of the grant from the answer.
pub const GRANT_COMMAND: &str = "file_grant";

/// The command that ends a grant.
pub const REVOKE_COMMAND: &str = "file_revoke";

/// What a refusal of a file control says: the same text for an unknown token, an expired grant and a token that is not a
/// token, so that nothing tells them apart.
pub const NO_SUCH_FILE: &str = "\"no such file\"";

/// What a failed read or a full queue says.
const READ_FAILED: &str = "\"the file could not be read\"";

/// What a refused upload step says.
const UPLOAD_REFUSED: &str = "\"the upload was refused\"";

/// What an upload that failed on the disk says.
const UPLOAD_FAILED: &str = "\"the upload could not be written\"";

/// The start of the name of a part file.
const PART_PREFIX: &str = ".part-";

/// The longest component of an upload folder, in bytes.
const FOLDER_COMPONENT_BYTES: usize = 64;

/// The most components of an upload folder.
const FOLDER_COMPONENTS: usize = 16;

/// Switches of the integration tests; the product passes the default, which changes nothing.
#[derive(Clone, Default)]
pub struct FileHooks {
    /// A shorter idle time of a grant.
    pub grant_idle: Option<Duration>,
    /// A disk that is full after this many bytes were written to one upload.
    pub fail_write_after: Option<u64>,
    /// Each write of a piece waits this long first.
    pub write_delay: Option<Duration>,
    /// Counts what the worker did.
    pub probe: Option<Arc<FileProbe>>,
}

impl fmt::Debug for FileHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileHooks(..)")
    }
}

/// What the worker counts for a test.
#[derive(Debug, Default)]
pub struct FileProbe {
    stats: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    uploads: AtomicU64,
}

impl FileProbe {
    /// A new counter set.
    pub fn new() -> Arc<FileProbe> {
        Arc::new(FileProbe::default())
    }

    /// The stats answered.
    pub fn stats(&self) -> u64 {
        self.stats.load(Ordering::SeqCst)
    }

    /// The reads that were run.
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    /// The bytes those reads returned.
    pub fn read_bytes(&self) -> u64 {
        self.read_bytes.load(Ordering::SeqCst)
    }

    /// The uploads that were opened.
    pub fn uploads(&self) -> u64 {
        self.uploads.load(Ordering::SeqCst)
    }
}

// ---- the names ----------------------------------------------------------------------------------------------------------

/// Whether `name` is one plain file name: 1 to 255 bytes, no separator, no NUL, not `.` or `..`, and not the name a part file
/// has.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::wire::NAME_BYTES
        && name != "."
        && name != ".."
        && !name.starts_with(PART_PREFIX)
        && !name.bytes().any(|b| b == b'/' || b == 0)
}

/// Whether `dir` is a relative folder of plain components: no leading or trailing separator, no empty, `.` or `..` component,
/// no NUL, each component at most 64 bytes, at most 16 components and 1024 bytes in all.
pub fn valid_dir(dir: &str) -> bool {
    if dir.is_empty() || dir.len() > crate::wire::FOLDER_BYTES || dir.starts_with('/') || dir.bytes().any(|b| b == 0) {
        return false;
    }
    let mut count = 0;
    for part in dir.split('/') {
        count += 1;
        if part.is_empty() || part == "." || part == ".." || part.len() > FOLDER_COMPONENT_BYTES {
            return false;
        }
    }
    count <= FOLDER_COMPONENTS
}

/// The name for the `n`th try: `name` for 0, and for the others `stem (n).ext` where `ext` is what follows the last dot that
/// is not the first character.
pub fn numbered(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Whether `name` is the name of a part file this module made: `.part-<digits>-<8 hex digits>`.
fn is_part_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PART_PREFIX) else { return false };
    let Some((id, random)) = rest.split_once('-') else { return false };
    !id.is_empty()
        && id.len() <= 20
        && id.bytes().all(|b| b.is_ascii_digit())
        && random.len() == 8
        && random.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---- the grants ---------------------------------------------------------------------------------------------------------

/// When each grant was last used.
pub struct Grants {
    idle: Duration,
    used: Mutex<HashMap<String, Instant>>,
}

impl Grants {
    fn new(idle: Duration) -> Grants {
        Grants { idle, used: Mutex::new(HashMap::new()) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.used.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A grant was made: it counts as used now. The oldest record is forgotten when there are too many.
    fn record(&self, token: &str) {
        let mut used = self.lock();
        if used.len() >= limits::GRANT_RECORDS
            && !used.contains_key(token)
            && let Some(oldest) = used.iter().min_by_key(|(_, at)| **at).map(|(token, _)| token.clone())
        {
            used.remove(&oldest);
        }
        used.insert(token.to_owned(), Instant::now());
    }

    fn forget(&self, token: &str) {
        self.lock().remove(token);
    }

    /// Uses `token`: `true` and renewed when it was used within the idle time, `false` when it was not (the record is dropped
    /// then). A token without a record counts as used now.
    fn touch(&self, token: &str) -> bool {
        let mut used = self.lock();
        let now = Instant::now();
        match used.get(token) {
            Some(at) if now.saturating_duration_since(*at) > self.idle => {
                used.remove(token);
                false
            }
            _ => {
                if used.len() >= limits::GRANT_RECORDS
                    && !used.contains_key(token)
                    && let Some(oldest) = used.iter().min_by_key(|(_, at)| **at).map(|(token, _)| token.clone())
                {
                    used.remove(&oldest);
                }
                used.insert(token.to_owned(), now);
                true
            }
        }
    }

    /// The observer that sees the commands that ran: a `file_grant` answer records its token and a `file_revoke` forgets it.
    pub fn observer(self: &Arc<Grants>) -> Observer {
        let grants = self.clone();
        Arc::new(move |name: &str, args: &str, outcome: &Outcome| {
            #[derive(Deserialize)]
            struct Token {
                token: String,
            }
            match (name, outcome) {
                (GRANT_COMMAND, Outcome::Ok(text)) => {
                    if let Ok(Token { token }) = serde_json::from_str::<Token>(text)
                        && valid_token(&token)
                    {
                        grants.record(&token);
                    }
                }
                (REVOKE_COMMAND, _) => {
                    if let Ok(Token { token }) = serde_json::from_str::<Token>(args) {
                        grants.forget(&token);
                    }
                }
                _ => {}
            }
        })
    }
}

// ---- the reads ----------------------------------------------------------------------------------------------------------

/// A read waiting for a thread of the pool.
struct ReadJob {
    request: u64,
    token: String,
    offset: u64,
    length: u32,
}

/// What the read threads and the upload threads share.
struct Shared {
    core: Weak<AppCore>,
    queue: Arc<Outbound>,
    grants: Arc<Grants>,
    hooks: FileHooks,
    log: WorkerLog,
    home: PathBuf,
}

impl Shared {
    fn answer(&self, request: u64, body: ResBody) {
        self.queue.push(Frame::Res(Res { socket: FILE_SOCKET, request, body }));
    }

    /// The media type and size of a grant that is usable now. An expired grant is revoked in the core.
    fn granted(&self, token: &str) -> Option<(String, u64)> {
        if !valid_token(token) {
            return None;
        }
        let core = self.core.upgrade()?;
        let Some(found) = core.granted(token) else {
            self.grants.forget(token);
            return None;
        };
        if self.grants.touch(token) {
            Some(found)
        } else {
            core.file_revoke(token);
            None
        }
    }

    fn read(&self, job: &ReadJob) {
        let ReadJob { request, token, offset, length } = job;
        if self.granted(token).is_none() {
            return self.answer(*request, ResBody::Err(NO_SUCH_FILE.into()));
        }
        let Some(core) = self.core.upgrade() else { return self.answer(*request, ResBody::Err(NO_SUCH_FILE.into())) };
        if let Some(probe) = &self.hooks.probe {
            probe.reads.fetch_add(1, Ordering::SeqCst);
        }
        match core.read_granted(token, *offset, u64::from(*length)) {
            Some(Ok(bytes)) if bytes.len() <= usize::try_from(*length).unwrap_or(0) => {
                if let Some(probe) = &self.hooks.probe {
                    probe.read_bytes.fetch_add(bytes.len() as u64, Ordering::SeqCst);
                }
                self.answer(*request, ResBody::Binary(bytes));
            }
            Some(_) => self.answer(*request, ResBody::Err(READ_FAILED.into())),
            None => self.answer(*request, ResBody::Err(NO_SUCH_FILE.into())),
        }
    }
}

// ---- the uploads --------------------------------------------------------------------------------------------------------

/// A step of an upload, as the thread of the upload receives it.
enum Msg {
    Open { size: u64, folder: String, name: String },
    Piece(Vec<u8>),
    Close { commit: bool },
}

/// The uploads that are open: the way to each one's thread.
type Uploads = Arc<Mutex<HashMap<u64, SyncSender<Msg>>>>;

/// How many steps may wait for the thread of one upload. The supervisor has one in flight, so a third means it is broken.
const UPLOAD_STEPS: usize = 4;

/// Why an upload step failed: only a word for the log; the answer says nothing more than "refused" or "failed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    Refused,
    Disk,
}

impl Fail {
    fn text(self) -> &'static str {
        match self {
            Fail::Refused => UPLOAD_REFUSED,
            Fail::Disk => UPLOAD_FAILED,
        }
    }
}

fn disk<T>(result: Result<T, Errno>) -> Result<T, Fail> {
    result.map_err(|_| Fail::Disk)
}

fn open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Opens `folder` below the home one component at a time, making each that is missing, and never following a link: a link
/// where a component should be makes the open fail.
fn open_folder(home: &Path, folder: &str) -> Result<OwnedFd, Fail> {
    let mut dir = disk(rustix::fs::open(home, open_flags(), Mode::empty()))?;
    for part in folder.split('/') {
        match rustix::fs::mkdirat(&dir, part, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(_) => return Err(Fail::Disk),
        }
        dir = disk(rustix::fs::openat(&dir, part, open_flags(), Mode::empty()))?;
    }
    Ok(dir)
}

/// Removes the part files of this folder that are older than the limit. Only names this module makes are touched, and only
/// regular files.
fn sweep(dir: &OwnedFd, max_age: Duration) {
    let Ok(mut listing) = Dir::read_from(dir) else { return };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut doomed = Vec::new();
    while let Some(Ok(entry)) = listing.read() {
        let Ok(name) = entry.file_name().to_str() else { continue };
        if !is_part_name(name) {
            continue;
        }
        let Ok(stat) = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) else { continue };
        let regular = rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile;
        let modified = u64::try_from(stat.st_mtime).unwrap_or(u64::MAX);
        if regular && now.saturating_sub(modified) > max_age.as_secs() {
            doomed.push(name.to_owned());
        }
    }
    for name in doomed {
        let _ = rustix::fs::unlinkat(dir, name.as_str(), AtFlags::empty());
    }
}

/// The name of a new part file.
fn part_name(id: u64) -> Result<String, Fail> {
    let mut random = [0u8; 4];
    getrandom::fill(&mut random).map_err(|_| Fail::Disk)?;
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("{PART_PREFIX}{id}-{hex}"))
}

/// An upload that is being written.
struct Writing {
    dir: OwnedFd,
    part: String,
    file: Option<File>,
    name: String,
    folder: String,
    size: u64,
    written: u64,
    done: bool,
}

impl Writing {
    /// Removes the part file, unless the upload was published.
    fn discard(&mut self) {
        self.file = None;
        if !self.done {
            let _ = rustix::fs::unlinkat(&self.dir, self.part.as_str(), AtFlags::empty());
            self.done = true;
        }
    }

    /// Syncs the part file and publishes it under the first name that does not exist: a hard link and the removal of the
    /// part. The existing files are never touched. The absolute path of the new file.
    fn publish(&mut self, home: &Path) -> Result<PathBuf, Fail> {
        let file = self.file.take().ok_or(Fail::Refused)?;
        file.sync_all().map_err(|_| Fail::Disk)?;
        drop(file);
        for n in 0..limits::UPLOAD_NAME_TRIES {
            let candidate = numbered(&self.name, n);
            if candidate.len() > crate::wire::NAME_BYTES {
                return Err(Fail::Refused);
            }
            match rustix::fs::linkat(&self.dir, self.part.as_str(), &self.dir, candidate.as_str(), AtFlags::empty()) {
                Ok(()) => {
                    let _ = rustix::fs::unlinkat(&self.dir, self.part.as_str(), AtFlags::empty());
                    self.done = true;
                    let _ = rustix::fs::fsync(&self.dir);
                    return Ok(home.join(&self.folder).join(candidate));
                }
                Err(Errno::EXIST) => {}
                Err(_) => return Err(Fail::Disk),
            }
        }
        Err(Fail::Refused)
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        self.discard();
    }
}

fn start_writing(shared: &Shared, id: u64, size: u64, folder: String, name: String) -> Result<Writing, Fail> {
    let dir = open_folder(&shared.home, &folder)?;
    sweep(&dir, limits::UPLOAD_PART_MAX_AGE);
    let part = part_name(id)?;
    let fd = disk(rustix::fs::openat(
        &dir,
        part.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    ))?;
    Ok(Writing { dir, part, file: Some(File::from(fd)), name, folder, size, written: 0, done: false })
}

fn write_piece(shared: &Shared, writing: &mut Writing, bytes: &[u8]) -> Result<(), Fail> {
    let total = writing.written.checked_add(bytes.len() as u64).ok_or(Fail::Refused)?;
    if total > writing.size {
        return Err(Fail::Refused);
    }
    if let Some(delay) = shared.hooks.write_delay {
        std::thread::sleep(delay);
    }
    if shared.hooks.fail_write_after.is_some_and(|limit| total > limit) {
        return Err(Fail::Disk);
    }
    let file = writing.file.as_mut().ok_or(Fail::Refused)?;
    file.write_all(bytes).map_err(|_| Fail::Disk)?;
    writing.written = total;
    Ok(())
}

/// The thread of one upload: it opens, writes and ends it, and answers every step after it was done.
fn upload_thread(shared: Arc<Shared>, uploads: Weak<Mutex<HashMap<u64, SyncSender<Msg>>>>, id: u64, steps: Receiver<Msg>) {
    // The map is the worker's: when the worker lets go of it the senders go with it and this thread ends.
    let forget = || {
        if let Some(uploads) = uploads.upgrade() {
            uploads.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
        }
    };
    let finish = |body: ResBody| {
        forget();
        shared.answer(id, body);
    };
    let mut writing: Option<Writing> = None;
    loop {
        // The thread ends, and the part file goes, when the worker lets go of its sender.
        let Ok(step) = steps.recv() else {
            forget();
            return;
        };
        match step {
            Msg::Open { size, folder, name } => match start_writing(&shared, id, size, folder, name) {
                Ok(opened) => {
                    writing = Some(opened);
                    shared.answer(id, ResBody::Ok("{}".into()));
                }
                Err(fail) => {
                    shared.log.event(Level::Warn, "worker_upload_refused", &[("step", "open")]);
                    return finish(ResBody::Err(fail.text().into()));
                }
            },
            Msg::Piece(bytes) => {
                let Some(current) = writing.as_mut() else { return finish(ResBody::Err(UPLOAD_REFUSED.into())) };
                match write_piece(&shared, current, &bytes) {
                    Ok(()) => shared.answer(id, ResBody::Ok("{}".into())),
                    Err(fail) => {
                        shared.log.event(Level::Warn, "worker_upload_refused", &[("step", "piece")]);
                        // The part file goes before the answer says the upload is over.
                        drop(writing.take());
                        return finish(ResBody::Err(fail.text().into()));
                    }
                }
            }
            Msg::Close { commit } => {
                let Some(mut current) = writing.take() else { return finish(ResBody::Err(UPLOAD_REFUSED.into())) };
                if !commit {
                    current.discard();
                    return finish(ResBody::Ok("{}".into()));
                }
                if current.written != current.size {
                    current.discard();
                    return finish(ResBody::Err(UPLOAD_REFUSED.into()));
                }
                let size = current.size;
                match current.publish(&shared.home) {
                    Ok(path) => match path.to_str() {
                        Some(text) => {
                            let body = serde_json::json!({ "path": text, "size": size }).to_string();
                            return finish(ResBody::Ok(body));
                        }
                        None => {
                            current.discard();
                            return finish(ResBody::Err(UPLOAD_FAILED.into()));
                        }
                    },
                    Err(fail) => {
                        shared.log.event(Level::Warn, "worker_upload_refused", &[("step", "close")]);
                        current.discard();
                        return finish(ResBody::Err(fail.text().into()));
                    }
                }
            }
        }
    }
}

// ---- the module the worker holds ----------------------------------------------------------------------------------------

/// The file side of one worker.
pub struct Files {
    shared: Arc<Shared>,
    reads: SyncSender<ReadJob>,
    uploads: Uploads,
}

impl Files {
    /// The file side of the worker whose core is `core`, answering through `queue`, with uploads below `home`.
    pub fn new(core: &Arc<AppCore>, queue: &Arc<Outbound>, home: &Path, hooks: FileHooks, log: WorkerLog) -> Files {
        let idle = hooks.grant_idle.unwrap_or(limits::GRANT_IDLE);
        let shared = Arc::new(Shared {
            core: Arc::downgrade(core),
            queue: queue.clone(),
            grants: Arc::new(Grants::new(idle)),
            hooks,
            log,
            home: home.to_path_buf(),
        });
        let (reads, jobs) = sync_channel::<ReadJob>(limits::WORKER_FILE_QUEUE);
        let jobs = Arc::new(Mutex::new(jobs));
        for n in 0..limits::WORKER_FILE_THREADS {
            let (shared, jobs) = (shared.clone(), jobs.clone());
            let spawned = std::thread::Builder::new().name(format!("hw-file-{n}")).spawn(move || {
                loop {
                    let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    match job {
                        Ok(job) => shared.read(&job),
                        Err(_) => return,
                    }
                }
            });
            if spawned.is_err() {
                break;
            }
        }
        Files { shared, reads, uploads: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// The observer for the dispatch: it sees `file_grant` and `file_revoke`.
    pub fn observer(&self) -> Observer {
        self.shared.grants.observer()
    }

    /// Answers a `FileServe`: a stat at once, a read on the pool. The answer is a `Res` on the file socket.
    pub fn serve(&self, request: u64, what: FileWhat, token: &str, offset: u64, length: u32) {
        let shared = &self.shared;
        match what {
            FileWhat::Stat => {
                if let Some(probe) = &shared.hooks.probe {
                    probe.stats.fetch_add(1, Ordering::SeqCst);
                }
                match shared.granted(token) {
                    Some((mime, size)) => {
                        let body = serde_json::json!({ "mime": mime, "size": size }).to_string();
                        shared.answer(request, ResBody::Ok(body));
                    }
                    None => shared.answer(request, ResBody::Err(NO_SUCH_FILE.into())),
                }
            }
            FileWhat::Read => {
                if !valid_token(token) {
                    return shared.answer(request, ResBody::Err(NO_SUCH_FILE.into()));
                }
                let job = ReadJob { request, token: token.to_owned(), offset, length };
                if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = self.reads.try_send(job) {
                    shared.answer(request, ResBody::Err(READ_FAILED.into()));
                }
            }
        }
    }

    /// Opens an upload.
    pub fn upload_open(&self, id: u64, size: u64, folder: String, name: String) {
        let shared = &self.shared;
        if !valid_dir(&folder) || !valid_name(&name) {
            shared.log.event(Level::Warn, "worker_upload_refused", &[("step", "names")]);
            return shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into()));
        }
        let (steps, inbox) = sync_channel::<Msg>(UPLOAD_STEPS);
        {
            let mut open = self.uploads.lock().unwrap_or_else(PoisonError::into_inner);
            if open.contains_key(&id) || open.len() >= limits::WORKER_OPEN_UPLOADS {
                drop(open);
                return shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into()));
            }
            open.insert(id, steps.clone());
        }
        if steps.try_send(Msg::Open { size, folder, name }).is_err() {
            self.uploads.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            return shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into()));
        }
        if let Some(probe) = &shared.hooks.probe {
            probe.uploads.fetch_add(1, Ordering::SeqCst);
        }
        let (thread_shared, uploads) = (shared.clone(), Arc::downgrade(&self.uploads));
        let spawned = std::thread::Builder::new()
            .name("hw-upload".into())
            .spawn(move || upload_thread(thread_shared, uploads, id, inbox));
        if spawned.is_err() {
            self.uploads.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            shared.answer(id, ResBody::Err(UPLOAD_FAILED.into()));
        }
    }

    fn step(&self, id: u64, step: Msg) {
        let shared = &self.shared;
        let sender = self.uploads.lock().unwrap_or_else(PoisonError::into_inner).get(&id).cloned();
        let Some(sender) = sender else { return shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into())) };
        match sender.try_send(step) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                // The supervisor sends one step at a time; more waiting means it is broken. The upload ends: the thread sees
                // its sender go and removes the part file.
                self.uploads.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into()));
            }
            Err(TrySendError::Disconnected(_)) => {
                self.uploads.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                shared.answer(id, ResBody::Err(UPLOAD_REFUSED.into()));
            }
        }
    }

    /// The next bytes of an upload.
    pub fn upload_piece(&self, id: u64, bytes: Vec<u8>) {
        self.step(id, Msg::Piece(bytes));
    }

    /// Publishes or removes an upload.
    pub fn upload_close(&self, id: u64, commit: bool) {
        self.step(id, Msg::Close { commit });
    }
}

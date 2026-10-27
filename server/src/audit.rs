//! The append-only audit log: one JSON line per security event, one file per day.
//!
//! The files are `<data dir>/server/audit/audit-YYYY-MM-DD.jsonl` (the UTC day from the injected clock), created with mode 0600
//! in a 0700 directory, opened with `O_APPEND` and no-follow, and written with one `write` per line. A line is
//!
//! ```text
//! {"ts":"2026-03-01T08:30:00Z","action":"signin.ok","result":"ok","actor":"3","addr":"203.0.113.9","target":"","detail":{"method":"passkey"}}
//! ```
//!
//! Nothing a person types can enter a line. The action is a closed enum, the address is a parsed [`IpAddr`], the actor and
//! the target are account numbers, and every member of `detail` is a key from a closed enum with a number, a boolean or a
//! static string as value. A password typed into a username box, a token or a username therefore has no way into the file.

use std::fmt;
use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use serde::ser::SerializeMap;

use crate::clock::{Clock, rfc3339, utc_date};
use crate::store::{AccountId, SERVER_DIR, ensure_private_dir};

/// The name of the audit directory inside `<data dir>/server`.
pub const AUDIT_DIR: &str = "audit";

/// What happened. The identifiers are the audit vocabulary of the server; a new action is added here and nowhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    BootstrapRedeem,
    SigninOk,
    SigninFail,
    SigninLocked,
    Signout,
    SessionExpired,
    SessionStepUp,
    AccountCreate,
    AccountInvite,
    AccountRedeem,
    AccountDisable,
    AccountEnable,
    AccountDelete,
    AccountRole,
    AccountUnlock,
    OidcCreate,
    AuthPasswordChange,
    AuthTotpEnrol,
    AuthPasskeyAdd,
    AuthPasskeyRemove,
    AuthPassphraseSet,
    RecoveryUse,
    SecretsReset,
    SecretsUnlock,
    SecretsLock,
}

impl Action {
    /// Every action, in the order of the vocabulary.
    pub const ALL: [Action; 25] = [
        Action::BootstrapRedeem,
        Action::SigninOk,
        Action::SigninFail,
        Action::SigninLocked,
        Action::Signout,
        Action::SessionExpired,
        Action::SessionStepUp,
        Action::AccountCreate,
        Action::AccountInvite,
        Action::AccountRedeem,
        Action::AccountDisable,
        Action::AccountEnable,
        Action::AccountDelete,
        Action::AccountRole,
        Action::AccountUnlock,
        Action::OidcCreate,
        Action::AuthPasswordChange,
        Action::AuthTotpEnrol,
        Action::AuthPasskeyAdd,
        Action::AuthPasskeyRemove,
        Action::AuthPassphraseSet,
        Action::RecoveryUse,
        Action::SecretsReset,
        Action::SecretsUnlock,
        Action::SecretsLock,
    ];

    /// The identifier written to the file.
    pub fn as_str(self) -> &'static str {
        match self {
            Action::BootstrapRedeem => "bootstrap.redeem",
            Action::SigninOk => "signin.ok",
            Action::SigninFail => "signin.fail",
            Action::SigninLocked => "signin.locked",
            Action::Signout => "signout",
            Action::SessionExpired => "session.expired",
            Action::SessionStepUp => "session.step_up",
            Action::AccountCreate => "account.create",
            Action::AccountInvite => "account.invite",
            Action::AccountRedeem => "account.redeem",
            Action::AccountDisable => "account.disable",
            Action::AccountEnable => "account.enable",
            Action::AccountDelete => "account.delete",
            Action::AccountRole => "account.role",
            Action::AccountUnlock => "account.unlock",
            Action::OidcCreate => "oidc.create",
            Action::AuthPasswordChange => "auth.password.change",
            Action::AuthTotpEnrol => "auth.totp.enrol",
            Action::AuthPasskeyAdd => "auth.passkey.add",
            Action::AuthPasskeyRemove => "auth.passkey.remove",
            Action::AuthPassphraseSet => "auth.passphrase.set",
            Action::RecoveryUse => "recovery.use",
            Action::SecretsReset => "secrets.reset",
            Action::SecretsUnlock => "secrets.unlock",
            Action::SecretsLock => "secrets.lock",
        }
    }
}

/// How it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It happened.
    Ok,
    /// It was refused or failed.
    Fail,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Fail => "fail",
        }
    }
}

/// Who did it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// Nobody was signed in.
    Anonymous,
    /// This account.
    Account(AccountId),
}

/// The names a `detail` member can have. The set is closed: no free string is a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKey {
    /// The sign-in method: the static strings `password_totp`, `passkey` and `oidc`.
    Method,
    /// A step of a flow, as a number or a static string.
    Step,
    /// Why something failed, as a static string.
    Reason,
    /// A role, as a static string.
    Role,
    /// An account state, as a static string.
    State,
    /// The name of a configured identity provider, as a static string.
    Provider,
    /// A count.
    Count,
    /// A number of days.
    Days,
    /// A slot number.
    Slot,
}

impl DetailKey {
    /// Every key.
    pub const ALL: [DetailKey; 9] = [
        DetailKey::Method,
        DetailKey::Step,
        DetailKey::Reason,
        DetailKey::Role,
        DetailKey::State,
        DetailKey::Provider,
        DetailKey::Count,
        DetailKey::Days,
        DetailKey::Slot,
    ];

    /// The key as written to the file.
    pub fn as_str(self) -> &'static str {
        match self {
            DetailKey::Method => "method",
            DetailKey::Step => "step",
            DetailKey::Reason => "reason",
            DetailKey::Role => "role",
            DetailKey::State => "state",
            DetailKey::Provider => "provider",
            DetailKey::Count => "count",
            DetailKey::Days => "days",
            DetailKey::Slot => "slot",
        }
    }
}

/// The value of a `detail` member: a number, a boolean or a string that is part of the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailValue {
    /// A number.
    Number(i64),
    /// A yes or no.
    Bool(bool),
    /// A static string.
    Text(&'static str),
}

/// One event to write.
#[derive(Debug, Clone)]
pub struct Event {
    action: Action,
    result: Outcome,
    actor: Actor,
    addr: Option<IpAddr>,
    target: Option<AccountId>,
    detail: Vec<(DetailKey, DetailValue)>,
}

impl Event {
    /// An event without a target and without detail. `addr` is the parsed address of the peer or the forwarded client, or
    /// `None` for an event the server causes itself.
    pub fn new(action: Action, result: Outcome, actor: Actor, addr: Option<IpAddr>) -> Event {
        Event { action, result, actor, addr, target: None, detail: Vec::new() }
    }

    /// The account the action was about.
    pub fn target(mut self, target: AccountId) -> Event {
        self.target = Some(target);
        self
    }

    /// A detail member; a second value for the same key replaces the first.
    pub fn detail(mut self, key: DetailKey, value: DetailValue) -> Event {
        match self.detail.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.detail.push((key, value)),
        }
        self
    }
}

/// A failure to write the audit log: one sentence without a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditError(&'static str);

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the audit log cannot be written: {}", self.0)
    }
}

impl std::error::Error for AuditError {}

struct DetailMap<'a>(&'a [(DetailKey, DetailValue)]);

impl Serialize for DetailMap<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            match value {
                DetailValue::Number(n) => map.serialize_entry(key.as_str(), n)?,
                DetailValue::Bool(b) => map.serialize_entry(key.as_str(), b)?,
                DetailValue::Text(t) => map.serialize_entry(key.as_str(), t)?,
            }
        }
        map.end()
    }
}

#[derive(Serialize)]
struct Line<'a> {
    ts: &'a str,
    action: &'a str,
    result: &'a str,
    actor: &'a str,
    addr: &'a str,
    target: &'a str,
    detail: DetailMap<'a>,
}

/// The file being written and the day it belongs to.
struct Current {
    date: String,
    file: File,
}

/// The audit log.
pub struct Audit {
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    current: Mutex<Option<Current>>,
}

impl fmt::Debug for Audit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Audit(..)")
    }
}

impl Audit {
    /// Opens the log of the data directory: makes `<data dir>/server/audit` with mode 0700 (the data directory itself must
    /// exist). Files are made when the first line of a day is written.
    pub fn open(data_dir: &Path, clock: Arc<dyn Clock>) -> Result<Audit, AuditError> {
        let server = data_dir.join(SERVER_DIR);
        let dir = server.join(AUDIT_DIR);
        ensure_private_dir(&server).map_err(|_| AuditError("the server directory cannot be prepared"))?;
        ensure_private_dir(&dir).map_err(|_| AuditError("the audit directory cannot be prepared"))?;
        Ok(Audit { dir, clock, current: Mutex::new(None) })
    }

    /// The directory of the audit files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path of the file of a UTC day (`YYYY-MM-DD`).
    pub fn file_for(&self, date: &str) -> PathBuf {
        self.dir.join(format!("audit-{date}.jsonl"))
    }

    fn open_file(&self, date: &str) -> Result<File, AuditError> {
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("the open flags fit in an i32"))
            .open(self.file_for(date))
            .map_err(|_| AuditError("the day's file cannot be opened (a link or a loose directory is refused)"))?;
        let meta = file.metadata().map_err(|_| AuditError("the day's file cannot be examined"))?;
        if !meta.file_type().is_file() {
            return Err(AuditError("the day's file is not a regular file"));
        }
        if meta.mode() & 0o777 != 0o600 {
            // A file that was copied in with another mode is made private before a line goes into it.
            file.set_permissions(Permissions::from_mode(0o600)).map_err(|_| AuditError("the day's file cannot be made private"))?;
        }
        Ok(file)
    }

    /// Writes one line. The day comes from the clock; a day change opens the next file. On a failure nothing is cached, so
    /// the next call tries again.
    pub fn record(&self, event: &Event) -> Result<(), AuditError> {
        let now = self.clock.now();
        let date = utc_date(now);
        let ts = rfc3339(now);
        let actor = match event.actor {
            Actor::Anonymous => "anonymous".to_owned(),
            Actor::Account(id) => id.to_string(),
        };
        let addr = event.addr.map(|a| a.to_string()).unwrap_or_default();
        let target = event.target.map(|t| t.to_string()).unwrap_or_default();
        let line = Line {
            ts: &ts,
            action: event.action.as_str(),
            result: event.result.as_str(),
            actor: &actor,
            addr: &addr,
            target: &target,
            detail: DetailMap(&event.detail),
        };
        let mut buf = serde_json::to_vec(&line).map_err(|_| AuditError("the line cannot be encoded"))?;
        buf.push(b'\n');

        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current.as_ref().is_none_or(|c| c.date != date) {
            *current = None;
            let file = self.open_file(&date)?;
            *current = Some(Current { date, file });
        }
        let slot = current.as_mut().expect("the file was just opened");
        let written = write_once(&mut slot.file, &buf);
        if written.is_err() {
            *current = None;
        }
        written
    }
}

/// Writes the whole buffer with one `write`; only an interrupted call is repeated. A short write is an error, because a
/// split line could interleave with another writer's.
fn write_once(file: &mut File, buf: &[u8]) -> Result<(), AuditError> {
    loop {
        match file.write(buf) {
            Ok(n) if n == buf.len() => return Ok(()),
            Ok(_) => return Err(AuditError("the line was written only in part")),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(AuditError("the write failed")),
        }
    }
}

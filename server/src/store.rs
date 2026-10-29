//! The SQLite store of accounts, sessions, invitations and sign-in material, with forward-only migrations.
//!
//! One [`Store`] owns one connection behind a mutex. The API is synchronous; the HTTP code calls it from a blocking task.
//! Every query is a method of [`Tx`] with bound parameters (no SQL is built from data), and [`Store::write`] and
//! [`Store::read`] run a closure inside one transaction, so a caller composes several queries into one atomic step:
//! `write` begins immediately, commits when the closure returns `Ok` and rolls back when it returns `Err`.
//!
//! The database lives in `<data dir>/server/server.sqlite3` (mode 0600, in a 0700 directory) in WAL mode with
//! `synchronous=FULL`. No secret is stored in the clear: passwords as a verifier and a salt, TOTP secrets sealed by the
//! caller, tokens and session ids as their SHA-256. Errors are sentences without SQL, values or paths.
//!
//! Time is always a plain `now` argument in unix seconds, so tests drive it without a clock.

use std::fmt;
use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::ops::RangeInclusive;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use sha2::{Digest, Sha256};

/// The value of `PRAGMA application_id`: the bytes `hwSV`. A database with another value is not ours.
pub const APPLICATION_ID: i64 = 0x6877_5356;

/// The schema version this code writes and the newest one it reads.
pub const SCHEMA_VERSION: i64 = 2;

/// How long a statement waits for another writer before it fails with a busy error.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The most passkeys one account can have.
pub const MAX_PASSKEYS: usize = 10;

/// The longest username, in characters.
pub const MAX_USERNAME: usize = 64;

/// The longest display name, in characters.
pub const MAX_DISPLAY_NAME: usize = 128;

/// The length of a WebAuthn user handle in bytes.
pub const HANDLE_BYTES: usize = 16;

/// The name of the database file inside `<data dir>/server`.
pub const DATABASE_FILE: &str = "server.sqlite3";

/// The name of the server's own directory inside the data directory.
pub const SERVER_DIR: &str = "server";

/// A SHA-256 hash: of a one-time token or of a session id. The token itself is never stored.
pub type TokenHash = [u8; 32];

/// The SHA-256 of `bytes`.
pub fn hash_token(bytes: &[u8]) -> TokenHash {
    Sha256::digest(bytes).into()
}

/// Why a store call failed. The texts are sentences; none holds SQL, a value from a row or a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The row asked for is not there.
    NotFound,
    /// The row would break a uniqueness rule. The text does not say which row holds the value.
    Conflict,
    /// The change would leave no active administrator.
    LastAdmin,
    /// Every user id of the configured range is taken.
    UidRangeFull,
    /// A one-time token is unknown, expired or used. One error for all three.
    NotRedeemable,
    /// The database was written by a newer schema than this code knows.
    NewerSchema {
        /// The version in the file.
        found: i64,
        /// The newest version this code supports.
        supported: i64,
    },
    /// The file is not a database of this server (or is damaged). It was left untouched.
    NotOurDatabase,
    /// An input broke a rule; the text is a fixed sentence.
    Invalid(&'static str),
    /// The account already has the most passkeys.
    PasskeyLimit,
    /// The database or its files failed; the text is a fixed sentence.
    Storage(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound => f.write_str("no such record"),
            StoreError::Conflict => f.write_str("that name or credential is already in use"),
            StoreError::LastAdmin => {
                f.write_str("the last active administrator cannot be demoted, disabled or deleted")
            }
            StoreError::UidRangeFull => f.write_str("no free user ids in the configured range"),
            StoreError::NotRedeemable => f.write_str("the token is not valid"),
            StoreError::NewerSchema { found, supported } => write!(
                f,
                "the database has schema version {found} but this server supports up to {supported}; \
                 run a newer server instead of changing the database"
            ),
            StoreError::NotOurDatabase => f.write_str(
                "the database file is not a usable database of this server; it was left untouched, \
                 move it away or name another data directory",
            ),
            StoreError::Invalid(reason) => f.write_str(reason),
            StoreError::PasskeyLimit => write!(f, "an account can have at most {MAX_PASSKEYS} passkeys"),
            StoreError::Storage(reason) => write!(f, "the database failed: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> StoreError {
        use rusqlite::ErrorCode;
        use rusqlite::ffi;
        match &e {
            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound,
            rusqlite::Error::SqliteFailure(failure, _) => match failure.extended_code {
                ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => StoreError::Conflict,
                ffi::SQLITE_CONSTRAINT_FOREIGNKEY => StoreError::NotFound,
                _ => StoreError::Storage(match failure.code {
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => "another writer holds the database",
                    ErrorCode::ReadOnly => "the database is read-only",
                    ErrorCode::DiskFull => "the disk is full",
                    ErrorCode::SystemIoFailure => "a read or write failed",
                    ErrorCode::ConstraintViolation => "a value broke a rule of the database",
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => "the file is damaged",
                    _ => "an operation was refused",
                }),
            },
            _ => StoreError::Storage("an operation was refused"),
        }
    }
}

/// The result of a store call.
pub type Result<T> = std::result::Result<T, StoreError>;

fn io_error(_: std::io::Error) -> StoreError {
    StoreError::Storage("the data directory or the database file cannot be prepared")
}

/// Creates the directory with mode 0700 (and tightens an existing one); a link in its place is refused.
pub(crate) fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_dir() {
        return Err(std::io::Error::other("not a directory"));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(path, Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The id of an account row. Ids are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountId(i64);

impl AccountId {
    /// An id from its number (a number that names no account is refused by the queries).
    pub fn from_raw(raw: i64) -> AccountId {
        AccountId(raw)
    }

    /// The number.
    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What an account may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Manages people and the server.
    Admin,
    /// Uses their own workspace.
    Member,
}

impl Role {
    /// The spelling in the database and in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }

    /// The role of its spelling.
    pub fn parse(text: &str) -> Option<Role> {
        match text {
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
}

/// Whether an account can sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    /// Can sign in.
    Active,
    /// Cannot sign in until enabled again.
    Disabled,
    /// Gone; the row stays so that the user id and the name are never given to anyone else.
    Deleted,
}

impl AccountState {
    /// The spelling in the database and in the API.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::Active => "active",
            AccountState::Disabled => "disabled",
            AccountState::Deleted => "deleted",
        }
    }

    /// The state of its spelling.
    pub fn parse(text: &str) -> Option<AccountState> {
        match text {
            "active" => Some(AccountState::Active),
            "disabled" => Some(AccountState::Disabled),
            "deleted" => Some(AccountState::Deleted),
            _ => None,
        }
    }
}

/// One row of `accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The row id.
    pub id: AccountId,
    /// The sign-in name: lower case ASCII.
    pub username: String,
    /// The WebAuthn user handle: 16 random bytes, fixed for the life of the account.
    pub handle: [u8; HANDLE_BYTES],
    /// The name shown to people.
    pub display_name: String,
    /// The role.
    pub role: Role,
    /// The operating-system user id the person's worker runs as. Never reused.
    pub uid: u32,
    /// The state.
    pub state: AccountState,
    /// Unix seconds.
    pub created_at: i64,
}

/// What a new account is made from.
#[derive(Debug, Clone)]
pub struct NewAccount<'a> {
    /// The sign-in name (see [`valid_username`]).
    pub username: &'a str,
    /// The name shown to people; defaults to the username.
    pub display_name: &'a str,
    /// The role.
    pub role: Role,
    /// The WebAuthn handle the caller chose (a passkey enrolment chooses it before the account exists), or `None` for a
    /// random one.
    pub handle: Option<[u8; HANDLE_BYTES]>,
}

impl<'a> NewAccount<'a> {
    /// An account with `username`, shown under the same name, with a random handle.
    pub fn new(username: &'a str, role: Role) -> NewAccount<'a> {
        NewAccount { username, display_name: username, role, handle: None }
    }

    /// The same account shown under another name.
    pub fn display_name(mut self, display_name: &'a str) -> NewAccount<'a> {
        self.display_name = display_name;
        self
    }

    /// The same account with a handle the caller chose.
    pub fn handle(mut self, handle: [u8; HANDLE_BYTES]) -> NewAccount<'a> {
        self.handle = Some(handle);
        self
    }
}

/// Whether `name` is a username: 1 to 64 characters of `a-z 0-9 . _ -`, not starting or ending with a dot.
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_USERNAME
        && name.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
        && !name.starts_with('.')
        && !name.ends_with('.')
}

/// Whether `name` can be shown: 1 to 128 characters, none a control character.
fn valid_display_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_DISPLAY_NAME).contains(&count) && !name.chars().any(char::is_control)
}

/// The statements that make schema version 1.
const SCHEMA_V1: &str = "
CREATE TABLE accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    username TEXT NOT NULL UNIQUE CHECK (length(username) BETWEEN 1 AND 64),
    handle BLOB NOT NULL UNIQUE CHECK (length(handle) = 16),
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 512),
    role TEXT NOT NULL CHECK (role IN ('admin', 'member')),
    uid INTEGER NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN ('active', 'disabled', 'deleted')),
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE password_credentials (
    account_id INTEGER NOT NULL UNIQUE REFERENCES accounts (id),
    verifier BLOB NOT NULL,
    salt BLOB NOT NULL,
    params TEXT NOT NULL,
    changed_at INTEGER NOT NULL
) STRICT;
CREATE TABLE totp (
    account_id INTEGER NOT NULL UNIQUE REFERENCES accounts (id),
    sealed_secret BLOB NOT NULL,
    last_step INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE TABLE passkeys (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    credential_id BLOB NOT NULL UNIQUE,
    passkey_json TEXT NOT NULL,
    prf_salt BLOB,
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE oidc_identities (
    issuer TEXT NOT NULL,
    sub TEXT NOT NULL,
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    UNIQUE (issuer, sub)
) STRICT;
CREATE TABLE invites (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    account_id INTEGER REFERENCES accounts (id),
    role TEXT NOT NULL CHECK (role IN ('admin', 'member')),
    created_by INTEGER NOT NULL REFERENCES accounts (id),
    expires_at INTEGER NOT NULL,
    redeemed_at INTEGER
) STRICT;
CREATE TABLE bootstrap (
    token_hash BLOB NOT NULL CHECK (length(token_hash) = 32),
    expires_at INTEGER NOT NULL,
    redeemed_at INTEGER
) STRICT;
CREATE TABLE sessions (
    id_hash BLOB PRIMARY KEY CHECK (length(id_hash) = 32),
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    created_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    last_auth INTEGER NOT NULL,
    method TEXT NOT NULL,
    absolute_expires INTEGER NOT NULL,
    device TEXT NOT NULL,
    addr TEXT NOT NULL
) STRICT;
CREATE INDEX sessions_by_account ON sessions (account_id);
CREATE TABLE throttle (
    key TEXT PRIMARY KEY,
    window_start INTEGER NOT NULL,
    failures INTEGER NOT NULL,
    locked_until INTEGER NOT NULL
) STRICT;
CREATE TABLE unlock_methods (
    account_id INTEGER NOT NULL REFERENCES accounts (id),
    kind TEXT NOT NULL CHECK (kind IN ('password', 'passphrase', 'prf', 'recovery')),
    salt BLOB NOT NULL,
    params TEXT NOT NULL,
    credential_id BLOB
) STRICT;
CREATE INDEX unlock_methods_by_account ON unlock_methods (account_id);
";

/// The migrations, indexed by the version they start from: entry `n` takes a database at version `n` to `n + 1`.
const MIGRATIONS: [fn(&Connection) -> rusqlite::Result<()>; SCHEMA_VERSION as usize] = [migrate_to_1, migrate_to_2];

fn migrate_to_1(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_V1)?;
    conn.pragma_update(None, "application_id", APPLICATION_ID)
}

/// Version 2 adds the time of the last sign-in to an account: nullable, and `NULL` until the account signs in again. It
/// takes no value from the sessions that exist, because a session row says when it was made, not that it was the newest.
fn migrate_to_2(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch("ALTER TABLE accounts ADD COLUMN last_signin INTEGER;")
}

/// Looks at the identity of an open database with queries that write nothing. A database that is empty and carries no
/// identity (a new file) and a database of this server at a version this code knows pass; anything else is refused.
fn classify(conn: &Connection) -> Result<()> {
    use rusqlite::ErrorCode;
    let not_ours = |e: rusqlite::Error| match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) =>
        {
            StoreError::NotOurDatabase
        }
        _ => StoreError::from(e),
    };
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0)).map_err(not_ours)?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(not_ours)?;
    let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0)).map_err(not_ours)?;
    if application_id == APPLICATION_ID {
        if user_version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found: user_version, supported: SCHEMA_VERSION });
        }
        return Ok(());
    }
    if application_id == 0 && user_version == 0 && tables == 0 {
        return Ok(());
    }
    Err(StoreError::NotOurDatabase)
}

/// Applies every migration from the version the file is at, each in one immediate transaction.
fn migrate(conn: &mut Connection) -> Result<()> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Read again under the lock: another process may have migrated while this one waited.
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::NewerSchema { found: version, supported: SCHEMA_VERSION });
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        let step = usize::try_from(version).map_err(|_| StoreError::NotOurDatabase)?;
        MIGRATIONS[step](&tx)?;
        tx.pragma_update(None, "user_version", version + 1)?;
        tx.commit()?;
    }
}

/// The store: one connection, one mutex.
pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Store(..)")
    }
}

impl Store {
    /// Opens `<data_dir>/server/server.sqlite3`, making the directory and the database when they are missing.
    ///
    /// The directory is made 0700 and the files 0600 (the `-wal` and `-shm` files follow the database file). A link in place
    /// of the directory or the file is refused. The file is looked at with queries that write nothing before anything is
    /// changed, so a file that is not a database of this server, or a newer one, is refused and left as it was.
    pub fn open(data_dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(data_dir).map_err(io_error)?;
        // The operator's own path may pass through links (a mounted volume); everything below it is checked here, and SQLite
        // is given a path without links so that its no-follow flag speaks only about the file.
        let data_dir = std::fs::canonicalize(data_dir).map_err(io_error)?;
        let dir = data_dir.join(SERVER_DIR);
        ensure_private_dir(&dir).map_err(io_error)?;
        let path = dir.join(DATABASE_FILE);
        // Make the file ourselves so that it exists with mode 0600 from its first byte and a link is refused.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).expect("the open flags fit in an i32"))
            .open(&path)
            .map_err(io_error)?;
        if !file.metadata().map_err(io_error)?.file_type().is_file() {
            return Err(StoreError::Storage("the database path is not a regular file"));
        }
        drop(file);

        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut conn = Connection::open_with_flags(&path, flags)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        classify(&conn)?;
        // Now it is ours: tighten the mode of a file that existed, then set the connection up.
        std::fs::set_permissions(&path, Permissions::from_mode(0o600)).map_err(io_error)?;
        let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Storage("the file system does not support write-ahead logging"));
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "trusted_schema", "OFF")?;
        conn.pragma_update(None, "cell_size_check", "ON")?;
        migrate(&mut conn)?;
        Ok(Store { conn: Mutex::new(conn), path })
    }

    /// The path of the database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        match self.conn.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // A panic inside a closure rolled its transaction back when the handle dropped, so the connection is still
                // sound; a panic inside `read` may have left the connection read-only, which is undone here.
                let guard = poisoned.into_inner();
                let _ = guard.pragma_update(None, "query_only", false);
                guard
            }
        }
    }

    /// Runs `f` in one immediate transaction: committed when it returns `Ok`, rolled back when it returns `Err`.
    pub fn write<T>(&self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&Tx { conn: &tx })?;
        tx.commit()?;
        Ok(out)
    }

    /// Runs `f` in one read transaction, so every query in it sees the same state. A write inside is refused.
    pub fn read<T>(&self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let mut guard = self.lock();
        let tx = guard.transaction_with_behavior(TransactionBehavior::Deferred)?;
        tx.pragma_update(None, "query_only", true)?;
        let out = f(&Tx { conn: &tx });
        // The setting belongs to the connection, not to the transaction: put it back before anything else runs.
        let reset = tx.pragma_update(None, "query_only", false);
        let out = out?;
        reset?;
        Ok(out)
    }

    /// Creates the one-time token the first administrator redeems. See [`Tx::bootstrap_set`].
    pub fn bootstrap_set(&self, token_hash: &TokenHash, expires_at: i64) -> Result<()> {
        self.write(|tx| tx.bootstrap_set(token_hash, expires_at))
    }

    /// An account by id. See [`Tx::account_get`].
    pub fn account_get(&self, id: AccountId) -> Result<Option<Account>> {
        self.read(|tx| tx.account_get(id))
    }

    /// An account by username. See [`Tx::account_by_username`].
    pub fn account_by_username(&self, username: &str) -> Result<Option<Account>> {
        self.read(|tx| tx.account_by_username(username))
    }
}

/// The columns of an account row, in the order [`account_from_row`] reads them.
macro_rules! select_account {
    ($tail:literal) => {
        concat!("SELECT id, username, handle, display_name, role, uid, state, created_at FROM accounts ", $tail)
    };
}

fn corrupt_column() -> rusqlite::Error {
    rusqlite::Error::InvalidColumnType(0, "stored value".into(), rusqlite::types::Type::Text)
}

fn account_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let handle: Vec<u8> = row.get(2)?;
    let role: String = row.get(4)?;
    let uid: i64 = row.get(5)?;
    let state: String = row.get(6)?;
    Ok(Account {
        id: AccountId(row.get(0)?),
        username: row.get(1)?,
        handle: handle.try_into().map_err(|_| corrupt_column())?,
        display_name: row.get(3)?,
        role: Role::parse(&role).ok_or_else(corrupt_column)?,
        uid: u32::try_from(uid).map_err(|_| corrupt_column())?,
        state: AccountState::parse(&state).ok_or_else(corrupt_column)?,
        created_at: row.get(7)?,
    })
}

/// The settings of the connection as the database reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    /// `journal_mode`, lower case.
    pub journal_mode: String,
    /// `synchronous` as the number SQLite reports (2 is FULL).
    pub synchronous: i64,
    /// `foreign_keys`.
    pub foreign_keys: bool,
    /// `secure_delete`.
    pub secure_delete: bool,
    /// `trusted_schema`.
    pub trusted_schema: bool,
    /// `busy_timeout` in milliseconds.
    pub busy_timeout_ms: i64,
    /// `application_id`.
    pub application_id: i64,
    /// `user_version`.
    pub user_version: i64,
}

/// A table with its columns as `(name, declared type)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    /// The table name.
    pub name: String,
    /// The columns, sorted by name.
    pub columns: Vec<(String, String)>,
}

/// The handle of a transaction; every query is a method of it.
pub struct Tx<'a> {
    conn: &'a Connection,
}

impl fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tx(..)")
    }
}

impl Tx<'_> {
    /// The settings of the connection.
    pub fn diagnostics(&self) -> Result<Diagnostics> {
        let int = |name: &str| -> Result<i64> {
            // The pragma names are literals of this file; a name is never taken from data.
            let sql = match name {
                "synchronous" => "PRAGMA synchronous",
                "foreign_keys" => "PRAGMA foreign_keys",
                "secure_delete" => "PRAGMA secure_delete",
                "trusted_schema" => "PRAGMA trusted_schema",
                "busy_timeout" => "PRAGMA busy_timeout",
                "application_id" => "PRAGMA application_id",
                _ => "PRAGMA user_version",
            };
            Ok(self.conn.query_row(sql, [], |r| r.get(0))?)
        };
        let journal_mode: String = self.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        Ok(Diagnostics {
            journal_mode: journal_mode.to_ascii_lowercase(),
            synchronous: int("synchronous")?,
            foreign_keys: int("foreign_keys")? != 0,
            secure_delete: int("secure_delete")? != 0,
            trusted_schema: int("trusted_schema")? != 0,
            busy_timeout_ms: int("busy_timeout")?,
            application_id: int("application_id")?,
            user_version: int("user_version")?,
        })
    }

    /// Every table with its columns, both sorted by name. The tables SQLite keeps for itself are
    /// left out.
    pub fn schema(&self) -> Result<Vec<TableInfo>> {
        let mut tables = self.conn.prepare_cached(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let names: Vec<String> = tables.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let mut columns = self.conn.prepare_cached("SELECT name, type FROM pragma_table_info(?1) ORDER BY name")?;
            let columns: Vec<(String, String)> =
                columns.query_map(params![name], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            out.push(TableInfo { name, columns });
        }
        Ok(out)
    }

    // ---- accounts ----

    /// An account by id.
    pub fn account_get(&self, id: AccountId) -> Result<Option<Account>> {
        let mut stmt = self.conn.prepare_cached(select_account!("WHERE id = ?1"))?;
        Ok(optional(stmt.query_row(params![id.0], account_from_row))?)
    }

    /// An account by username (exact match, including deleted accounts, whose names stay reserved).
    pub fn account_by_username(&self, username: &str) -> Result<Option<Account>> {
        let mut stmt = self.conn.prepare_cached(select_account!("WHERE username = ?1"))?;
        Ok(optional(stmt.query_row(params![username], account_from_row))?)
    }

    /// The number of account rows, deleted ones included.
    pub fn account_count(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT count(*) FROM accounts", [], |r| r.get(0))?)
    }

    /// Creates an account and gives it the next user id of `range`.
    ///
    /// The id is the highest user id ever allocated plus one, or the start of the range when none is above it. Rows of
    /// deleted accounts stay, so an id is never given twice. A full range is [`StoreError::UidRangeFull`]; a taken
    /// username or handle is [`StoreError::Conflict`], which does not say which.
    pub fn account_create(&self, new: &NewAccount<'_>, range: RangeInclusive<u32>, now: i64) -> Result<Account> {
        if !valid_username(new.username) {
            return Err(StoreError::Invalid(
                "a username is 1 to 64 characters of a-z, 0-9, dot, underscore and hyphen, and does not start or end with a dot",
            ));
        }
        if !valid_display_name(new.display_name) {
            return Err(StoreError::Invalid("a display name is 1 to 128 characters without control characters"));
        }
        let handle = match new.handle {
            Some(handle) => {
                if handle.iter().all(|b| *b == 0) {
                    return Err(StoreError::Invalid("the account handle must not be all zero"));
                }
                handle
            }
            None => {
                let mut handle = [0u8; HANDLE_BYTES];
                getrandom::fill(&mut handle).map_err(|_| StoreError::Storage("no random bytes for the account handle"))?;
                handle
            }
        };
        let (first, last) = (i64::from(*range.start()), i64::from(*range.end()));
        let highest: Option<i64> = self.conn.query_row("SELECT max(uid) FROM accounts", [], |r| r.get(0))?;
        let uid = match highest {
            Some(highest) => (highest + 1).max(first),
            None => first,
        };
        if first > last || uid > last {
            return Err(StoreError::UidRangeFull);
        }
        self.conn
            .prepare_cached(
                "INSERT INTO accounts (username, handle, display_name, role, uid, state, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)",
            )?
            .execute(params![new.username, &handle[..], new.display_name, new.role.as_str(), uid, now])?;
        let id = AccountId(self.conn.last_insert_rowid());
        self.account_get(id)?.ok_or(StoreError::NotFound)
    }

    // ---- bootstrap token ----

    /// Stores the SHA-256 of the bootstrap token, replacing an earlier one. Refused while an account exists.
    pub fn bootstrap_set(&self, token_hash: &TokenHash, expires_at: i64) -> Result<()> {
        if self.account_count()? > 0 {
            return Err(StoreError::Conflict);
        }
        self.conn.execute("DELETE FROM bootstrap", [])?;
        self.conn
            .prepare_cached("INSERT INTO bootstrap (token_hash, expires_at, redeemed_at) VALUES (?1, ?2, NULL)")?
            .execute(params![&token_hash[..], expires_at])?;
        Ok(())
    }

    /// Redeems the bootstrap token, once, and only while no account exists. Call it in the same `write` that creates the
    /// first administrator, so a failure there gives the token back. An unknown, expired or used token and a token redeemed
    /// when an account exists are the same error.
    pub fn bootstrap_redeem(&self, token_hash: &TokenHash, now: i64) -> Result<()> {
        if self.account_count()? > 0 {
            return Err(StoreError::NotRedeemable);
        }
        let changed = self
            .conn
            .prepare_cached(
                "UPDATE bootstrap SET redeemed_at = ?1 \
                 WHERE token_hash = ?2 AND redeemed_at IS NULL AND expires_at > ?1",
            )?
            .execute(params![now, &token_hash[..]])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotRedeemable) }
    }
}

/// Turns "no row" into `None`.
fn optional<T>(found: rusqlite::Result<T>) -> rusqlite::Result<Option<T>> {
    match found {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

// ---- the rows of the other tables ----

/// A password credential: the Argon2 verifier and the salt. The `Debug` text prints a marker instead of the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordCredential {
    /// The stored verifier.
    pub verifier: Vec<u8>,
    /// The per-account salt.
    pub salt: Vec<u8>,
    /// The hashing parameters as text, so that a change of cost is recognised.
    pub params: String,
    /// Unix seconds.
    pub changed_at: i64,
}

impl fmt::Debug for PasswordCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PasswordCredential(..)")
    }
}

/// A TOTP secret, sealed by the caller, and the last time step that was accepted.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpSecret {
    /// The sealed secret.
    pub sealed_secret: Vec<u8>,
    /// The last accepted step; a step is accepted only when it is greater.
    pub last_step: i64,
}

impl fmt::Debug for TotpSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TotpSecret(.., last_step: {})", self.last_step)
    }
}

/// One passkey of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passkey {
    /// The row id.
    pub id: i64,
    /// The account.
    pub account_id: AccountId,
    /// The credential id, unique across accounts.
    pub credential_id: Vec<u8>,
    /// The serialized passkey of the WebAuthn library.
    pub passkey_json: String,
    /// The salt of the PRF extension, when the passkey can unlock the secret store.
    pub prf_salt: Option<Vec<u8>>,
    /// Unix seconds.
    pub created_at: i64,
}

/// A link between an account and an identity of an OpenID Connect provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcIdentity {
    /// The issuer URL.
    pub issuer: String,
    /// The subject at that issuer.
    pub sub: String,
    /// The account.
    pub account_id: AccountId,
}

/// What an invitation is made from.
#[derive(Clone)]
pub struct NewInvite {
    /// The SHA-256 of the token.
    pub token_hash: TokenHash,
    /// The account the invitation is for (a person added again, a new passkey), or `None` for a new person.
    pub account_id: Option<AccountId>,
    /// The role the new person gets.
    pub role: Role,
    /// The administrator who made it.
    pub created_by: AccountId,
    /// Unix seconds after which the token no longer works.
    pub expires_at: i64,
}

/// One invitation. The `Debug` text prints a marker instead of the hash.
#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    /// The row id.
    pub id: i64,
    /// The SHA-256 of the token.
    pub token_hash: TokenHash,
    /// The account the invitation is for, if any.
    pub account_id: Option<AccountId>,
    /// The role.
    pub role: Role,
    /// The administrator who made it.
    pub created_by: AccountId,
    /// Unix seconds.
    pub expires_at: i64,
    /// Unix seconds, once it was redeemed.
    pub redeemed_at: Option<i64>,
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("id", &self.id)
            .field("account_id", &self.account_id)
            .field("role", &self.role)
            .field("created_by", &self.created_by)
            .field("expires_at", &self.expires_at)
            .field("redeemed_at", &self.redeemed_at)
            .finish_non_exhaustive()
    }
}

/// How a secret-store key can be unlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockKind {
    /// The account password.
    Password,
    /// A separate passphrase.
    Passphrase,
    /// A passkey with the PRF extension.
    Prf,
    /// A recovery code.
    Recovery,
}

impl UnlockKind {
    /// The spelling in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            UnlockKind::Password => "password",
            UnlockKind::Passphrase => "passphrase",
            UnlockKind::Prf => "prf",
            UnlockKind::Recovery => "recovery",
        }
    }

    /// The kind of its spelling.
    pub fn parse(text: &str) -> Option<UnlockKind> {
        match text {
            "password" => Some(UnlockKind::Password),
            "passphrase" => Some(UnlockKind::Passphrase),
            "prf" => Some(UnlockKind::Prf),
            "recovery" => Some(UnlockKind::Recovery),
            _ => None,
        }
    }
}

/// One way to unlock the secret store of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockMethod {
    /// The row id; methods are listed in the order of their ids.
    pub id: i64,
    /// The account.
    pub account_id: AccountId,
    /// The kind.
    pub kind: UnlockKind,
    /// The salt of the key derivation.
    pub salt: Vec<u8>,
    /// The derivation parameters as text.
    pub params: String,
    /// The passkey credential, for the `prf` kind.
    pub credential_id: Option<Vec<u8>>,
}

/// How a person signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInMethod {
    /// A password and a TOTP code.
    PasswordTotp,
    /// A passkey.
    Passkey,
    /// An OpenID Connect provider.
    Oidc,
}

impl SignInMethod {
    /// The spelling in the database and in the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            SignInMethod::PasswordTotp => "password_totp",
            SignInMethod::Passkey => "passkey",
            SignInMethod::Oidc => "oidc",
        }
    }

    /// The method of its spelling.
    pub fn parse(text: &str) -> Option<SignInMethod> {
        match text {
            "password_totp" => Some(SignInMethod::PasswordTotp),
            "passkey" => Some(SignInMethod::Passkey),
            "oidc" => Some(SignInMethod::Oidc),
            _ => None,
        }
    }
}

/// The longest device summary that is kept, in characters.
pub const MAX_DEVICE: usize = 64;

/// What a session is made from.
#[derive(Clone)]
pub struct NewSession<'a> {
    /// The SHA-256 of the cookie value.
    pub id_hash: TokenHash,
    /// The account.
    pub account_id: AccountId,
    /// How the person signed in.
    pub method: SignInMethod,
    /// Unix seconds of the sign-in.
    pub now: i64,
    /// Unix seconds after which the session ends whatever happens.
    pub absolute_expires: i64,
    /// A short summary of the browser; control characters are dropped and it is cut to 64 characters.
    pub device: &'a str,
    /// The client address.
    pub addr: std::net::IpAddr,
}

/// One session. The `Debug` text prints a marker instead of the hash.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    /// The SHA-256 of the cookie value.
    pub id_hash: TokenHash,
    /// The account.
    pub account_id: AccountId,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds of the last request.
    pub last_seen: i64,
    /// Unix seconds of the last sign-in or step-up.
    pub last_auth: i64,
    /// How the person signed in.
    pub method: SignInMethod,
    /// Unix seconds after which the session ends whatever happens.
    pub absolute_expires: i64,
    /// The device summary.
    pub device: String,
    /// The client address as text.
    pub addr: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("account_id", &self.account_id)
            .field("created_at", &self.created_at)
            .field("last_seen", &self.last_seen)
            .field("last_auth", &self.last_auth)
            .field("method", &self.method)
            .field("absolute_expires", &self.absolute_expires)
            .field("device", &self.device)
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

/// The numbers of one throttle rule; the callers own the values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleRule {
    /// How long failures are counted, in seconds.
    pub window_secs: i64,
    /// The number of failures in a window that locks the key.
    pub limit: i64,
    /// How long the lock lasts, in seconds.
    pub lock_secs: i64,
}

/// What the throttle knows about a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleState {
    /// Unix seconds the window began (0 when there is no state).
    pub window_start: i64,
    /// Failures counted in the window.
    pub failures: i64,
    /// Unix seconds until which the key is locked (0 when it is not).
    pub locked_until: i64,
}

impl ThrottleState {
    /// Whether the key is locked at `now`.
    pub fn is_locked(&self, now: i64) -> bool {
        self.locked_until > now
    }
}

/// The longest throttle key, in bytes.
pub const MAX_THROTTLE_KEY: usize = 256;

const MAX_CREDENTIAL_ID: usize = 1023;
const MAX_PASSKEY_JSON: usize = 64 * 1024;
const MAX_PRF_SALT: usize = 64;
const MAX_OIDC_ISSUER: usize = 512;
const MAX_OIDC_SUB: usize = 255;
const MAX_PARAMS: usize = 1024;
const MAX_BLOB: usize = 1024;

fn token_hash_from(blob: Vec<u8>) -> rusqlite::Result<TokenHash> {
    blob.try_into().map_err(|_| corrupt_column())
}

fn invite_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Invite> {
    let role: String = row.get(3)?;
    let account_id: Option<i64> = row.get(2)?;
    Ok(Invite {
        id: row.get(0)?,
        token_hash: token_hash_from(row.get(1)?)?,
        account_id: account_id.map(AccountId),
        role: Role::parse(&role).ok_or_else(corrupt_column)?,
        created_by: AccountId(row.get(4)?),
        expires_at: row.get(5)?,
        redeemed_at: row.get(6)?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let method: String = row.get(5)?;
    Ok(Session {
        id_hash: token_hash_from(row.get(0)?)?,
        account_id: AccountId(row.get(1)?),
        created_at: row.get(2)?,
        last_seen: row.get(3)?,
        last_auth: row.get(4)?,
        method: SignInMethod::parse(&method).ok_or_else(corrupt_column)?,
        absolute_expires: row.get(6)?,
        device: row.get(7)?,
        addr: row.get(8)?,
    })
}

fn passkey_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Passkey> {
    Ok(Passkey {
        id: row.get(0)?,
        account_id: AccountId(row.get(1)?),
        credential_id: row.get(2)?,
        passkey_json: row.get(3)?,
        prf_salt: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn unlock_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UnlockMethod> {
    let kind: String = row.get(2)?;
    Ok(UnlockMethod {
        id: row.get(0)?,
        account_id: AccountId(row.get(1)?),
        kind: UnlockKind::parse(&kind).ok_or_else(corrupt_column)?,
        salt: row.get(3)?,
        params: row.get(4)?,
        credential_id: row.get(5)?,
    })
}

/// A cap for a purge, as the number SQLite takes.
fn cap(rows: usize) -> i64 {
    i64::try_from(rows).unwrap_or(i64::MAX)
}

// ---- the queries of the other tables ----

impl Tx<'_> {
    // ---- accounts ----

    /// Every account, deleted ones included, in the order they were made.
    pub fn account_list(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare_cached(select_account!("ORDER BY id"))?;
        Ok(stmt.query_map([], account_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// An account by its WebAuthn handle.
    pub fn account_by_handle(&self, handle: &[u8; HANDLE_BYTES]) -> Result<Option<Account>> {
        let mut stmt = self.conn.prepare_cached(select_account!("WHERE handle = ?1"))?;
        Ok(optional(stmt.query_row(params![&handle[..]], account_from_row))?)
    }

    /// An account by its operating-system user id.
    pub fn account_by_uid(&self, uid: u32) -> Result<Option<Account>> {
        let mut stmt = self.conn.prepare_cached(select_account!("WHERE uid = ?1"))?;
        Ok(optional(stmt.query_row(params![i64::from(uid)], account_from_row))?)
    }

    /// The time of the last sign-in of an account (the creation of its newest session), or `None` when it never signed in or
    /// does not exist.
    pub fn account_last_signin(&self, id: AccountId) -> Result<Option<i64>> {
        let mut stmt = self.conn.prepare_cached("SELECT last_signin FROM accounts WHERE id = ?1")?;
        Ok(optional(stmt.query_row(params![id.0], |r| r.get::<_, Option<i64>>(0)))?.flatten())
    }

    /// The accounts that are not deleted with their last sign-in, in the order they were made, at most `limit` of them.
    pub fn account_list_signins(&self, limit: usize) -> Result<Vec<(Account, Option<i64>)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, username, handle, display_name, role, uid, state, created_at, last_signin \
             FROM accounts WHERE state != 'deleted' ORDER BY id LIMIT ?1",
        )?;
        Ok(stmt
            .query_map(params![cap(limit)], |row| Ok((account_from_row(row)?, row.get::<_, Option<i64>>(8)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// The number of administrators who can sign in.
    pub fn admin_count_active(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM accounts WHERE role = 'admin' AND state = 'active'",
            [],
            |r| r.get(0),
        )?)
    }

    /// Fails with [`StoreError::LastAdmin`] when `id` is the only active administrator. It runs in the transaction of the
    /// change it guards, so two changes cannot both pass.
    fn require_other_admin(&self, id: AccountId) -> Result<()> {
        let others: i64 = self.conn.query_row(
            "SELECT count(*) FROM accounts WHERE role = 'admin' AND state = 'active' AND id != ?1",
            params![id.0],
            |r| r.get(0),
        )?;
        if others == 0 { Err(StoreError::LastAdmin) } else { Ok(()) }
    }

    /// A live account (not deleted) by id, or [`StoreError::NotFound`].
    fn live_account(&self, id: AccountId) -> Result<Account> {
        match self.account_get(id)? {
            Some(account) if account.state != AccountState::Deleted => Ok(account),
            _ => Err(StoreError::NotFound),
        }
    }

    /// Changes the role. Demoting the only active administrator is [`StoreError::LastAdmin`].
    pub fn account_set_role(&self, id: AccountId, role: Role) -> Result<()> {
        let account = self.live_account(id)?;
        if account.role == role {
            return Ok(());
        }
        if account.role == Role::Admin && account.state == AccountState::Active {
            self.require_other_admin(id)?;
        }
        self.conn
            .prepare_cached("UPDATE accounts SET role = ?1 WHERE id = ?2")?
            .execute(params![role.as_str(), id.0])?;
        Ok(())
    }

    /// Enables or disables an account. Disabling the only active administrator is [`StoreError::LastAdmin`]; deleting goes
    /// through [`Tx::account_delete`].
    pub fn account_set_state(&self, id: AccountId, state: AccountState) -> Result<()> {
        if state == AccountState::Deleted {
            return Err(StoreError::Invalid("an account is deleted with account_delete"));
        }
        let account = self.live_account(id)?;
        if account.state == state {
            return Ok(());
        }
        if account.role == Role::Admin && account.state == AccountState::Active {
            self.require_other_admin(id)?;
        }
        self.conn
            .prepare_cached("UPDATE accounts SET state = ?1 WHERE id = ?2")?
            .execute(params![state.as_str(), id.0])?;
        Ok(())
    }

    /// Changes the name shown to people.
    pub fn account_set_display_name(&self, id: AccountId, display_name: &str) -> Result<()> {
        if !valid_display_name(display_name) {
            return Err(StoreError::Invalid("a display name is 1 to 128 characters without control characters"));
        }
        self.live_account(id)?;
        self.conn
            .prepare_cached("UPDATE accounts SET display_name = ?1 WHERE id = ?2")?
            .execute(params![display_name, id.0])?;
        Ok(())
    }

    /// Deletes an account: its password, TOTP secret, passkeys, identities, unlock methods, sessions and invitations go; the
    /// row stays with state `deleted`, so its user id, username and handle are never given to anyone else. Deleting the only
    /// active administrator is [`StoreError::LastAdmin`]. Unredeemed invitations the account made go too.
    pub fn account_delete(&self, id: AccountId) -> Result<()> {
        let account = self.live_account(id)?;
        if account.role == Role::Admin && account.state == AccountState::Active {
            self.require_other_admin(id)?;
        }
        for sql in [
            "DELETE FROM password_credentials WHERE account_id = ?1",
            "DELETE FROM totp WHERE account_id = ?1",
            "DELETE FROM passkeys WHERE account_id = ?1",
            "DELETE FROM oidc_identities WHERE account_id = ?1",
            "DELETE FROM unlock_methods WHERE account_id = ?1",
            "DELETE FROM sessions WHERE account_id = ?1",
            "DELETE FROM invites WHERE account_id = ?1 OR (created_by = ?1 AND redeemed_at IS NULL)",
            "UPDATE accounts SET state = 'deleted' WHERE id = ?1",
        ] {
            self.conn.prepare_cached(sql)?.execute(params![id.0])?;
        }
        Ok(())
    }

    // ---- passwords ----

    /// Stores the password credential of an account, replacing an earlier one.
    pub fn password_set(&self, id: AccountId, credential: &PasswordCredential) -> Result<()> {
        if credential.verifier.is_empty()
            || credential.verifier.len() > MAX_BLOB
            || credential.salt.is_empty()
            || credential.salt.len() > MAX_BLOB
            || credential.params.len() > MAX_PARAMS
        {
            return Err(StoreError::Invalid("the password credential has a wrong size"));
        }
        self.live_account(id)?;
        self.conn
            .prepare_cached(
                "INSERT INTO password_credentials (account_id, verifier, salt, params, changed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (account_id) DO UPDATE SET \
                 verifier = excluded.verifier, salt = excluded.salt, params = excluded.params, changed_at = excluded.changed_at",
            )?
            .execute(params![id.0, credential.verifier, credential.salt, credential.params, credential.changed_at])?;
        Ok(())
    }

    /// The password credential of an account.
    pub fn password_get(&self, id: AccountId) -> Result<Option<PasswordCredential>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT verifier, salt, params, changed_at FROM password_credentials WHERE account_id = ?1",
        )?;
        Ok(optional(stmt.query_row(params![id.0], |r| {
            Ok(PasswordCredential { verifier: r.get(0)?, salt: r.get(1)?, params: r.get(2)?, changed_at: r.get(3)? })
        }))?)
    }

    /// Removes the password credential; answers whether there was one.
    pub fn password_delete(&self, id: AccountId) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached("DELETE FROM password_credentials WHERE account_id = ?1")?
            .execute(params![id.0])?;
        Ok(changed > 0)
    }

    // ---- TOTP ----

    /// Stores the sealed TOTP secret of an account, replacing an earlier one and starting the step counter again.
    pub fn totp_set(&self, id: AccountId, sealed_secret: &[u8]) -> Result<()> {
        if sealed_secret.is_empty() || sealed_secret.len() > MAX_BLOB {
            return Err(StoreError::Invalid("the sealed TOTP secret has a wrong size"));
        }
        self.live_account(id)?;
        self.conn
            .prepare_cached(
                "INSERT INTO totp (account_id, sealed_secret, last_step) VALUES (?1, ?2, 0) \
                 ON CONFLICT (account_id) DO UPDATE SET sealed_secret = excluded.sealed_secret, last_step = 0",
            )?
            .execute(params![id.0, sealed_secret])?;
        Ok(())
    }

    /// The sealed TOTP secret and the last accepted step.
    pub fn totp_get(&self, id: AccountId) -> Result<Option<TotpSecret>> {
        let mut stmt =
            self.conn.prepare_cached("SELECT sealed_secret, last_step FROM totp WHERE account_id = ?1")?;
        Ok(optional(stmt.query_row(params![id.0], |r| {
            Ok(TotpSecret { sealed_secret: r.get(0)?, last_step: r.get(1)? })
        }))?)
    }

    /// Accepts a time step if it is greater than the last one accepted and answers whether it did. One conditional update,
    /// so of several calls with the same step exactly one answers `true`; this is what stops a replayed code.
    pub fn totp_accept_step(&self, id: AccountId, step: i64) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached("UPDATE totp SET last_step = ?1 WHERE account_id = ?2 AND last_step < ?1")?
            .execute(params![step, id.0])?;
        Ok(changed == 1)
    }

    /// Removes the TOTP secret; answers whether there was one.
    pub fn totp_delete(&self, id: AccountId) -> Result<bool> {
        let changed = self.conn.prepare_cached("DELETE FROM totp WHERE account_id = ?1")?.execute(params![id.0])?;
        Ok(changed > 0)
    }

    // ---- passkeys ----

    /// The number of passkeys of an account.
    pub fn passkey_count(&self, id: AccountId) -> Result<usize> {
        let count: i64 =
            self.conn.query_row("SELECT count(*) FROM passkeys WHERE account_id = ?1", params![id.0], |r| r.get(0))?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    /// Adds a passkey. An account has at most [`MAX_PASSKEYS`]; a credential id that exists anywhere is a conflict.
    pub fn passkey_add(
        &self,
        id: AccountId,
        credential_id: &[u8],
        passkey_json: &str,
        prf_salt: Option<&[u8]>,
        now: i64,
    ) -> Result<Passkey> {
        if credential_id.is_empty()
            || credential_id.len() > MAX_CREDENTIAL_ID
            || passkey_json.len() > MAX_PASSKEY_JSON
            || prf_salt.is_some_and(|s| s.is_empty() || s.len() > MAX_PRF_SALT)
        {
            return Err(StoreError::Invalid("the passkey has a wrong size"));
        }
        self.live_account(id)?;
        if self.passkey_count(id)? >= MAX_PASSKEYS {
            return Err(StoreError::PasskeyLimit);
        }
        self.conn
            .prepare_cached(
                "INSERT INTO passkeys (account_id, credential_id, passkey_json, prf_salt, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![id.0, credential_id, passkey_json, prf_salt, now])?;
        let row = self.conn.last_insert_rowid();
        Ok(Passkey {
            id: row,
            account_id: id,
            credential_id: credential_id.to_vec(),
            passkey_json: passkey_json.to_owned(),
            prf_salt: prf_salt.map(<[u8]>::to_vec),
            created_at: now,
        })
    }

    /// The passkeys of an account, oldest first.
    pub fn passkey_list(&self, id: AccountId) -> Result<Vec<Passkey>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, account_id, credential_id, passkey_json, prf_salt, created_at \
             FROM passkeys WHERE account_id = ?1 ORDER BY id",
        )?;
        Ok(stmt.query_map(params![id.0], passkey_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// The passkey with a credential id, whoever owns it.
    pub fn passkey_by_credential(&self, credential_id: &[u8]) -> Result<Option<Passkey>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, account_id, credential_id, passkey_json, prf_salt, created_at \
             FROM passkeys WHERE credential_id = ?1",
        )?;
        Ok(optional(stmt.query_row(params![credential_id], passkey_from_row))?)
    }

    /// Replaces the serialized passkey of a row (its signature counter moved) for the owning account.
    pub fn passkey_update_json(&self, id: AccountId, passkey_id: i64, passkey_json: &str) -> Result<()> {
        if passkey_json.len() > MAX_PASSKEY_JSON {
            return Err(StoreError::Invalid("the passkey has a wrong size"));
        }
        let changed = self
            .conn
            .prepare_cached("UPDATE passkeys SET passkey_json = ?1 WHERE id = ?2 AND account_id = ?3")?
            .execute(params![passkey_json, passkey_id, id.0])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    /// Removes a passkey of the owning account; another account's id removes nothing.
    pub fn passkey_remove(&self, id: AccountId, passkey_id: i64) -> Result<()> {
        let changed = self
            .conn
            .prepare_cached("DELETE FROM passkeys WHERE id = ?1 AND account_id = ?2")?
            .execute(params![passkey_id, id.0])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    // ---- OpenID Connect identities ----

    /// Links an identity of a provider to an account. An identity belongs to one account only.
    pub fn oidc_link(&self, id: AccountId, issuer: &str, sub: &str) -> Result<()> {
        let plain = |s: &str| !s.is_empty() && !s.chars().any(char::is_control);
        if !plain(issuer) || issuer.len() > MAX_OIDC_ISSUER || !plain(sub) || sub.len() > MAX_OIDC_SUB {
            return Err(StoreError::Invalid("the identity has a wrong shape"));
        }
        self.live_account(id)?;
        self.conn
            .prepare_cached("INSERT INTO oidc_identities (issuer, sub, account_id) VALUES (?1, ?2, ?3)")?
            .execute(params![issuer, sub, id.0])?;
        Ok(())
    }

    /// The account an identity is linked to.
    pub fn oidc_find(&self, issuer: &str, sub: &str) -> Result<Option<AccountId>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT account_id FROM oidc_identities WHERE issuer = ?1 AND sub = ?2")?;
        Ok(optional(stmt.query_row(params![issuer, sub], |r| r.get::<_, i64>(0).map(AccountId)))?)
    }

    /// The identities linked to an account.
    pub fn oidc_list(&self, id: AccountId) -> Result<Vec<OidcIdentity>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT issuer, sub, account_id FROM oidc_identities WHERE account_id = ?1 ORDER BY rowid",
        )?;
        Ok(stmt
            .query_map(params![id.0], |r| {
                Ok(OidcIdentity { issuer: r.get(0)?, sub: r.get(1)?, account_id: AccountId(r.get(2)?) })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    /// Removes the link of an identity from its account.
    pub fn oidc_unlink(&self, id: AccountId, issuer: &str, sub: &str) -> Result<()> {
        let changed = self
            .conn
            .prepare_cached("DELETE FROM oidc_identities WHERE issuer = ?1 AND sub = ?2 AND account_id = ?3")?
            .execute(params![issuer, sub, id.0])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    // ---- invitations ----

    /// Stores an invitation by the hash of its token.
    pub fn invite_create(&self, new: &NewInvite) -> Result<Invite> {
        self.live_account(new.created_by)?;
        if let Some(account) = new.account_id {
            self.live_account(account)?;
        }
        self.conn
            .prepare_cached(
                "INSERT INTO invites (token_hash, account_id, role, created_by, expires_at, redeemed_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            )?
            .execute(params![&new.token_hash[..], new.account_id.map(|a| a.0), new.role.as_str(), new.created_by.0, new.expires_at])?;
        Ok(Invite {
            id: self.conn.last_insert_rowid(),
            token_hash: new.token_hash,
            account_id: new.account_id,
            role: new.role,
            created_by: new.created_by,
            expires_at: new.expires_at,
            redeemed_at: None,
        })
    }

    /// Every invitation, newest first.
    pub fn invite_list(&self) -> Result<Vec<Invite>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, token_hash, account_id, role, created_by, expires_at, redeemed_at FROM invites ORDER BY id DESC",
        )?;
        Ok(stmt.query_map([], invite_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// How many invitations are open at `now`: not redeemed and not expired.
    pub fn invite_open_count(&self, now: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM invites WHERE redeemed_at IS NULL AND expires_at > ?1",
            params![now],
            |r| r.get(0),
        )?)
    }

    /// Withdraws an invitation that was not redeemed.
    pub fn invite_revoke(&self, invite_id: i64) -> Result<()> {
        let changed = self
            .conn
            .prepare_cached("DELETE FROM invites WHERE id = ?1 AND redeemed_at IS NULL")?
            .execute(params![invite_id])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    /// Redeems an invitation, once. One conditional update with `RETURNING`: of several callers with the same token exactly
    /// one gets the row. An unknown, an expired and a redeemed token are the same [`StoreError::NotRedeemable`].
    pub fn invite_redeem(&self, token_hash: &TokenHash, now: i64) -> Result<Invite> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE invites SET redeemed_at = ?1 \
             WHERE token_hash = ?2 AND redeemed_at IS NULL AND expires_at > ?1 \
             RETURNING id, token_hash, account_id, role, created_by, expires_at, redeemed_at",
        )?;
        match stmt.query_row(params![now, &token_hash[..]], invite_from_row) {
            Ok(invite) => Ok(invite),
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(StoreError::NotRedeemable),
            Err(e) => Err(e.into()),
        }
    }

    // ---- unlock methods ----

    /// Adds a way to unlock the secret store of an account.
    pub fn unlock_add(
        &self,
        id: AccountId,
        kind: UnlockKind,
        salt: &[u8],
        params_text: &str,
        credential_id: Option<&[u8]>,
    ) -> Result<UnlockMethod> {
        if salt.is_empty()
            || salt.len() > MAX_BLOB
            || params_text.len() > MAX_PARAMS
            || credential_id.is_some_and(|c| c.is_empty() || c.len() > MAX_CREDENTIAL_ID)
        {
            return Err(StoreError::Invalid("the unlock method has a wrong size"));
        }
        self.live_account(id)?;
        self.conn
            .prepare_cached(
                "INSERT INTO unlock_methods (account_id, kind, salt, params, credential_id) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![id.0, kind.as_str(), salt, params_text, credential_id])?;
        Ok(UnlockMethod {
            id: self.conn.last_insert_rowid(),
            account_id: id,
            kind,
            salt: salt.to_vec(),
            params: params_text.to_owned(),
            credential_id: credential_id.map(<[u8]>::to_vec),
        })
    }

    /// The unlock methods of an account, in the order they were added.
    pub fn unlock_list(&self, id: AccountId) -> Result<Vec<UnlockMethod>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT rowid, account_id, kind, salt, params, credential_id FROM unlock_methods \
             WHERE account_id = ?1 ORDER BY rowid",
        )?;
        Ok(stmt.query_map(params![id.0], unlock_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Removes an unlock method of the owning account.
    pub fn unlock_remove(&self, id: AccountId, method_id: i64) -> Result<()> {
        let changed = self
            .conn
            .prepare_cached("DELETE FROM unlock_methods WHERE rowid = ?1 AND account_id = ?2")?
            .execute(params![method_id, id.0])?;
        if changed == 1 { Ok(()) } else { Err(StoreError::NotFound) }
    }

    // ---- sessions ----

    /// Stores a session by the hash of its cookie value.
    pub fn session_create(&self, new: &NewSession<'_>) -> Result<Session> {
        self.live_account(new.account_id)?;
        let device: String = new.device.chars().filter(|c| !c.is_control()).take(MAX_DEVICE).collect();
        let addr = new.addr.to_string();
        self.conn
            .prepare_cached(
                "INSERT INTO sessions (id_hash, account_id, created_at, last_seen, last_auth, method, absolute_expires, device, addr) \
                 VALUES (?1, ?2, ?3, ?3, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![&new.id_hash[..], new.account_id.0, new.now, new.method.as_str(), new.absolute_expires, device, addr])?;
        // The newest session names the last sign-in. The stamp is written with the row, so a creation that fails leaves it as
        // it was, and it moves forward only.
        self.conn
            .prepare_cached("UPDATE accounts SET last_signin = max(coalesce(last_signin, 0), ?1) WHERE id = ?2")?
            .execute(params![new.now, new.account_id.0])?;
        Ok(Session {
            id_hash: new.id_hash,
            account_id: new.account_id,
            created_at: new.now,
            last_seen: new.now,
            last_auth: new.now,
            method: new.method,
            absolute_expires: new.absolute_expires,
            device,
            addr,
        })
    }

    /// A session by the hash of its cookie value.
    pub fn session_get(&self, id_hash: &TokenHash) -> Result<Option<Session>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id_hash, account_id, created_at, last_seen, last_auth, method, absolute_expires, device, addr \
             FROM sessions WHERE id_hash = ?1",
        )?;
        Ok(optional(stmt.query_row(params![&id_hash[..]], session_from_row))?)
    }

    /// The sessions of an account, oldest first.
    pub fn session_list(&self, id: AccountId) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id_hash, account_id, created_at, last_seen, last_auth, method, absolute_expires, device, addr \
             FROM sessions WHERE account_id = ?1 ORDER BY created_at, rowid",
        )?;
        Ok(stmt.query_map(params![id.0], session_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Moves `last_seen` forward to `now`; it never moves back. Answers whether the session exists.
    pub fn session_touch(&self, id_hash: &TokenHash, now: i64) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached("UPDATE sessions SET last_seen = max(last_seen, ?1) WHERE id_hash = ?2")?
            .execute(params![now, &id_hash[..]])?;
        Ok(changed == 1)
    }

    /// Records a fresh sign-in check (a step-up) at `now`. Answers whether the session exists.
    pub fn session_set_last_auth(&self, id_hash: &TokenHash, now: i64) -> Result<bool> {
        let changed = self
            .conn
            .prepare_cached("UPDATE sessions SET last_auth = max(last_auth, ?1) WHERE id_hash = ?2")?
            .execute(params![now, &id_hash[..]])?;
        Ok(changed == 1)
    }

    /// Ends one session; answers whether it existed.
    pub fn session_delete(&self, id_hash: &TokenHash) -> Result<bool> {
        let changed =
            self.conn.prepare_cached("DELETE FROM sessions WHERE id_hash = ?1")?.execute(params![&id_hash[..]])?;
        Ok(changed == 1)
    }

    /// Ends every session of an account; answers how many there were.
    pub fn session_delete_all(&self, id: AccountId) -> Result<usize> {
        Ok(self.conn.prepare_cached("DELETE FROM sessions WHERE account_id = ?1")?.execute(params![id.0])?)
    }

    /// Removes sessions whose absolute expiry has passed or that were idle for `idle_secs`, at most `max_rows` of them.
    /// Answers how many it removed; a caller that gets `max_rows` back calls again.
    pub fn sessions_purge(&self, now: i64, idle_secs: i64, max_rows: usize) -> Result<usize> {
        Ok(self
            .conn
            .prepare_cached(
                "DELETE FROM sessions WHERE rowid IN (\
                 SELECT rowid FROM sessions WHERE absolute_expires <= ?1 OR last_seen + ?2 <= ?1 LIMIT ?3)",
            )?
            .execute(params![now, idle_secs, cap(max_rows)])?)
    }

    // ---- throttle ----

    /// Counts a failure for `key` in one atomic upsert and answers the state after it. A window that has ended (and is not
    /// locked) starts again at one failure; reaching `rule.limit` failures sets `locked_until` to `now + rule.lock_secs`,
    /// and a failure during a lock counts but does not move the end of the lock.
    pub fn throttle_fail(&self, key: &str, now: i64, rule: &ThrottleRule) -> Result<ThrottleState> {
        if key.is_empty() || key.len() > MAX_THROTTLE_KEY || rule.limit < 1 || rule.window_secs < 1 || rule.lock_secs < 1 {
            return Err(StoreError::Invalid("the throttle key or rule has a wrong shape"));
        }
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO throttle (key, window_start, failures, locked_until) \
             VALUES (?1, ?2, 1, CASE WHEN 1 >= ?4 THEN ?2 + ?5 ELSE 0 END) \
             ON CONFLICT (key) DO UPDATE SET \
               window_start = CASE WHEN throttle.window_start + ?3 <= ?2 AND throttle.locked_until <= ?2 \
                                   THEN ?2 ELSE throttle.window_start END, \
               failures = CASE WHEN throttle.window_start + ?3 <= ?2 AND throttle.locked_until <= ?2 \
                               THEN 1 ELSE throttle.failures + 1 END, \
               locked_until = CASE \
                 WHEN (CASE WHEN throttle.window_start + ?3 <= ?2 AND throttle.locked_until <= ?2 \
                            THEN 1 ELSE throttle.failures + 1 END) >= ?4 \
                   THEN (CASE WHEN throttle.locked_until > ?2 THEN throttle.locked_until ELSE ?2 + ?5 END) \
                 ELSE (CASE WHEN throttle.window_start + ?3 <= ?2 AND throttle.locked_until <= ?2 \
                            THEN 0 ELSE throttle.locked_until END) \
               END \
             RETURNING window_start, failures, locked_until",
        )?;
        Ok(stmt.query_row(params![key, now, rule.window_secs, rule.limit, rule.lock_secs], |r| {
            Ok(ThrottleState { window_start: r.get(0)?, failures: r.get(1)?, locked_until: r.get(2)? })
        })?)
    }

    /// The state of `key` at `now`: all zero when nothing is counted, the window has ended and no lock is left.
    pub fn throttle_get(&self, key: &str, now: i64, window_secs: i64) -> Result<ThrottleState> {
        let none = ThrottleState { window_start: 0, failures: 0, locked_until: 0 };
        let mut stmt = self
            .conn
            .prepare_cached("SELECT window_start, failures, locked_until FROM throttle WHERE key = ?1")?;
        let found = optional(stmt.query_row(params![key], |r| {
            Ok(ThrottleState { window_start: r.get(0)?, failures: r.get(1)?, locked_until: r.get(2)? })
        }))?;
        Ok(match found {
            Some(state) if state.window_start + window_secs > now || state.locked_until > now => state,
            _ => none,
        })
    }

    /// Forgets `key` (a successful sign-in); answers whether there was state.
    pub fn throttle_clear(&self, key: &str) -> Result<bool> {
        let changed = self.conn.prepare_cached("DELETE FROM throttle WHERE key = ?1")?.execute(params![key])?;
        Ok(changed > 0)
    }

    /// Removes throttle state whose window ended and that is not locked, at most `max_rows` of it.
    pub fn throttle_purge(&self, now: i64, window_secs: i64, max_rows: usize) -> Result<usize> {
        Ok(self
            .conn
            .prepare_cached(
                "DELETE FROM throttle WHERE key IN (\
                 SELECT key FROM throttle WHERE window_start + ?1 <= ?2 AND locked_until <= ?2 LIMIT ?3)",
            )?
            .execute(params![window_secs, now, cap(max_rows)])?)
    }

    /// Removes invitations and bootstrap tokens that expired unused, at most `max_rows` of them in all.
    pub fn tokens_purge(&self, now: i64, max_rows: usize) -> Result<usize> {
        let first = self
            .conn
            .prepare_cached(
                "DELETE FROM invites WHERE id IN (\
                 SELECT id FROM invites WHERE redeemed_at IS NULL AND expires_at <= ?1 LIMIT ?2)",
            )?
            .execute(params![now, cap(max_rows)])?;
        let left = max_rows.saturating_sub(first);
        if left == 0 {
            return Ok(first);
        }
        let second = self
            .conn
            .prepare_cached(
                "DELETE FROM bootstrap WHERE rowid IN (\
                 SELECT rowid FROM bootstrap WHERE redeemed_at IS NULL AND expires_at <= ?1 LIMIT ?2)",
            )?
            .execute(params![now, cap(left)])?;
        Ok(first + second)
    }
}

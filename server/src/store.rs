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
pub const SCHEMA_VERSION: i64 = 1;

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
const MIGRATIONS: [fn(&Connection) -> rusqlite::Result<()>; SCHEMA_VERSION as usize] = [migrate_to_1];

fn migrate_to_1(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_V1)?;
    conn.pragma_update(None, "application_id", APPLICATION_ID)
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

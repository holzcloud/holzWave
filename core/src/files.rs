//! Files, local and over SFTP: what the file browser and the previews read, and the editor writes.
//!
//! [`FileSource`] is the one interface. [`LocalSource`] is this machine's files; [`SftpSource`] is the files of a
//! saved SSH connection over SFTP on a connection of its own (so a file read never waits behind a terminal's output
//! and a terminal never waits behind a file). Everything is bounded: a listing holds at most [`MAX_ENTRIES`] entries,
//! a read at most the limit it is given, a path at most [`MAX_PATH`] bytes, and nothing is ever executed or
//! interpreted here.

use std::cmp::Ordering;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use russh::client::Handle;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileType, OpenFlags};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::ssh::{Client, SshTarget, open_session};
use crate::{Error, Result};

/// The most entries a listing holds; the rest is cut and the listing says so.
pub const MAX_ENTRIES: usize = 5000;
/// The longest path accepted.
pub const MAX_PATH: usize = 4096;
/// The most bytes one read returns unless the caller asks for less.
pub const MAX_READ: u64 = 256 * 1024 * 1024;
/// How long one remote request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Dir,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    /// The entry is a symbolic link; `kind` is what it points to.
    pub link: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Listing {
    /// The directory as the source resolved it (absolute).
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    /// More entries than [`MAX_ENTRIES`] existed; the rest is not shown.
    pub truncated: bool,
}

pub trait FileSource: Send + Sync {
    /// A name for the source in the UI: `This computer`, or the connection's name.
    fn label(&self) -> String;
    /// The directory a browser starts in.
    fn home(&self) -> Result<String>;
    fn list(&self, path: &str) -> Result<Listing>;
    /// The entry at `path` (following links).
    fn stat(&self, path: &str) -> Result<Entry>;
    /// The first `limit` bytes of the file; an error when the file is larger and `limit` is the caller's cap.
    fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>>;
    /// `len` bytes from `offset`, fewer at the end of the file. For video and large files read in pieces.
    fn read_range(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>>;
    /// Replace the file with `bytes`: written aside and renamed over it, so a failure leaves the old file.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<()>;
}

fn bad(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > MAX_PATH || path.contains('\0') {
        return Err(Error::Unsupported("the path is empty, too long or has a NUL".into()));
    }
    Ok(())
}

fn too_large(size: u64, limit: u64) -> Error {
    Error::Unsupported(format!("the file is {size} bytes, more than the {limit} allowed here"))
}

/// Folders first, then by name without regard to case; the order a file list is read in.
fn order(a: &Entry, b: &Entry) -> Ordering {
    (b.kind == Kind::Dir).cmp(&(a.kind == Kind::Dir)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name))
}

fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(trimmed[..i].to_owned()),
        None => None,
    }
}

fn secs(time: SystemTime) -> Option<i64> {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok(),
        Err(e) => i64::try_from(e.duration().as_secs()).ok().map(|s| -s),
    }
}

// ---- this computer -----------------------------------------------------------------------------------------------

/// This machine's files.
pub struct LocalSource {
    home: PathBuf,
}

impl LocalSource {
    pub fn new(home: Option<PathBuf>) -> Self {
        Self { home: home.unwrap_or_else(|| PathBuf::from("/")) }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf> {
        bad(path)?;
        let p = match path.strip_prefix("~/").or_else(|| (path == "~").then_some("")) {
            Some(rest) => self.home.join(rest),
            None => PathBuf::from(path),
        };
        Ok(p)
    }

    fn entry_of(name: String, path: &Path) -> Option<Entry> {
        let link_meta = fs::symlink_metadata(path).ok()?;
        let link = link_meta.file_type().is_symlink();
        // A link is shown as what it points to; a broken link as an other entry.
        let meta = if link { fs::metadata(path).ok() } else { Some(link_meta) };
        let (kind, size, modified) = match &meta {
            Some(m) if m.is_dir() => (Kind::Dir, 0, m.modified().ok().and_then(secs)),
            Some(m) if m.is_file() => (Kind::File, m.len(), m.modified().ok().and_then(secs)),
            Some(m) => (Kind::Other, 0, m.modified().ok().and_then(secs)),
            None => (Kind::Other, 0, None),
        };
        let hidden = name.starts_with('.');
        Some(Entry { name, kind, link, size, modified, hidden })
    }
}

impl FileSource for LocalSource {
    fn label(&self) -> String {
        "This computer".into()
    }

    fn home(&self) -> Result<String> {
        Ok(self.home.to_string_lossy().into_owned())
    }

    fn list(&self, path: &str) -> Result<Listing> {
        let dir = fs::canonicalize(self.resolve(path)?)?;
        let mut entries = Vec::new();
        let mut truncated = false;
        for item in fs::read_dir(&dir)? {
            let Ok(item) = item else { continue };
            if entries.len() >= MAX_ENTRIES {
                truncated = true;
                break;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if let Some(entry) = Self::entry_of(name, &item.path()) {
                entries.push(entry);
            }
        }
        entries.sort_by(order);
        let path = dir.to_string_lossy().into_owned();
        Ok(Listing { parent: parent_of(&path), path, entries, truncated })
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        let p = self.resolve(path)?;
        let name = p.file_name().map_or_else(|| p.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        Self::entry_of(name, &p).ok_or_else(|| Error::Unsupported(format!("no such file: {path}")))
    }

    fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        let limit = limit.min(MAX_READ);
        let p = self.resolve(path)?;
        let file = fs::File::open(&p)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(Error::Unsupported("not a regular file".into()));
        }
        if meta.len() > limit {
            return Err(too_large(meta.len(), limit));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
        // `take` keeps a file that grows while it is read inside the limit.
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(too_large(bytes.len() as u64, limit));
        }
        Ok(bytes)
    }

    fn read_range(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let len = len.min(MAX_READ);
        let mut file = fs::File::open(self.resolve(path)?)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(len).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let p = self.resolve(path)?;
        // The new content is written aside and renamed over the old file; the permissions of the old file are kept.
        let permissions = fs::metadata(&p).ok().map(|m| m.permissions());
        crate::persist::write_atomic(&p, bytes)?;
        if let Some(permissions) = permissions {
            let _ = fs::set_permissions(&p, permissions);
        }
        Ok(())
    }
}

// ---- SFTP --------------------------------------------------------------------------------------------------------

struct Conn {
    /// Kept so the connection lives as long as the SFTP session on it.
    _handle: Handle<Client>,
    sftp: SftpSession,
}

/// The files of an SSH connection, over SFTP, on a connection of its own.
pub struct SftpSource {
    label: String,
    target: SshTarget,
    runtime: tokio::runtime::Runtime,
    conn: Mutex<Option<std::sync::Arc<Conn>>>,
}

impl SftpSource {
    /// Prepare a source for `target`; nothing connects until the first request, and a lost connection is made again by
    /// the next request.
    pub fn new(label: impl Into<String>, target: SshTarget) -> Result<Self> {
        // One worker thread drives the SSH and SFTP tasks between requests (keepalives, replies); the calling threads
        // only wait on the result.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .thread_name("hw-sftp")
            .build()
            .map_err(|e| Error::Pty(format!("sftp runtime: {e}")))?;
        Ok(Self { label: label.into(), target, runtime, conn: Mutex::new(None) })
    }

    fn connection(&self) -> Result<std::sync::Arc<Conn>> {
        if let Some(conn) = self.conn.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            return Ok(conn);
        }
        let made = self.runtime.block_on(async {
            let handle = tokio::time::timeout(Duration::from_secs(20), open_session(&self.target))
                .await
                .map_err(|_| "the connection timed out".to_string())??;
            let channel = handle.channel_open_session().await.map_err(|e| e.to_string())?;
            channel.request_subsystem(true, "sftp").await.map_err(|e| e.to_string())?;
            let sftp = SftpSession::new(channel.into_stream()).await.map_err(|e| e.to_string())?;
            Ok::<_, String>(std::sync::Arc::new(Conn { _handle: handle, sftp }))
        });
        let conn = made.map_err(|e| Error::Unsupported(format!("sftp: {e}")))?;
        *self.conn.lock().unwrap_or_else(PoisonError::into_inner) = Some(conn.clone());
        Ok(conn)
    }

    /// Run one request. A request that fails because the connection is gone is tried once more on a new connection.
    fn run<T, F, Fut>(&self, f: F) -> Result<T>
    where
        F: Fn(std::sync::Arc<Conn>) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, russh_sftp::client::error::Error>>,
    {
        for attempt in 0..2 {
            let conn = self.connection()?;
            let outcome = self.runtime.block_on(async { tokio::time::timeout(REQUEST_TIMEOUT, f(conn)).await });
            match outcome {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(e)) if attempt == 0 && is_connection_error(&e) => {
                    *self.conn.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
                Ok(Err(e)) => return Err(Error::Unsupported(format!("sftp: {e}"))),
                Err(_) => {
                    *self.conn.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    return Err(Error::Unsupported("sftp: the request timed out".into()));
                }
            }
        }
        Err(Error::Unsupported("sftp: the connection could not be made again".into()))
    }
}

fn is_connection_error(e: &russh_sftp::client::error::Error) -> bool {
    use russh_sftp::client::error::Error as E;
    matches!(e, E::IO(_) | E::Timeout | E::UnexpectedBehavior(_))
}

impl FileSource for SftpSource {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn home(&self) -> Result<String> {
        self.run(|c| async move { c.sftp.canonicalize(".").await })
    }

    fn list(&self, path: &str) -> Result<Listing> {
        bad(path)?;
        let path = path.to_owned();
        let (dir, mut entries, truncated) = self.run(|c| {
            let path = path.clone();
            async move {
                let dir = c.sftp.canonicalize(path).await?;
                let mut entries = Vec::new();
                let mut truncated = false;
                for item in c.sftp.read_dir(dir.clone()).await? {
                    if entries.len() >= MAX_ENTRIES {
                        truncated = true;
                        break;
                    }
                    let meta = item.metadata();
                    let name = item.file_name();
                    let link = item.file_type() == FileType::Symlink;
                    let (kind, size, modified) = if link {
                        // A link shows what it points to; one that points nowhere is an other entry.
                        match c.sftp.metadata(item.path()).await {
                            Ok(target) => (kind_of(target.file_type()), target.len(), target.modified().ok().and_then(secs)),
                            Err(_) => (Kind::Other, 0, None),
                        }
                    } else {
                        (kind_of(item.file_type()), meta.len(), meta.modified().ok().and_then(secs))
                    };
                    let hidden = name.starts_with('.');
                    entries.push(Entry { name, kind, link, size: if kind == Kind::Dir { 0 } else { size }, modified, hidden });
                }
                Ok((dir, entries, truncated))
            }
        })?;
        entries.sort_by(order);
        Ok(Listing { parent: parent_of(&dir), path: dir, entries, truncated })
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        bad(path)?;
        let path = path.to_owned();
        let (name, meta) = self.run(|c| {
            let path = path.clone();
            async move {
                let meta = c.sftp.metadata(path.clone()).await?;
                Ok((path, meta))
            }
        })?;
        let base = name.rsplit('/').next().unwrap_or(&name).to_owned();
        Ok(Entry {
            hidden: base.starts_with('.'),
            kind: kind_of(meta.file_type()),
            link: false,
            size: meta.len(),
            modified: meta.modified().ok().and_then(secs),
            name: base,
        })
    }

    fn read(&self, path: &str, limit: u64) -> Result<Vec<u8>> {
        bad(path)?;
        let limit = limit.min(MAX_READ);
        let path = path.to_owned();
        self.run(|c| {
            let path = path.clone();
            async move {
                let mut file = c.sftp.open(path).await?;
                let size = file.metadata().await?.len();
                if size > limit {
                    return Ok(Err(too_large(size, limit)));
                }
                let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
                // `take` keeps a file that grows while it is read inside the limit.
                (&mut file).take(limit + 1).read_to_end(&mut bytes).await?;
                if bytes.len() as u64 > limit {
                    return Ok(Err(too_large(bytes.len() as u64, limit)));
                }
                Ok(Ok(bytes))
            }
        })?
    }

    fn read_range(&self, path: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        bad(path)?;
        let len = len.min(MAX_READ);
        let path = path.to_owned();
        self.run(|c| {
            let path = path.clone();
            async move {
                let mut file = c.sftp.open(path).await?;
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                let mut bytes = Vec::new();
                (&mut file).take(len).read_to_end(&mut bytes).await?;
                Ok(bytes)
            }
        })
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        bad(path)?;
        let path = path.to_owned();
        let aside = format!("{path}.holzwave-tmp");
        self.run(|c| {
            let (path, aside) = (path.clone(), aside.clone());
            async move {
                let mode = c.sftp.metadata(path.clone()).await.ok();
                let mut file = c.sftp.open_with_flags(aside.clone(), OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE).await?;
                file.write_all(bytes).await?;
                file.flush().await?;
                file.shutdown().await?;
                if let Some(meta) = mode {
                    // Keep the permissions of the file that is replaced.
                    let mut keep = russh_sftp::protocol::FileAttributes::empty();
                    keep.permissions = meta.permissions;
                    let _ = c.sftp.set_metadata(aside.clone(), keep).await;
                }
                if c.sftp.rename(aside.clone(), path.clone()).await.is_err() {
                    // A server that does not rename over an existing file: remove it, then rename.
                    c.sftp.remove_file(path.clone()).await?;
                    c.sftp.rename(aside, path).await?;
                }
                Ok(())
            }
        })
    }
}

fn kind_of(t: FileType) -> Kind {
    match t {
        FileType::Dir => Kind::Dir,
        FileType::File => Kind::File,
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("holzwave-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_listing_has_folders_first_then_names_without_regard_to_case() {
        let d = scratch("list");
        fs::create_dir(d.join("zeta")).unwrap();
        fs::create_dir(d.join("Alpha")).unwrap();
        fs::write(d.join("beta.txt"), "b").unwrap();
        fs::write(d.join("Apple.md"), "aa").unwrap();
        fs::write(d.join(".hidden"), "").unwrap();
        let src = LocalSource::new(Some(d.clone()));
        let l = src.list(d.to_str().unwrap()).unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", ".hidden", "Apple.md", "beta.txt"]);
        assert!(l.entries.iter().find(|e| e.name == ".hidden").unwrap().hidden);
        assert_eq!(l.entries.iter().find(|e| e.name == "Apple.md").unwrap().size, 2);
        assert!(!l.truncated);
        let real = fs::canonicalize(&d).unwrap();
        assert_eq!(l.parent.as_deref(), real.parent().and_then(|p| p.to_str()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_tilde_is_the_home_directory_and_a_bad_path_is_refused() {
        let d = scratch("home");
        fs::write(d.join("a.txt"), "x").unwrap();
        let src = LocalSource::new(Some(d.clone()));
        assert_eq!(src.read("~/a.txt", 10).unwrap(), b"x");
        assert_eq!(src.list("~").unwrap().entries.len(), 1);
        assert!(src.list("").is_err());
        assert!(src.read("a\0b", 10).is_err());
        assert!(src.read(&"x".repeat(MAX_PATH + 1), 10).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_read_is_bounded_and_a_range_reads_a_piece() {
        let d = scratch("read");
        fs::write(d.join("f"), b"0123456789").unwrap();
        let src = LocalSource::new(Some(d.clone()));
        let p = d.join("f");
        let p = p.to_str().unwrap();
        assert_eq!(src.read(p, 10).unwrap(), b"0123456789");
        assert!(src.read(p, 9).unwrap_err().to_string().contains("more than the 9 allowed"));
        assert_eq!(src.read_range(p, 3, 4).unwrap(), b"3456");
        assert_eq!(src.read_range(p, 8, 100).unwrap(), b"89", "a range stops at the end of the file");
        assert!(src.read(d.to_str().unwrap(), 10).is_err(), "a directory is not a file");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_replaces_the_file_whole_and_keeps_its_permissions() {
        let d = scratch("write");
        let p = d.join("f.txt");
        fs::write(&p, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let src = LocalSource::new(Some(d.clone()));
        src.write(p.to_str().unwrap(), b"new content").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new content");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o640);
        }
        let leftovers: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left: {leftovers:?}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_link_shows_what_it_points_to() {
        #[cfg(unix)]
        {
            let d = scratch("link");
            fs::create_dir(d.join("real")).unwrap();
            std::os::unix::fs::symlink(d.join("real"), d.join("alias")).unwrap();
            std::os::unix::fs::symlink(d.join("nowhere"), d.join("broken")).unwrap();
            let l = LocalSource::new(Some(d.clone())).list(d.to_str().unwrap()).unwrap();
            let alias = l.entries.iter().find(|e| e.name == "alias").unwrap();
            assert_eq!((alias.kind, alias.link), (Kind::Dir, true));
            let broken = l.entries.iter().find(|e| e.name == "broken").unwrap();
            assert_eq!((broken.kind, broken.link), (Kind::Other, true));
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn the_parent_of_a_path() {
        assert_eq!(parent_of("/a/b"), Some("/a".into()));
        assert_eq!(parent_of("/a"), Some("/".into()));
        assert_eq!(parent_of("/"), None);
        assert_eq!(parent_of("/a/b/"), Some("/a".into()));
    }
}

#[cfg(test)]
mod sftp_tests {
    use super::*;
    use crate::ssh::{Reconnect, SshAuth};
    use russh::keys::{Algorithm, PrivateKey};
    use russh::server::{self, Auth, Msg, Server as _, Session};
    use russh::{Channel, ChannelId};
    use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, Status, StatusCode, Version};
    use std::collections::HashMap;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    /// An SSH server with an SFTP subsystem that serves a real directory.
    #[derive(Clone)]
    struct Fake;

    struct Conn {
        channels: HashMap<ChannelId, Channel<Msg>>,
    }

    impl server::Server for Fake {
        type Handler = Conn;
        fn new_client(&mut self, _: Option<SocketAddr>) -> Conn {
            Conn { channels: HashMap::new() }
        }
    }

    impl server::Handler for Conn {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> std::result::Result<Auth, Self::Error> {
            Ok(if user == "holz" && password == "pw" { Auth::Accept } else { Auth::reject() })
        }

        async fn channel_open_session(
            &mut self,
            channel: Channel<Msg>,
            reply: server::ChannelOpenHandle,
            _session: &mut Session,
        ) -> std::result::Result<(), Self::Error> {
            self.channels.insert(channel.id(), channel);
            reply.accept().await;
            Ok(())
        }

        async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> std::result::Result<(), Self::Error> {
            if name == "sftp" {
                if let Some(channel) = self.channels.remove(&id) {
                    session.channel_success(id)?;
                    russh_sftp::server::run(channel.into_stream(), Files::default()).await;
                    return Ok(());
                }
            }
            session.channel_failure(id)?;
            Ok(())
        }
    }

    enum Open {
        Dir(Vec<File>),
        File(std::fs::File),
    }

    #[derive(Default)]
    struct Files {
        handles: HashMap<String, Open>,
        next: u32,
    }

    fn status(id: u32, code: StatusCode) -> Status {
        Status { id, status_code: code, error_message: format!("{code:?}"), language_tag: "en".into() }
    }

    fn io_status(e: &std::io::Error) -> StatusCode {
        match e.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
            std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
            _ => StatusCode::Failure,
        }
    }

    impl russh_sftp::server::Handler for Files {
        type Error = StatusCode;

        fn unimplemented(&self) -> Self::Error {
            StatusCode::OpUnsupported
        }

        async fn init(&mut self, _v: u32, _e: HashMap<String, String>) -> std::result::Result<Version, Self::Error> {
            Ok(Version::new())
        }

        async fn realpath(&mut self, id: u32, path: String) -> std::result::Result<Name, Self::Error> {
            let real = std::fs::canonicalize(if path == "." { "/".to_owned() } else { path }).map_err(|e| io_status(&e))?;
            Ok(Name { id, files: vec![File::dummy(real.to_string_lossy().into_owned())] })
        }

        async fn opendir(&mut self, id: u32, path: String) -> std::result::Result<Handle, Self::Error> {
            let mut files = Vec::new();
            for item in std::fs::read_dir(&path).map_err(|e| io_status(&e))? {
                let item = item.map_err(|e| io_status(&e))?;
                let meta = std::fs::symlink_metadata(item.path()).map_err(|e| io_status(&e))?;
                files.push(File::new(item.file_name().to_string_lossy().into_owned(), FileAttributes::from(&meta)));
            }
            self.next += 1;
            let handle = format!("h{}", self.next);
            self.handles.insert(handle.clone(), Open::Dir(files));
            Ok(Handle { id, handle })
        }

        async fn readdir(&mut self, id: u32, handle: String) -> std::result::Result<Name, Self::Error> {
            match self.handles.get_mut(&handle) {
                Some(Open::Dir(files)) if !files.is_empty() => Ok(Name { id, files: std::mem::take(files) }),
                _ => Err(StatusCode::Eof),
            }
        }

        async fn stat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
            let meta = std::fs::metadata(&path).map_err(|e| io_status(&e))?;
            Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
        }

        async fn lstat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, Self::Error> {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| io_status(&e))?;
            Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
        }

        async fn fstat(&mut self, id: u32, handle: String) -> std::result::Result<Attrs, Self::Error> {
            match self.handles.get(&handle) {
                Some(Open::File(f)) => Ok(Attrs { id, attrs: FileAttributes::from(&f.metadata().map_err(|e| io_status(&e))?) }),
                _ => Err(StatusCode::Failure),
            }
        }

        async fn open(&mut self, id: u32, path: String, flags: russh_sftp::protocol::OpenFlags, _a: FileAttributes) -> std::result::Result<Handle, Self::Error> {
            use russh_sftp::protocol::OpenFlags as F;
            let mut options = std::fs::OpenOptions::new();
            options.read(flags.contains(F::READ)).write(flags.contains(F::WRITE)).create(flags.contains(F::CREATE)).truncate(flags.contains(F::TRUNCATE));
            let file = options.open(&path).map_err(|e| io_status(&e))?;
            self.next += 1;
            let handle = format!("h{}", self.next);
            self.handles.insert(handle.clone(), Open::File(file));
            Ok(Handle { id, handle })
        }

        async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> std::result::Result<Data, Self::Error> {
            let Some(Open::File(file)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
            file.seek(SeekFrom::Start(offset)).map_err(|e| io_status(&e))?;
            let mut buf = vec![0u8; len as usize];
            let n = file.read(&mut buf).map_err(|e| io_status(&e))?;
            if n == 0 {
                return Err(StatusCode::Eof);
            }
            buf.truncate(n);
            Ok(Data { id, data: buf })
        }

        async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> std::result::Result<Status, Self::Error> {
            let Some(Open::File(file)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
            file.seek(SeekFrom::Start(offset)).map_err(|e| io_status(&e))?;
            file.write_all(&data).map_err(|e| io_status(&e))?;
            Ok(status(id, StatusCode::Ok))
        }

        async fn close(&mut self, id: u32, handle: String) -> std::result::Result<Status, Self::Error> {
            self.handles.remove(&handle);
            Ok(status(id, StatusCode::Ok))
        }

        async fn setstat(&mut self, id: u32, _path: String, _attrs: FileAttributes) -> std::result::Result<Status, Self::Error> {
            Ok(status(id, StatusCode::Ok))
        }

        async fn rename(&mut self, id: u32, old: String, new: String) -> std::result::Result<Status, Self::Error> {
            std::fs::rename(old, new).map_err(|e| io_status(&e))?;
            Ok(status(id, StatusCode::Ok))
        }

        async fn remove(&mut self, id: u32, path: String) -> std::result::Result<Status, Self::Error> {
            std::fs::remove_file(path).map_err(|e| io_status(&e))?;
            Ok(status(id, StatusCode::Ok))
        }
    }

    fn serve() -> SocketAddr {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::from_millis(0),
            auth_rejection_time_initial: Some(Duration::from_millis(0)),
            ..Default::default()
        });
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                tx.send(socket.local_addr().unwrap()).unwrap();
                let _ = Fake.run_on_socket(config, &socket).await;
            });
        });
        rx.recv().unwrap()
    }

    fn source(addr: SocketAddr) -> SftpSource {
        let mut t = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("pw".into()));
        t.port = addr.port();
        t.reconnect = Reconnect { attempts: 0, delay: Duration::from_millis(10) };
        SftpSource::new("Test host", t).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("holzwave-sftp-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_remote_directory_is_listed_like_a_local_one() {
        let d = scratch("list");
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("b.txt"), "bb").unwrap();
        fs::write(d.join("A.md"), "a").unwrap();
        let src = source(serve());
        assert_eq!(src.label(), "Test host");
        let l = src.list(d.to_str().unwrap()).unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(names, [("sub", Kind::Dir), ("A.md", Kind::File), ("b.txt", Kind::File)]);
        assert_eq!(l.entries[2].size, 2);
        let real = fs::canonicalize(&d).unwrap();
        assert_eq!(l.parent.as_deref(), real.parent().and_then(|p| p.to_str()));
        assert!(src.list("/definitely/not/here").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_remote_file_is_read_in_bounds_and_in_ranges() {
        let d = scratch("read");
        fs::write(d.join("f"), b"0123456789").unwrap();
        let src = source(serve());
        let p = d.join("f");
        let p = p.to_str().unwrap();
        assert_eq!(src.read(p, 10).unwrap(), b"0123456789");
        assert!(src.read(p, 5).unwrap_err().to_string().contains("more than the 5 allowed"));
        assert_eq!(src.read_range(p, 4, 3).unwrap(), b"456");
        assert_eq!(src.stat(p).unwrap().size, 10);
        assert!(src.read(d.join("nope").to_str().unwrap(), 10).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_remote_write_replaces_the_file_and_leaves_no_temporary_file() {
        let d = scratch("write");
        let p = d.join("f.txt");
        fs::write(&p, "old").unwrap();
        let src = source(serve());
        src.write(p.to_str().unwrap(), b"new content").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new content");
        let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["f.txt"], "{names:?}");
        // A file that does not exist yet is created.
        src.write(d.join("fresh.txt").to_str().unwrap(), b"x").unwrap();
        assert_eq!(fs::read_to_string(d.join("fresh.txt")).unwrap(), "x");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_refused_login_is_an_error_with_the_reason() {
        let addr = serve();
        let mut t = SshTarget::new("127.0.0.1", "holz", SshAuth::Password("wrong".into()));
        t.port = addr.port();
        let src = SftpSource::new("x", t).unwrap();
        let err = src.home().unwrap_err().to_string();
        assert!(err.contains("refused the login"), "{err}");
    }
}

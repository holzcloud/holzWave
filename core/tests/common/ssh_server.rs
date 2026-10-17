//! The one in-process SSH server of the integration tests.
//!
//! It stands in for a real host so that the tests of the pane (`SshLink`), the exec primitive (`SshExec`) and every
//! other consumer of a session run in a plain `cargo test`, with no Zig and no machine of their own. Every test file
//! that needs a server starts it here; nobody writes a second one, and a feature that needs a new server behaviour (a
//! subsystem, a forward, another login method) adds it to this file.
//!
//! What it does:
//!
//! - **Login.** Password login (exactly the option's user and password) and public-key login (exactly the option's
//!   user and one of the authorized public keys). The server verifies the signature and knows the public half only,
//!   so a password cannot be what let a key test pass. Every refusal and every success is counted.
//! - **Shell.** A pty request is accepted and counted; a shell request prints `welcome N` (`N` is the zero-based index
//!   of the connection), each input line comes back as `echo:<line>` and the line `exit` ends the shell with status 3.
//! - **Exec.** The command line is recorded, `ran:<command>` is printed and the command ends with status 0.
//! - **Jump host.** With [`Options::jump`] the server answers `direct-tcpip` requests for loopback addresses by
//!   connecting to them and copying both ways; every request is recorded.
//! - **SFTP.** With [`Options::sftp`] the `sftp` subsystem serves a real directory (and refuses every path outside it);
//!   every subsystem started is counted.
//! - **Cuts and counts.** [`Server::cut`] ends every connection that is open now; `Seen::disconnects` counts the
//!   connections that ended, whoever ended them.
//!
//! The server runs on a thread of its own with a loopback listener on a free port, until the process ends.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use holzwave_core::ssh::{HostKeyPolicy, Reconnect, SshAuth, SshTarget, learn_host_key};
use russh::keys::ssh_key::private::{KeypairData, RsaKeypair};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Disconnect};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle as SftpHandle, Name, OpenFlags, Status, StatusCode, Version};
use tokio::net::{TcpListener, TcpStream};

/// What the server accepts.
#[derive(Clone)]
pub struct Options {
    /// The one user that can log in.
    pub user: &'static str,
    /// The password of that user; `None` refuses every password.
    pub password: Option<&'static str>,
    /// The public keys of that user; none refuses every key.
    pub authorized: Vec<PublicKey>,
    /// Answer `direct-tcpip` requests (the server is a jump host).
    pub jump: bool,
    /// Serve the `sftp` subsystem over this directory (made canonical by [`Options::sftp`]).
    pub sftp_root: Option<PathBuf>,
    /// What every exec request prints instead of `ran:<command>`.
    pub exec_reply: Option<Vec<u8>>,
}

impl Options {
    /// User `holz` with the password `pw` and nothing else.
    pub fn password() -> Self {
        Self { user: "holz", password: Some("pw"), authorized: Vec::new(), jump: false, sftp_root: None, exec_reply: None }
    }

    /// User `holz` with this public key and no password.
    pub fn key(key: PublicKey) -> Self {
        Self { user: "holz", password: None, authorized: vec![key], jump: false, sftp_root: None, exec_reply: None }
    }

    /// The same server, and it also answers `direct-tcpip`.
    pub fn jump(mut self) -> Self {
        self.jump = true;
        self
    }

    /// The same server, and its SFTP subsystem serves `root`. Paths outside it are refused.
    pub fn sftp(mut self, root: PathBuf) -> Self {
        self.sftp_root = Some(root.canonicalize().unwrap());
        self
    }

    /// The same server, and every exec request prints `bytes` (and nothing else) before it ends with status 0.
    pub fn exec_reply(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.exec_reply = Some(bytes.into());
        self
    }
}

/// What the server saw, shared with the test that started it.
#[derive(Default)]
pub struct Seen {
    /// TCP connections accepted.
    pub connections: AtomicUsize,
    /// Successful logins.
    pub logins: AtomicUsize,
    /// Refused logins.
    pub refused_logins: AtomicUsize,
    /// Shell requests.
    pub shells: AtomicUsize,
    /// Pty requests.
    pub ptys: AtomicUsize,
    /// The command line of every exec request, in order.
    pub commands: Mutex<Vec<String>>,
    /// The `(host, port)` of every `direct-tcpip` request, in order.
    pub direct_tcpip: Mutex<Vec<(String, u16)>>,
    /// Connections that ended (the server dropped the handler of the connection), whoever ended them.
    pub disconnects: AtomicUsize,
    /// `sftp` subsystems started.
    pub sftp_sessions: AtomicUsize,
    /// The handles of the connections that logged in, for [`Server::cut`].
    handles: Mutex<Vec<server::Handle>>,
}

impl Seen {
    /// The commands run so far.
    pub fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// The `direct-tcpip` requests so far.
    pub fn direct_tcpip(&self) -> Vec<(String, u16)> {
        self.direct_tcpip.lock().unwrap().clone()
    }
}

/// A running server.
pub struct Server {
    pub addr: SocketAddr,
    pub host_key: PublicKey,
    pub seen: Arc<Seen>,
}

/// Start a server on a loopback port of the system's choosing.
pub fn start(options: Options) -> Server {
    let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host.clone()],
        // A refusal is quick, so a test of a refused login does not wait.
        auth_rejection_time: Duration::from_millis(0),
        auth_rejection_time_initial: Some(Duration::from_millis(0)),
        ..Default::default()
    });
    let seen = Arc::new(Seen::default());
    let mut factory = Factory { seen: seen.clone(), options };
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            tx.send(socket.local_addr().unwrap()).unwrap();
            let _ = factory.run_on_socket(config, &socket).await;
        });
    });
    Server { addr: rx.recv().unwrap(), host_key: host.public_key().clone(), seen }
}

impl Server {
    /// End every connection that is open now, as a network that failed would. A connection made afterwards is not
    /// touched.
    pub fn cut(&self) {
        let handles = std::mem::take(&mut *self.seen.handles.lock().unwrap());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        for handle in handles {
            let _ = rt.block_on(handle.disconnect(Disconnect::ByApplication, "cut".into(), "en".into()));
        }
    }

    /// A target for a pane: host `127.0.0.1` and this port, user `holz`, three attempts 100 ms apart.
    pub fn target(&self, auth: SshAuth) -> SshTarget {
        let mut target = SshTarget::new("127.0.0.1", "holz", auth);
        target.port = self.addr.port();
        target.reconnect = Reconnect { attempts: 3, delay: Duration::from_millis(100) };
        target
    }

    /// Learn the host key of this server into a known hosts file, as if the user's own pane had connected before.
    pub fn trust(&self, known_hosts: &Path) {
        learn_host_key("127.0.0.1", self.addr.port(), &self.host_key, known_hosts).unwrap();
    }

    /// A target for the exec primitive: a known hosts file and the strict policy. It does not learn the key; call
    /// [`Server::trust`] for that. The file is made empty when it does not exist.
    pub fn exec_target(&self, auth: SshAuth, known_hosts: &Path) -> SshTarget {
        if !known_hosts.exists() {
            std::fs::write(known_hosts, "").unwrap();
        }
        let mut target = self.target(auth);
        target.known_hosts = Some(known_hosts.to_path_buf());
        target.host_key = HostKeyPolicy::Strict;
        target
    }
}

/// An Ed25519 key.
pub fn ed25519() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

/// An RSA key of `bits` bits. Generating one takes seconds in a dev build, so a test makes it once.
pub fn rsa(bits: usize) -> PrivateKey {
    let pair = RsaKeypair::random(&mut rand::rng(), bits).unwrap();
    PrivateKey::new(KeypairData::from(pair), "").unwrap()
}

/// Write `key` into `dir` as `name` in the OpenSSH format, mode 0600 on Unix, encrypted with `passphrase` when one is
/// given. The folder is the caller's private scratch folder; the path of the file is returned.
pub fn write_key(dir: &Path, name: &str, key: &PrivateKey, passphrase: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    let text = match passphrase {
        Some(passphrase) => {
            let sealed = key.encrypt(&mut rand::rng(), passphrase).unwrap();
            sealed.to_openssh(LineEnding::LF).unwrap().to_string()
        }
        None => key.to_openssh(LineEnding::LF).unwrap().to_string(),
    };
    write_private(&path, text.as_bytes());
    path
}

/// Write a file that only its owner can read.
pub fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).unwrap().write_all(bytes).unwrap();
}

/// The output of a link, read by one helper thread, so a test can wait for text as often as it likes.
pub struct Out {
    rx: mpsc::Receiver<Vec<u8>>,
    pub seen: String,
}

impl Out {
    pub fn new(mut reader: Box<dyn Read + Send>) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self { rx, seen: String::new() }
    }

    /// Wait until `needle` has been seen in the output, or fail after `secs` seconds.
    pub fn until(&mut self, needle: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !self.seen.contains(needle) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(chunk) => self.seen.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => panic!("no {needle:?} within {secs} s; saw {:?}", self.seen),
            }
        }
    }
}

// ---- the server itself ------------------------------------------------------------------------------------------

/// Makes one [`Handler`] per connection.
#[derive(Clone)]
struct Factory {
    seen: Arc<Seen>,
    options: Options,
}

impl server::Server for Factory {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Handler {
        let index = self.seen.connections.fetch_add(1, Ordering::SeqCst);
        Handler { seen: self.seen.clone(), options: self.options.clone(), index, shells: HashSet::new(), channels: HashMap::new() }
    }
}

struct Handler {
    seen: Arc<Seen>,
    options: Options,
    /// The zero-based index of this connection.
    index: usize,
    /// The channels that run the shell. Data on any other channel (a forwarded connection) is not the shell's input.
    shells: HashSet<ChannelId>,
    /// Session channels that have not asked for anything yet: an `sftp` request needs the channel itself. A channel is
    /// let go of when it runs a shell or a command, because nobody reads it then and its data would pile up.
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl Drop for Handler {
    fn drop(&mut self) {
        self.seen.disconnects.fetch_add(1, Ordering::SeqCst);
    }
}

impl Handler {
    fn judge(&self, accepted: bool) -> Auth {
        if accepted {
            self.seen.logins.fetch_add(1, Ordering::SeqCst);
            Auth::Accept
        } else {
            self.seen.refused_logins.fetch_add(1, Ordering::SeqCst);
            Auth::reject()
        }
    }
}

impl server::Handler for Handler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let accepted = user == self.options.user && self.options.password == Some(password);
        Ok(self.judge(accepted))
    }

    // Called after the signature was verified: the client really holds the private key.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let accepted = user == self.options.user && self.options.authorized.iter().any(|known| known == key);
        Ok(self.judge(accepted))
    }

    async fn auth_succeeded(&mut self, session: &mut Session) -> Result<(), Self::Error> {
        self.seen.handles.lock().unwrap().push(session.handle());
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.options.sftp_root.is_some() {
            self.channels.insert(channel.id(), channel);
        }
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        if let (Some(root), "sftp", Some(channel)) = (&self.options.sftp_root, name, self.channels.remove(&id)) {
            self.seen.sftp_sessions.fetch_add(1, Ordering::SeqCst);
            session.channel_success(id)?;
            let files = Files { root: root.clone(), handles: HashMap::new(), next: 0 };
            tokio::spawn(russh_sftp::server::run(channel.into_stream(), files));
            return Ok(());
        }
        session.channel_failure(id)?;
        Ok(())
    }

    // A jump request. The server only forwards to loopback addresses, and only when it was made a jump host; any other
    // request is rejected by dropping the reply.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port) else { return Ok(()) };
        self.seen.direct_tcpip.lock().unwrap().push((host.to_string(), port));
        if !self.options.jump || host != "127.0.0.1" {
            return Ok(());
        }
        let Ok(mut onward) = TcpStream::connect((host, port)).await else { return Ok(()) };
        reply.accept().await;
        let mut stream = channel.into_stream();
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut onward).await;
        });
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _cols: u32,
        _rows: u32,
        _px_w: u32,
        _px_h: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.seen.ptys.fetch_add(1, Ordering::SeqCst);
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        self.seen.shells.fetch_add(1, Ordering::SeqCst);
        self.shells.insert(channel);
        session.channel_success(channel)?;
        session.data(channel, format!("welcome {}\r\n", self.index).into_bytes())?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        if !self.shells.contains(&channel) {
            return Ok(());
        }
        let text = String::from_utf8_lossy(data).to_string();
        if text.trim() == "exit" {
            session.exit_status_request(channel, 3)?;
            session.close(channel)?;
        } else {
            session.data(channel, format!("echo:{text}").into_bytes())?;
        }
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.channels.remove(&channel);
        let command = String::from_utf8_lossy(data).to_string();
        self.seen.commands.lock().unwrap().push(command.clone());
        session.channel_success(channel)?;
        match &self.options.exec_reply {
            Some(bytes) => session.data(channel, bytes.clone())?,
            None => session.data(channel, format!("ran:{command}\r\n").into_bytes())?,
        }
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

// ---- the SFTP subsystem ------------------------------------------------------------------------------------------

/// An SFTP handler that serves a real directory with `std::fs` and refuses every path outside it.
struct Files {
    root: PathBuf,
    handles: HashMap<String, Open>,
    next: u32,
}

enum Open {
    Dir(Vec<File>),
    File(std::fs::File),
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

impl Files {
    /// The path as a path below the root, or a refusal: no `..`, and the root is a prefix.
    fn guard(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let path = Path::new(path);
        let climbs = path.components().any(|part| matches!(part, std::path::Component::ParentDir));
        if climbs || !path.starts_with(&self.root) {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(path.to_path_buf())
    }

    fn handle(&mut self, open: Open) -> String {
        self.next += 1;
        let handle = format!("h{}", self.next);
        self.handles.insert(handle.clone(), open);
        handle
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _v: u32, _e: HashMap<String, String>) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let real = if path == "." { self.root.clone() } else { std::fs::canonicalize(self.guard(&path)?).map_err(|e| io_status(&e))? };
        Ok(Name { id, files: vec![File::dummy(real.to_string_lossy().into_owned())] })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<SftpHandle, Self::Error> {
        let mut files = Vec::new();
        for item in std::fs::read_dir(self.guard(&path)?).map_err(|e| io_status(&e))? {
            let item = item.map_err(|e| io_status(&e))?;
            let meta = std::fs::symlink_metadata(item.path()).map_err(|e| io_status(&e))?;
            files.push(File::new(item.file_name().to_string_lossy().into_owned(), FileAttributes::from(&meta)));
        }
        Ok(SftpHandle { id, handle: self.handle(Open::Dir(files)) })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(Open::Dir(files)) if !files.is_empty() => Ok(Name { id, files: std::mem::take(files) }),
            _ => Err(StatusCode::Eof),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::metadata(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let meta = std::fs::symlink_metadata(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Open::File(f)) => Ok(Attrs { id, attrs: FileAttributes::from(&f.metadata().map_err(|e| io_status(&e))?) }),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn open(&mut self, id: u32, path: String, flags: OpenFlags, _a: FileAttributes) -> Result<SftpHandle, Self::Error> {
        let path = self.guard(&path)?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .create(flags.contains(OpenFlags::CREATE))
            .truncate(flags.contains(OpenFlags::TRUNCATE));
        let file = options.open(path).map_err(|e| io_status(&e))?;
        Ok(SftpHandle { id, handle: self.handle(Open::File(file)) })
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
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

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let Some(Open::File(file)) = self.handles.get_mut(&handle) else { return Err(StatusCode::Failure) };
        file.seek(SeekFrom::Start(offset)).map_err(|e| io_status(&e))?;
        file.write_all(&data).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(status(id, StatusCode::Ok))
    }

    async fn setstat(&mut self, id: u32, path: String, attrs: FileAttributes) -> Result<Status, Self::Error> {
        let path = self.guard(&path)?;
        #[cfg(unix)]
        if let Some(mode) = attrs.permissions {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o7777)).map_err(|e| io_status(&e))?;
        }
        #[cfg(not(unix))]
        let _ = (path, attrs);
        Ok(status(id, StatusCode::Ok))
    }

    async fn rename(&mut self, id: u32, old: String, new: String) -> Result<Status, Self::Error> {
        std::fs::rename(self.guard(&old)?, self.guard(&new)?).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.guard(&path)?).map_err(|e| io_status(&e))?;
        Ok(status(id, StatusCode::Ok))
    }
}

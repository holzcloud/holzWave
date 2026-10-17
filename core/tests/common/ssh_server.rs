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
//!
//! The server runs on a thread of its own with a loopback listener on a free port, until the process ends.

use std::collections::HashSet;
use std::io::Read;
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
use russh::{Channel, ChannelId};
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
}

impl Options {
    /// User `holz` with the password `pw` and nothing else.
    pub fn password() -> Self {
        Self { user: "holz", password: Some("pw"), authorized: Vec::new(), jump: false }
    }

    /// User `holz` with this public key and no password.
    pub fn key(key: PublicKey) -> Self {
        Self { user: "holz", password: None, authorized: vec![key], jump: false }
    }

    /// The same server, and it also answers `direct-tcpip`.
    pub fn jump(mut self) -> Self {
        self.jump = true;
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
        Handler { seen: self.seen.clone(), options: self.options.clone(), index, shells: HashSet::new() }
    }
}

struct Handler {
    seen: Arc<Seen>,
    options: Options,
    /// The zero-based index of this connection.
    index: usize,
    /// The channels that run the shell. Data on any other channel (a forwarded connection) is not the shell's input.
    shells: HashSet<ChannelId>,
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

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
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
        let command = String::from_utf8_lossy(data).to_string();
        self.seen.commands.lock().unwrap().push(command.clone());
        session.channel_success(channel)?;
        session.data(channel, format!("ran:{command}\r\n").into_bytes())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

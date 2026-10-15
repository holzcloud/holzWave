//! The local socket between the running app and `wsh`.
//!
//! The app listens on a socket in a directory only its user can enter (a Unix domain socket; a named pipe on Windows), and
//! hands every pane it starts three variables: `HOLZWAVE_IPC` (the directory), `HOLZWAVE_TOKEN` (a random secret of this
//! run of the app) and `HOLZWAVE_PANE`. A request is one line of JSON and so is the answer. A request without the token
//! is refused before anything is read from it, so only a process that was started inside a pane (or that can read its
//! environment, which is the same user) can control the app. Every size is bounded and every field is checked here,
//! before the app's handler sees it.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ListenerOptions, Stream, ToFsName, ToNsName};
use serde::{Deserialize, Serialize};

/// The longest request line, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// The longest path in a command.
pub const MAX_PATH: usize = 4096;
/// The longest value of `setmeta`.
pub const MAX_VALUE: usize = 200;
/// How many requests are handled at once.
const MAX_CLIENTS: usize = 8;

/// What `wsh` can ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    /// Show a file in a preview beside the pane.
    View { path: String },
    /// Open a file in the editor beside the pane.
    Edit { path: String },
    /// Set a property of the pane. `key` is `title`.
    Setmeta { key: String, value: String },
    /// Nothing: asks whether the app answers.
    Ping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    /// The pane the command was typed in (`HOLZWAVE_PANE`).
    pub pane: Option<u64>,
    #[serde(flatten)]
    pub cmd: Cmd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
}

impl Cmd {
    /// Whether the command is well formed: sizes and characters. The app checks what it means.
    pub fn check(&self) -> Result<(), String> {
        let path_ok = |p: &str| !p.is_empty() && p.len() <= MAX_PATH && !p.contains('\0');
        match self {
            Cmd::View { path } | Cmd::Edit { path } if !path_ok(path) => Err("the path is empty, too long or has a NUL".into()),
            Cmd::Setmeta { key, value } => {
                if key != "title" {
                    return Err(format!("unknown property {key:?}; the property is `title`"));
                }
                if value.chars().count() > MAX_VALUE || value.chars().any(char::is_control) {
                    return Err(format!("the title is longer than {MAX_VALUE} characters or has a control character"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// What the app does with a command. `Ok` is the text `wsh` prints, `Err` the reason it exits with an error.
pub trait Handler: Send + Sync {
    fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String>;
}

/// A new random token: 128 bits as hex.
pub fn new_token() -> io::Result<String> {
    use std::fmt::Write;
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|e| io::Error::other(format!("no random bytes: {e}")))?;
    Ok(raw.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Compare two secrets without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().min(b.len()) {
        diff |= usize::from(a[i] ^ b[i]);
    }
    diff == 0
}

fn name(dir: &Path) -> io::Result<interprocess::local_socket::Name<'static>> {
    if cfg!(windows) {
        // A named pipe is not in the file system; the directory's last parts make it the user's own.
        let tag: String = dir.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let tag = &tag[tag.len().saturating_sub(48)..];
        format!("holzwave-{tag}").to_ns_name::<GenericNamespaced>().map(interprocess::local_socket::Name::into_owned)
    } else {
        let path = dir.join("ipc.sock");
        // `sun_path` is about 104 bytes on macOS and 108 on Linux.
        if path.as_os_str().len() > 100 {
            return Err(io::Error::other("the socket path is too long; set XDG_RUNTIME_DIR to a shorter one"));
        }
        path.to_fs_name::<GenericFilePath>().map(interprocess::local_socket::Name::into_owned)
    }
}

/// Read one line of at most [`MAX_LINE`] bytes.
fn read_line(stream: &mut impl Read) -> io::Result<String> {
    let mut reader = BufReader::new(stream.take(MAX_LINE as u64 + 1));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the request is too long"));
    }
    Ok(line)
}

fn answer(stream: &mut Stream, ok: bool, message: impl Into<String>) {
    let mut line = serde_json::to_string(&Response { ok, message: message.into() }).unwrap_or_default();
    line.push('\n');
    let _ = stream.write_all(line.as_bytes());
}

fn serve_one(mut stream: Stream, token: &str, handler: &dyn Handler) {
    let line = match read_line(&mut stream) {
        Ok(l) => l,
        Err(e) => return answer(&mut stream, false, e.to_string()),
    };
    // The token first: a request that does not carry it is not looked at any further.
    let value: serde_json::Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(_) => return answer(&mut stream, false, "not a valid request"),
    };
    if !value.get("token").and_then(|t| t.as_str()).is_some_and(|t| same(t, token)) {
        return answer(&mut stream, false, "refused: wsh must run in a holzWave terminal (the token is missing or wrong)");
    }
    let request: Request = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => return answer(&mut stream, false, format!("unknown command: {e}")),
    };
    if let Err(why) = request.cmd.check() {
        return answer(&mut stream, false, why);
    }
    match handler.handle(request.pane, request.cmd) {
        Ok(m) => answer(&mut stream, true, m),
        Err(e) => answer(&mut stream, false, e),
    }
}

/// A running server. Dropping it stops accepting.
pub struct Server {
    stop: Arc<AtomicBool>,
    dir: PathBuf,
}

impl Server {
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // A connection wakes the blocked accept; the socket file goes.
        let _ = connect(&self.dir);
        let _ = std::fs::remove_file(self.dir.join("ipc.sock"));
    }
}

/// Make the directory (only its user may enter it) and listen in it.
pub fn serve(dir: &Path, token: String, handler: Arc<dyn Handler>) -> io::Result<Server> {
    make_private_dir(dir)?;
    let socket = dir.join("ipc.sock");
    // A socket left by a crashed run is not a reason to refuse to start.
    if !cfg!(windows) {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = ListenerOptions::new().name(name(dir)?).create_sync()?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let active = Arc::new(AtomicUsize::new(0));
    thread::Builder::new().name("holzwave-ipc".into()).spawn(move || {
        for stream in listener.incoming() {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            let Ok(stream) = stream else { continue };
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CLIENTS {
                active.fetch_sub(1, Ordering::SeqCst);
                let mut stream = stream;
                answer(&mut stream, false, "busy");
                continue;
            }
            let (token, handler, active) = (token.clone(), handler.clone(), active.clone());
            let _ = thread::Builder::new().name("holzwave-ipc-client".into()).spawn(move || {
                serve_one(stream, &token, &*handler);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
    })?;
    Ok(Server { stop, dir: dir.to_path_buf() })
}

#[cfg(unix)]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // A directory that was there before keeps its mode otherwise.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn make_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Connect to the app whose socket is in `dir`.
pub fn connect(dir: &Path) -> io::Result<Stream> {
    Stream::connect(name(dir)?)
}

/// Send one request and read the answer.
pub fn request(dir: &Path, request: &Request) -> io::Result<Response> {
    let mut stream = connect(dir)?;
    let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let answer = read_line(&mut stream)?;
    serde_json::from_str(&answer).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the app sent no valid answer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Echo(Mutex<Vec<(Option<u64>, Cmd)>>);

    impl Handler for Echo {
        fn handle(&self, pane: Option<u64>, cmd: Cmd) -> Result<String, String> {
            self.0.lock().unwrap().push((pane, cmd.clone()));
            match cmd {
                Cmd::View { path } if path == "/boom" => Err("no such file".into()),
                _ => Ok("done".into()),
            }
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hw-ipc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn req(token: &str, pane: Option<u64>, cmd: Cmd) -> Request {
        Request { token: token.into(), pane, cmd }
    }

    #[test]
    fn a_command_with_the_token_reaches_the_handler_and_its_answer_comes_back() {
        let d = dir("a");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let ok = request(&d, &req("secret", Some(3), Cmd::View { path: "/a/b".into() })).unwrap();
        assert_eq!(ok, Response { ok: true, message: "done".into() });
        assert_eq!(echo.0.lock().unwrap().as_slice(), &[(Some(3), Cmd::View { path: "/a/b".into() })]);
        let err = request(&d, &req("secret", None, Cmd::View { path: "/boom".into() })).unwrap();
        assert_eq!(err, Response { ok: false, message: "no such file".into() });
        assert!(request(&d, &req("secret", None, Cmd::Ping)).unwrap().ok);
    }

    #[test]
    fn a_wrong_or_missing_token_is_refused_and_the_handler_never_runs() {
        let d = dir("b");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        let r = request(&d, &req("guess", None, Cmd::Ping)).unwrap();
        assert!(!r.ok && r.message.contains("refused"));
        let mut stream = connect(&d).unwrap();
        stream.write_all(b"{\"cmd\":\"view\",\"path\":\"/x\"}\n").unwrap();
        let answer = read_line(&mut stream).unwrap();
        assert!(answer.contains("refused"), "{answer}");
        assert!(echo.0.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_oversized_and_unknown_requests_are_answered_not_crashed_on() {
        let d = dir("c");
        let echo = Arc::new(Echo(Mutex::default()));
        let _server = serve(&d, "secret".into(), echo.clone()).unwrap();
        for (line, expect) in [
            ("not json\n".to_owned(), "not a valid request"),
            ("{\"token\":\"secret\",\"cmd\":\"rm\"}\n".to_owned(), "unknown command"),
            (format!("{}\n", "x".repeat(MAX_LINE + 10)), "too long"),
        ] {
            let mut stream = connect(&d).unwrap();
            stream.write_all(line.as_bytes()).unwrap();
            let answer = read_line(&mut stream).unwrap();
            assert!(answer.contains(expect), "{answer}");
        }
        let bad = request(&d, &req("secret", None, Cmd::Setmeta { key: "cwd".into(), value: "x".into() })).unwrap();
        assert!(!bad.ok);
        let bad = request(&d, &req("secret", None, Cmd::Edit { path: "a\0b".into() })).unwrap();
        assert!(!bad.ok);
        assert!(echo.0.lock().unwrap().is_empty());
    }

    #[test]
    fn tokens_are_32_hex_characters_and_differ() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert!(a.len() == 32 && a.bytes().all(|c| c.is_ascii_hexdigit()) && a != b);
    }

    #[test]
    fn the_secrets_compare_exactly() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd") && !same("abc", "ab") && !same("", "a"));
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _server = serve(&d, "t".into(), Arc::new(Echo(Mutex::default()))).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn a_path_that_is_too_long_for_a_socket_is_said() {
        let long = PathBuf::from("/tmp").join("x".repeat(120));
        if !cfg!(windows) {
            assert!(serve(&long, "t".into(), Arc::new(Echo(Mutex::default()))).is_err());
        }
    }
}

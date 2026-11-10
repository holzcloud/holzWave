//! The holzWave server as a sync backend: the same [`Backend`] the WebDAV transport implements, spoken over the server's own
//! REST contract (`/sync/v1`), with the device token as the only credential.
//!
//! What the server stores is what the sync already produces: sealed blobs (`meta.json`, `files/<name>`, `backups/<name>`)
//! it cannot read. So this file carries bytes and conditions and nothing else, and the rules of the WebDAV transport apply
//! unchanged:
//!
//! * `https` is required except for a loopback address, redirects are not followed (so the token is never sent to another
//!   host), every answer is bounded and a server that answers wrongly is an error, never a panic;
//! * a failed condition (`412`) is the error whose text is [`CONFLICT`], the text the engine already reacts to;
//! * the token is only in the `Authorization` header of a request, marked sensitive. It is in no URL, no body, no error text
//!   and no `Debug` print, and the texts of every error are fixed sentences: nothing the server wrote is repeated.
//!
//! A `404` on `ensure` or `list` means the server has the sync target switched off (every route of a feature answers `404`
//! while its switch is off, so a probe learns nothing); on `get` it means the file does not exist, which is all a server that
//! answers the same for both can say. The next `ensure` or `list` of a run says which.

use std::fmt;
use std::io::{self, Read};
use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::config::Config;
use ureq::http::{HeaderValue, Request, StatusCode};
use ureq::tls::{RootCerts, TlsConfig};
use zeroize::Zeroizing;

use super::crypto::MAX_BLOB;
use super::webdav::{Backend, CONFLICT, Cond, Entry, safe_segment, sha256_hex};
use crate::server_link::{Fault, Origin, Token};
use crate::{Error, Result};

/// The most bytes of the answer to `ensure` and to a write: neither has a body worth reading.
const SMALL_LIMIT: u64 = 4 * 1024;
/// The most bytes of the answer to `list`: the server holds at most a few hundred files, each a short line.
const LIST_LIMIT: u64 = 1024 * 1024;
/// The most entries of a listing that are looked at; a server that sends more is answering wrongly.
const MAX_LIST_ENTRIES: usize = 4096;
/// The longest ETag that is accepted, in bytes (the server's is 64 hex characters).
const MAX_ETAG: usize = 128;
/// How long one request may take altogether.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// What the server says when a write is too large for it.
const TOO_LARGE: &str = "The file is too large for the server.";
/// What the server says when the person's space on it is used up.
const NO_SPACE: &str = "The space on the server is used up.";
/// What a path outside the contract is refused with, before any request.
const UNSAFE_PATH: &str = "The sync path is not one the server accepts.";

fn err(sentence: impl Into<String>) -> Error {
    Error::Io(io::Error::other(sentence.into()))
}

/// The sync backend of a holzWave server.
pub struct ServerBackend {
    agent: Agent,
    /// `scheme://host[:port]`, no trailing slash.
    base: String,
    /// The host and port, for the check before a connection to the local network.
    host: String,
    port: u16,
    /// The value of the `Authorization` header, wiped when the backend is dropped.
    bearer: Zeroizing<String>,
}

impl fmt::Debug for ServerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerBackend").field("base", &self.base).finish_non_exhaustive()
    }
}

/// An ETag without the weak marker and the quotes.
fn clean_etag(v: &str) -> String {
    v.trim().trim_start_matches("W/").trim_matches('"').to_owned()
}

/// Whether text is 64 lowercase hex characters, the form of a checksum and of the server's ETag.
fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The folder word of a route: `-` for the root.
fn dir_word(dir: &str) -> Result<&str> {
    match dir {
        "" => Ok("-"),
        "files" | "backups" => Ok(dir),
        _ => Err(err(UNSAFE_PATH)),
    }
}

/// A path of the contract: `meta.json`, or `files/<segment>` or `backups/<segment>` with a safe segment.
fn check_path(path: &str) -> Result<()> {
    let ok = path == "meta.json"
        || path.split_once('/').is_some_and(|(dir, segment)| matches!(dir, "files" | "backups") && safe_segment(segment));
    if ok { Ok(()) } else { Err(err(UNSAFE_PATH)) }
}

/// One answer: the status, the ETag of the answer and its body (empty unless the status is a success).
struct Answer {
    status: StatusCode,
    etag: Option<String>,
    body: Vec<u8>,
}

/// The HTTP agent every request to a holzWave server goes through: the platform's certificate verifier, `https` only unless
/// the address is a loopback one (`secure` is false), no redirect followed (so a token is never sent to another host), no
/// cookie jar, and `timeout` as the longest one request may take altogether.
pub(crate) fn agent(secure: bool, timeout: Duration) -> Agent {
    let config: Config = Config::builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .https_only(secure)
        .timeout_global(Some(timeout))
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .user_agent("holzWave-sync")
        .build();
    config.into()
}

impl ServerBackend {
    /// `origin` is the server's address, `https://host[:port]` (a plain `http` address only for a loopback host, which a test
    /// or a server on this computer uses). Anything with a user name, a path, a query or a fragment is refused.
    pub fn new(origin: &str, token: &Token) -> Result<Self> {
        let origin = Origin::parse(origin)?;
        Ok(Self {
            agent: agent(origin.secure(), REQUEST_TIMEOUT),
            base: origin.to_text(),
            host: origin.host().to_owned(),
            port: origin.port(),
            bearer: token.bearer(),
        })
    }

    fn send(&self, method: &str, target: &str, headers: &[(&str, String)], body: &[u8], limit: u64) -> Result<Answer> {
        let mut bearer = HeaderValue::from_str(&self.bearer).map_err(|_| Fault::Protocol)?;
        bearer.set_sensitive(true);
        let mut req = Request::builder().method(method).uri(format!("{}{target}", self.base)).header("Authorization", bearer);
        for (name, value) in headers {
            req = req.header(*name, value.as_str());
        }
        let req = req.body(body.to_vec()).map_err(|_| Fault::Protocol)?;
        // A server on the local network needs the platform's permission on Android 17 (a connection without it times out):
        // asked here, when the request is made. No gate exists elsewhere: this returns at once.
        crate::reach::ensure_blocking(&self.host, self.port).map_err(err)?;
        let mut resp = self.agent.run(req).map_err(|_| Fault::Unreachable)?;
        let status = resp.status();
        let etag = resp.headers().get("ETag").and_then(|v| v.to_str().ok()).map(clean_etag);
        // The body of a refusal is never read: it is the server's words, and nothing here would use them.
        if !status.is_success() {
            return Ok(Answer { status, etag, body: Vec::new() });
        }
        // An answer that says it is too large is refused before a byte of it is read.
        let announced = resp.headers().get("Content-Length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if announced.is_some_and(|n| n > limit) {
            return Err(Fault::Protocol.into());
        }
        let mut bytes = Vec::new();
        resp.body_mut().as_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|_| Fault::Unreachable)?;
        if bytes.len() as u64 > limit {
            return Err(Fault::Protocol.into());
        }
        Ok(Answer { status, etag, body: bytes })
    }

    /// The error of a status the caller has no meaning of its own for.
    fn fail(status: StatusCode) -> Error {
        Fault::of_status(status.as_u16()).into()
    }
}

/// One entry of the JSON listing, as the server writes it. Members it adds are ignored.
#[derive(Deserialize)]
struct Item {
    name: String,
    etag: String,
    #[serde(default)]
    checksum: Option<String>,
}

fn parse_listing(bytes: &[u8]) -> Result<Vec<Entry>> {
    let items: Vec<Item> = serde_json::from_slice(bytes).map_err(|_| Fault::Protocol)?;
    if items.len() > MAX_LIST_ENTRIES {
        return Err(Fault::Protocol.into());
    }
    Ok(items
        .into_iter()
        .filter_map(|item| {
            // A name that is not a safe segment, or an ETag that is not a short plain word, is dropped as the WebDAV
            // transport drops an odd name: one bad entry is no reason to refuse the rest.
            let etag = clean_etag(&item.etag);
            let plain = !etag.is_empty() && etag.len() <= MAX_ETAG && etag.bytes().all(|b| b.is_ascii_graphic());
            (safe_segment(&item.name) && plain)
                .then(|| Entry { name: item.name, etag, checksum: item.checksum.filter(|c| is_sha256_hex(c)) })
        })
        .collect())
}

impl Backend for ServerBackend {
    fn ensure(&self, dir: &str) -> Result<()> {
        let word = dir_word(dir)?;
        let answer = self.send("POST", &format!("/sync/v1/ensure/{word}"), &[], &[], SMALL_LIMIT)?;
        if answer.status.is_success() { Ok(()) } else { Err(Self::fail(answer.status)) }
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let word = dir_word(dir)?;
        let answer = self.send("GET", &format!("/sync/v1/list/{word}"), &[], &[], LIST_LIMIT)?;
        if answer.status.as_u16() == 200 { parse_listing(&answer.body) } else { Err(Self::fail(answer.status)) }
    }

    fn get(&self, path: &str) -> Result<Option<(Vec<u8>, String)>> {
        check_path(path)?;
        let answer = self.send("GET", &format!("/sync/v1/blob/{path}"), &[], &[], MAX_BLOB as u64)?;
        match answer.status.as_u16() {
            404 => Ok(None),
            200 => {
                // The server's ETag is the SHA-256 of the stored bytes; a server that leaves it out gets the same value.
                let etag = answer.etag.filter(|e| !e.is_empty() && e.len() <= MAX_ETAG).unwrap_or_else(|| sha256_hex(&answer.body));
                Ok(Some((answer.body, etag)))
            }
            _ => Err(Self::fail(answer.status)),
        }
    }

    fn put(&self, path: &str, bytes: &[u8], cond: &Cond) -> Result<String> {
        check_path(path)?;
        if bytes.len() > MAX_BLOB {
            return Err(err(TOO_LARGE));
        }
        let mut headers = vec![("Content-Type", "application/octet-stream".to_owned())];
        match cond {
            Cond::Absent => headers.push(("If-None-Match", "*".into())),
            Cond::Match(etag) => headers.push(("If-Match", format!("\"{etag}\""))),
            Cond::Any => {}
        }
        let answer = self.send("PUT", &format!("/sync/v1/blob/{path}"), &headers, bytes, SMALL_LIMIT)?;
        match answer.status.as_u16() {
            412 => Err(err(CONFLICT)),
            413 => Err(err(TOO_LARGE)),
            507 => Err(err(NO_SPACE)),
            200 | 201 | 204 => {
                Ok(answer.etag.filter(|e| !e.is_empty() && e.len() <= MAX_ETAG).unwrap_or_else(|| sha256_hex(bytes)))
            }
            _ => Err(Self::fail(answer.status)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// A token with the shape of a real one and text that is easy to find in any output.
    const TOKEN: &str = "hwd_canaryCANARY-0123456789_abcdefghijklmnopqrs";
    /// A word the fake puts into its refusals: it must never come back in an error.
    const SERVER_WORDS: &str = "server-words-that-must-not-be-echoed";

    fn token() -> Token {
        Token::parse(TOKEN).expect("the test token has the shape of a token")
    }

    #[derive(Clone, Debug)]
    struct Seen {
        method: String,
        target: String,
        /// Header names lowercased.
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
        }
    }

    /// How the fake answers.
    #[derive(Clone, Debug)]
    enum Mode {
        /// The contract, on a map of the files.
        Normal,
        /// This status to every request, with a body of server words.
        Status(u16),
        /// A redirect to this address.
        Redirect(String),
        /// A `200` that announces a length of a terabyte and sends nothing.
        Announces,
        /// A `200` without a length, as chunks, of this many bytes.
        Chunked(usize),
    }

    struct Fake {
        addr: std::net::SocketAddr,
        log: Arc<Mutex<Vec<Seen>>>,
        files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        mode: Arc<Mutex<Mode>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Let the accept loop see the flag.
            let _ = TcpStream::connect(self.addr);
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<Seen> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next()?.to_owned(), parts.next()?.to_owned());
        let mut headers = Vec::new();
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).ok()?;
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':')?;
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_owned());
            if k == "content-length" {
                length = v.parse().ok()?;
            }
            headers.push((k, v));
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).ok()?;
        Some(Seen { method, target, headers, body })
    }

    fn respond(stream: &mut TcpStream, status: u16, headers: &[(&str, String)], body: &[u8]) {
        let mut head = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (k, v) in headers {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    }

    fn json_list(files: &BTreeMap<String, Vec<u8>>, prefix: &str) -> String {
        let items: Vec<String> = files
            .iter()
            .filter_map(|(path, body)| {
                let name = path.strip_prefix(prefix)?;
                (!name.contains('/')).then(|| {
                    let sum = sha256_hex(body);
                    format!(r#"{{"name":"{name}","etag":"{sum}","checksum":"{sum}","extra":1}}"#)
                })
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    fn serve(mut stream: TcpStream, log: &Mutex<Vec<Seen>>, files: &Mutex<BTreeMap<String, Vec<u8>>>, mode: &Mutex<Mode>) {
        let Some(seen) = read_request(&mut stream) else { return };
        log.lock().unwrap().push(seen.clone());
        let mode = mode.lock().unwrap().clone();
        match mode {
            Mode::Status(status) => return respond(&mut stream, status, &[], SERVER_WORDS.as_bytes()),
            Mode::Redirect(to) => return respond(&mut stream, 302, &[("Location", to)], SERVER_WORDS.as_bytes()),
            Mode::Announces => {
                let _ = stream.write_all(b"HTTP/1.1 200 X\r\nContent-Length: 1099511627776\r\nConnection: close\r\n\r\n");
                return;
            }
            Mode::Chunked(total) => {
                let _ = stream.write_all(b"HTTP/1.1 200 X\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
                let chunk = vec![b'x'; total];
                let _ = stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes());
                let _ = stream.write_all(&chunk);
                let _ = stream.write_all(b"\r\n0\r\n\r\n");
                return;
            }
            Mode::Normal => {}
        }
        let mut files = files.lock().unwrap();
        let target = seen.target.as_str();
        if seen.method == "POST" && target.starts_with("/sync/v1/ensure/") {
            respond(&mut stream, 204, &[], b"");
        } else if let (true, Some(dir)) = (seen.method == "GET", target.strip_prefix("/sync/v1/list/")) {
            let prefix = if dir == "-" { String::new() } else { format!("{dir}/") };
            respond(&mut stream, 200, &[("Content-Type", "application/json".into())], json_list(&files, &prefix).as_bytes());
        } else if let Some(path) = target.strip_prefix("/sync/v1/blob/") {
            let path = path.to_owned();
            match seen.method.as_str() {
                "GET" => match files.get(&path) {
                    Some(body) => respond(&mut stream, 200, &[("ETag", format!("\"{}\"", sha256_hex(body)))], body),
                    None => respond(&mut stream, 404, &[], b""),
                },
                "PUT" => {
                    let current = files.get(&path).map(|b| sha256_hex(b));
                    let refused = match (seen.header("if-none-match"), seen.header("if-match")) {
                        (Some("*"), _) => current.is_some(),
                        (_, Some(m)) => current.as_deref() != Some(m.trim_matches('"')),
                        _ => false,
                    };
                    if refused {
                        respond(&mut stream, 412, &[], b"");
                    } else {
                        let etag = sha256_hex(&seen.body);
                        files.insert(path, seen.body.clone());
                        respond(&mut stream, 201, &[("ETag", format!("\"{etag}\""))], b"");
                    }
                }
                _ => respond(&mut stream, 405, &[], b""),
            }
        } else {
            respond(&mut stream, 404, &[], b"");
        }
    }

    fn fake() -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let addr = listener.local_addr().expect("address");
        let log: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let files: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
        let mode = Arc::new(Mutex::new(Mode::Normal));
        let stop = Arc::new(AtomicBool::new(false));
        let (log_in, files_in, mode_in, stop_in) = (log.clone(), files.clone(), mode.clone(), stop.clone());
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_in.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (log, files, mode) = (log_in.clone(), files_in.clone(), mode_in.clone());
                thread::spawn(move || serve(stream, &log, &files, &mode));
            }
        });
        Fake { addr, log, files, mode, stop }
    }

    impl Fake {
        fn backend(&self) -> ServerBackend {
            ServerBackend::new(&format!("http://{}", self.addr), &token()).expect("a loopback address is accepted")
        }

        fn set(&self, mode: Mode) {
            *self.mode.lock().unwrap() = mode;
        }

        fn seen(&self) -> Vec<Seen> {
            self.log.lock().unwrap().clone()
        }

        /// What the server holds, by path.
        fn stored(&self) -> BTreeMap<String, Vec<u8>> {
            self.files.lock().unwrap().clone()
        }
    }

    fn text_of(e: Error) -> String {
        e.to_string()
    }

    #[test]
    fn the_contract_is_spoken_for_ensure_and_list() {
        let fake = fake();
        let backend = fake.backend();
        for dir in ["", "files", "backups"] {
            backend.ensure(dir).expect("ensure");
        }
        let targets: Vec<_> = fake.seen().iter().map(|s| (s.method.clone(), s.target.clone())).collect();
        assert_eq!(
            targets,
            [("POST", "/sync/v1/ensure/-"), ("POST", "/sync/v1/ensure/files"), ("POST", "/sync/v1/ensure/backups")]
                .map(|(m, t)| (m.to_owned(), t.to_owned()))
        );
        assert_eq!(backend.list("files").expect("an empty folder"), Vec::new(), "an empty folder is an empty list");

        let etag = backend.put("files/a.hwx", b"sealed", &Cond::Absent).expect("put");
        backend.put("meta.json", b"{}", &Cond::Absent).expect("put meta");
        assert_eq!(etag, sha256_hex(b"sealed"));
        let listed = backend.list("files").expect("list");
        assert_eq!(listed, vec![Entry { name: "a.hwx".into(), etag: sha256_hex(b"sealed"), checksum: Some(sha256_hex(b"sealed")) }]);
        assert_eq!(listed[0].checksum.as_deref(), Some(listed[0].etag.as_str()), "the checksum is the etag");
        let root = backend.list("").expect("the root");
        assert_eq!(root.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["meta.json"]);
        assert!(fake.seen().iter().any(|s| s.target == "/sync/v1/list/-"));
        // The server holds the bytes it was given and nothing else.
        assert_eq!(fake.stored().get("files/a.hwx").map(Vec::as_slice), Some(&b"sealed"[..]));
        assert_eq!(fake.stored().len(), 2);
    }

    #[test]
    fn get_returns_the_bytes_and_the_etag_without_quotes_and_none_on_404() {
        let fake = fake();
        let backend = fake.backend();
        backend.put("backups/b.hwx", b"older", &Cond::Any).expect("put");
        let (bytes, etag) = backend.get("backups/b.hwx").expect("get").expect("the file exists");
        assert_eq!(bytes, b"older");
        assert_eq!(etag, sha256_hex(b"older"));
        assert!(!etag.contains('"'));
        assert_eq!(backend.get("files/none.hwx").expect("a missing file is not an error"), None);
    }

    #[test]
    fn put_sends_the_condition_in_a_header_and_a_failed_one_is_the_conflict_text() {
        let fake = fake();
        let backend = fake.backend();
        let first = backend.put("files/c.hwx", b"one", &Cond::Absent).expect("created");
        backend.put("files/c.hwx", b"two", &Cond::Match(first.clone())).expect("the etag matches");
        backend.put("files/c.hwx", b"three", &Cond::Any).expect("unconditional");

        let puts: Vec<_> = fake.seen().into_iter().filter(|s| s.method == "PUT").collect();
        assert_eq!(puts[0].header("if-none-match"), Some("*"));
        assert_eq!(puts[0].header("if-match"), None);
        assert_eq!(puts[1].header("if-match"), Some(format!("\"{first}\"").as_str()));
        assert_eq!(puts[1].header("if-none-match"), None);
        assert_eq!((puts[2].header("if-match"), puts[2].header("if-none-match")), (None, None));
        assert_eq!(puts[0].body, b"one");

        // The name exists: `Absent` fails. The etag is stale: `Match` fails. Both are exactly the WebDAV conflict text.
        let absent = backend.put("files/c.hwx", b"x", &Cond::Absent).unwrap_err();
        let stale = backend.put("files/c.hwx", b"x", &Cond::Match(first)).unwrap_err();
        assert_eq!(text_of(absent), CONFLICT);
        assert_eq!(text_of(stale), CONFLICT);
    }

    #[test]
    fn a_too_large_write_and_a_full_quota_have_their_own_fixed_sentences() {
        let fake = fake();
        let backend = fake.backend();
        fake.set(Mode::Status(413));
        assert_eq!(text_of(backend.put("files/a.hwx", b"x", &Cond::Any).unwrap_err()), TOO_LARGE);
        fake.set(Mode::Status(507));
        assert_eq!(text_of(backend.put("files/a.hwx", b"x", &Cond::Any).unwrap_err()), NO_SPACE);
        // A blob over the bound is not sent at all.
        fake.set(Mode::Normal);
        let before = fake.seen().len();
        assert_eq!(text_of(backend.put("files/a.hwx", &vec![0u8; MAX_BLOB + 1], &Cond::Any).unwrap_err()), TOO_LARGE);
        assert_eq!(fake.seen().len(), before);
    }

    #[test]
    fn a_401_and_a_404_are_typed_faults_without_a_server_text() {
        let fake = fake();
        let backend = fake.backend();
        fake.set(Mode::Status(401));
        for e in [
            backend.ensure("").unwrap_err(),
            backend.list("files").unwrap_err(),
            backend.get("meta.json").unwrap_err(),
            backend.put("files/a.hwx", b"x", &Cond::Any).unwrap_err(),
        ] {
            assert_eq!(text_of(e), Fault::SignedOut.sentence());
        }
        fake.set(Mode::Status(404));
        assert_eq!(text_of(backend.ensure("files").unwrap_err()), Fault::Off.sentence());
        assert_eq!(text_of(backend.list("files").unwrap_err()), Fault::Off.sentence());
        fake.set(Mode::Status(500));
        assert_eq!(text_of(backend.list("files").unwrap_err()), Fault::Refused.sentence());
        assert_eq!(text_of(backend.put("files/a.hwx", b"x", &Cond::Any).unwrap_err()), Fault::Refused.sentence());
    }

    #[test]
    fn a_redirect_is_not_followed_and_is_a_refusal() {
        let target = fake();
        let fake = fake();
        let backend = fake.backend();
        fake.set(Mode::Redirect(format!("http://{}/sync/v1/list/files", target.addr)));
        let e = backend.list("files").unwrap_err();
        assert_eq!(text_of(e), Fault::Refused.sentence());
        assert_eq!(fake.seen().len(), 1);
        assert!(target.seen().is_empty(), "the token was not sent where the redirect pointed");
    }

    #[test]
    fn a_plain_address_is_refused_except_for_loopback_and_so_is_anything_but_an_origin() {
        let t = token();
        assert!(ServerBackend::new("https://sync.example", &t).is_ok());
        assert!(ServerBackend::new("https://sync.example:8443/", &t).is_ok());
        assert!(ServerBackend::new("http://127.0.0.1:8080", &t).is_ok());
        assert!(ServerBackend::new("http://[::1]:8080", &t).is_ok());
        for bad in [
            "http://sync.example",
            "http://10.0.0.5:8080",
            "https://user:pw@sync.example",
            "https://sync.example/sync",
            "https://sync.example?x=1",
            "https://sync.example#x",
            "ftp://sync.example",
            "sync.example",
            "",
            "https://",
        ] {
            assert!(ServerBackend::new(bad, &t).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_answer_over_the_bound_is_refused_before_it_is_read_in_full() {
        let fake = fake();
        let backend = fake.backend();
        // Announced: a terabyte, no byte follows; the refusal does not wait for them.
        fake.set(Mode::Announces);
        let started = std::time::Instant::now();
        assert_eq!(text_of(backend.get("meta.json").unwrap_err()), Fault::Protocol.sentence());
        assert_eq!(text_of(backend.list("files").unwrap_err()), Fault::Protocol.sentence());
        assert!(started.elapsed() < Duration::from_secs(10));
        // Not announced (chunked): the read stops one byte past the bound.
        fake.set(Mode::Chunked(LIST_LIMIT as usize + 10));
        assert_eq!(text_of(backend.list("files").unwrap_err()), Fault::Protocol.sentence());
        // A listing that is not a JSON array, or has too many entries, is wrong.
        let many = format!("[{}]", vec![r#"{"name":"a","etag":"e"}"#; MAX_LIST_ENTRIES + 1].join(","));
        assert!(parse_listing(many.as_bytes()).is_err());
        assert!(parse_listing(b"{\"name\":\"a\"}").is_err());
        assert!(parse_listing(b"<html>").is_err());
        // One bad entry does not cost the rest.
        let mixed = br#"[{"name":"../x","etag":"e"},{"name":"ok.hwx","etag":"\"e1\""},{"name":"b.hwx","etag":""},{"name":"c.hwx","etag":"e2","checksum":"XYZ"}]"#;
        let got = parse_listing(mixed).expect("a listing with odd entries");
        assert_eq!(got.iter().map(|e| (e.name.as_str(), e.etag.as_str(), e.checksum.is_some())).collect::<Vec<_>>(), [("ok.hwx", "e1", false), ("c.hwx", "e2", false)]);
    }

    #[test]
    fn a_path_outside_the_contract_is_refused_before_any_request() {
        let fake = fake();
        let backend = fake.backend();
        for bad in ["", "..", "../x", "files", "files/", "files/a/b", "files/../a", "other/a.hwx", "meta.json/x", "/files/a", "files/a b", "meta", "files/%2e"] {
            assert!(backend.get(bad).is_err(), "get {bad:?}");
            assert!(backend.put(bad, b"x", &Cond::Any).is_err(), "put {bad:?}");
        }
        for bad in ["a", "files/a", "a/b", "-", "../files", "Files"] {
            assert!(backend.ensure(bad).is_err(), "ensure {bad:?}");
            assert!(backend.list(bad).is_err(), "list {bad:?}");
        }
        assert!(fake.seen().is_empty(), "no request was made");
    }

    #[test]
    fn the_token_is_only_in_the_authorization_header() {
        let fake = fake();
        let backend = fake.backend();
        backend.ensure("").unwrap();
        backend.put("files/a.hwx", b"sealed", &Cond::Absent).unwrap();
        backend.list("files").unwrap();
        backend.get("files/a.hwx").unwrap();
        let mut texts = vec![format!("{backend:?}")];
        for mode in [Mode::Status(401), Mode::Status(404), Mode::Status(500), Mode::Status(412), Mode::Announces] {
            fake.set(mode);
            texts.push(format!("{:?}", backend.list("files").unwrap_err()));
            texts.push(backend.get("meta.json").map_or_else(text_of, |_| String::new()));
            texts.push(text_of(backend.put("files/a.hwx", b"x", &Cond::Absent).unwrap_err()));
        }
        for text in &texts {
            assert!(!text.contains("canary") && !text.contains(SERVER_WORDS) && !text.contains("hwd_"), "{text}");
        }
        let seen = fake.seen();
        assert!(!seen.is_empty());
        for s in &seen {
            assert_eq!(s.header("authorization"), Some(format!("Bearer {TOKEN}").as_str()));
            assert!(!s.target.contains("canary"), "{}", s.target);
            assert!(!String::from_utf8_lossy(&s.body).contains("canary"));
            for (name, value) in &s.headers {
                if name != "authorization" {
                    assert!(!value.contains("canary"), "{name}");
                }
            }
        }
    }
}

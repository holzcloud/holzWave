//! The environment of the file-route tests: the WebSocket environment with the real gate and the real list of routes, people
//! who are signed in, media files in their homes, granted files, ranged and streamed requests, upload bodies and a checker for
//! the file system.
//!
//! Tests pull it in with `#[path = "support/edge.rs"] mod edge;`, `#[path = "support/edge_tls.rs"] mod edge_tls;`,
//! `#[path = "support/ws_env.rs"] mod ws_env;` and `#[path = "support/files_env.rs"] mod files_env;`. Everything here needs the
//! `in-process-workers` feature.
#![allow(dead_code)]
#![cfg(feature = "in-process-workers")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use holzwave_server::config::Upload;
use holzwave_server::limits::Limits;
use holzwave_server::worker::files::{FileHooks, FileProbe};
use holzwave_server::worker::runtime::TestHooks;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::edge::{ORIGIN, Signed};
use crate::edge_tls::{Conn, Reply, Request};
use crate::ws_env::{PATIENCE, Person, WsClient, WsEnv, WsOptions};

/// How a started environment differs from the default one.
#[derive(Default)]
pub struct Opts {
    /// A shorter idle time of a grant.
    pub grant_idle: Option<Duration>,
    /// A disk that is full after this many bytes of one upload.
    pub fail_write_after: Option<u64>,
    /// Each write of a piece waits this long.
    pub write_delay: Option<Duration>,
    /// Other limits of the application.
    pub limits: Option<Limits>,
    /// Other upload settings.
    pub upload: Option<Upload>,
}

/// A started server with the file routes.
pub struct FilesEnv {
    pub env: WsEnv,
    /// What the workers counted.
    pub probe: Arc<FileProbe>,
}

/// A grant as `file_grant` answered it.
#[derive(Debug, Clone)]
pub struct Granted {
    pub token: String,
    pub url: String,
    pub mime: String,
    pub size: u64,
}

impl FilesEnv {
    /// The default environment.
    pub async fn start() -> FilesEnv {
        FilesEnv::start_with(Opts::default()).await
    }

    /// An environment with the given options.
    pub async fn start_with(opts: Opts) -> FilesEnv {
        let probe = FileProbe::new();
        let hooks = TestHooks {
            files: FileHooks {
                grant_idle: opts.grant_idle,
                fail_write_after: opts.fail_write_after,
                write_delay: opts.write_delay,
                probe: Some(probe.clone()),
            },
            ..TestHooks::default()
        };
        let env = WsEnv::start_with(WsOptions {
            real_gate: true,
            hooks,
            limits: opts.limits,
            upload: opts.upload,
            ..WsOptions::default()
        })
        .await;
        FilesEnv { env, probe }
    }

    /// Makes the person `name` and signs them in.
    pub fn person(&self, name: &str) -> Person {
        self.env.person(name)
    }

    /// The home of `person`.
    pub fn home(&self, person: &Person) -> PathBuf {
        self.env.launcher.home(person.key.uid)
    }

    /// Opens a WebSocket for `person`, which starts their worker. Keep the value for as long as the worker is needed.
    pub async fn worker(&self, person: &Person) -> WsClient {
        let mut client = self.env.connect_as(&person.signed).await;
        client.ok("get_layout", "{}").await;
        client
    }

    /// Grants the file at `path` to the person behind `client`.
    pub async fn grant(&self, client: &mut WsClient, path: &Path) -> Granted {
        let args = json!({ "source": "local", "path": path.to_str().expect("a UTF-8 path") }).to_string();
        let answer = client.ok("file_grant", &args).await;
        let token = answer["token"].as_str().expect("a token").to_owned();
        let url = answer["url"].as_str().expect("a url").to_owned();
        assert_eq!(url, format!("/files/{token}"), "the address of a grant");
        Granted { token, url, mime: answer["mime"].as_str().unwrap_or_default().to_owned(), size: answer["size"].as_u64().unwrap_or(0) }
    }

    /// A GET for `target` with the cookie of `who` and, when given, a `Range` header.
    pub fn get_request(&self, who: Option<&Signed>, target: &str, range: Option<&str>) -> Request {
        let mut request = Request::get(target).close();
        if let Some(who) = who {
            request = request.cookie(&who.cookie);
        }
        if let Some(range) = range {
            request = request.header("Range", range);
        }
        request
    }

    /// Sends `request` on a fresh connection and reads the whole answer.
    pub async fn send(&self, request: Request) -> Reply {
        self.env.plain(request).await
    }

    /// Downloads `target` as `who`.
    pub async fn get(&self, who: &Signed, target: &str) -> Reply {
        self.send(self.get_request(Some(who), target, None)).await
    }

    /// Downloads `target` as `who` with a `Range` header.
    pub async fn get_range(&self, who: &Signed, target: &str, range: &str) -> Reply {
        self.send(self.get_request(Some(who), target, Some(range))).await
    }

    /// The upload request a page sends: the cookie, the public `Origin`, the token and the octet-stream type.
    pub fn upload_request(&self, who: &Signed, query: &str, body: &[u8]) -> Request {
        let mut request = Request::new("PUT", &format!("/api/upload?{query}"))
            .close()
            .cookie(&who.cookie)
            .header("Origin", ORIGIN)
            .header("X-CSRF-Token", &who.csrf)
            .header("Content-Type", "application/octet-stream");
        if body.is_empty() {
            request = request.header("Content-Length", "0");
        }
        request.body = body.to_vec();
        request
    }

    /// Uploads `body` as `name` as `who`.
    pub async fn upload(&self, who: &Signed, name: &str, body: &[u8]) -> Reply {
        let query = format!("name={}", encode(name));
        self.send(self.upload_request(who, &query, body)).await
    }

    /// Starts an upload that declares `declared` bytes: the head is sent, the body is not. The test writes the body with
    /// `conn.send` and reads the answer with `conn.reply(false)`.
    pub async fn begin_upload(&self, who: &Signed, query: &str, declared: u64) -> Conn<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
        let mut head = self.upload_request(who, query, &[]);
        head.headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-length"));
        head = head.header("Content-Length", &declared.to_string());
        let mut conn = self.env.connection().await;
        conn.send(&head.bytes()).await;
        conn
    }

    /// Writes a file into the home of `person` (the folders are made) and returns its path.
    pub fn put_file(&self, person: &Person, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.home(person).join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the folders");
        }
        std::fs::write(&path, bytes).expect("the file");
        path
    }

    /// A PNG of `len` bytes (at least 8) in the home of `person`.
    pub fn put_png(&self, person: &Person, relative: &str, len: usize) -> PathBuf {
        self.put_file(person, relative, &png(len))
    }

    /// A WAV file of `len` bytes in the home of `person`, written without holding it in memory.
    pub fn put_wav(&self, person: &Person, relative: &str, len: u64) -> PathBuf {
        let path = self.home(person).join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the folders");
        }
        let mut file = std::fs::File::create(&path).expect("the file");
        let mut written = 0u64;
        let mut chunk = vec![0u8; 1 << 20];
        while written < len {
            let take = usize::try_from((len - written).min(chunk.len() as u64)).expect("small");
            for (i, byte) in chunk[..take].iter_mut().enumerate() {
                *byte = wav_byte(written + i as u64);
            }
            file.write_all(&chunk[..take]).expect("the bytes");
            written += take as u64;
        }
        path
    }
}

/// Percent-encodes `text` for a query value: everything but unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A PNG signature and then bytes that depend on their place, `len` bytes in all.
pub fn png(len: usize) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut i = 0usize;
    while out.len() < len {
        out.push(u8::try_from((i * 7 + 3) % 251).expect("a byte"));
        i += 1;
    }
    out.truncate(len);
    out
}

/// The byte at `at` of a test WAV file: the header of a WAV file, then bytes that depend on their place.
pub fn wav_byte(at: u64) -> u8 {
    const HEAD: &[u8; 12] = b"RIFF\x00\x00\x00\x00WAVE";
    match usize::try_from(at) {
        Ok(at) if at < HEAD.len() => HEAD[at],
        _ => u8::try_from((at.wrapping_mul(31).wrapping_add(7)) % 253).expect("a byte"),
    }
}

/// The bytes `start..start + len` of a test WAV file.
pub fn wav_bytes(start: u64, len: u64) -> Vec<u8> {
    (start..start + len).map(wav_byte).collect()
}

/// The five headers every response of the download route carries, with their values.
pub const FILE_HEADERS: [(&str, &str); 4] = [
    ("x-content-type-options", "nosniff"),
    ("cache-control", "no-store"),
    ("content-security-policy", "default-src 'none'; sandbox"),
    ("cross-origin-resource-policy", "same-site"),
];

/// Asserts the five security headers of the file route on `reply`.
pub fn assert_file_headers(reply: &Reply) {
    for (name, value) in FILE_HEADERS {
        assert_eq!(reply.all(name), vec![value], "the header {name} on a {} answer", reply.status);
    }
    assert_eq!(reply.all("accept-ranges"), vec!["bytes"], "accept-ranges on a {} answer", reply.status);
}

// ---- the file system ------------------------------------------------------------------------------------------------------

/// The names in `dir`, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Every file below `root`, with its path relative to `root`, size and mode, links not followed.
pub fn tree(root: &Path) -> Vec<(String, u64, u32)> {
    use std::os::unix::fs::PermissionsExt;
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, u64, u32)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().into_owned();
            out.push((relative, meta.len(), meta.permissions().mode() & 0o7777));
            if meta.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The files below `root` whose bytes hold `needle`.
pub fn files_holding(root: &Path, needle: &[u8]) -> Vec<PathBuf> {
    fn walk(dir: &Path, needle: &[u8], out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                walk(&path, needle, out);
            } else if meta.is_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    if bytes.windows(needle.len()).any(|w| w == needle) {
                        out.push(path);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(root, needle, &mut out);
    out
}

// ---- a download read by hand --------------------------------------------------------------------------------------------

/// A GET whose body the test reads at its own speed.
pub struct SlowGet {
    pub stream: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Body bytes that came with the head.
    pub early: Vec<u8>,
}

impl SlowGet {
    /// Sends `request` and reads the head only.
    pub async fn start(env: &FilesEnv, request: Request) -> SlowGet {
        let mut conn = env.env.connection().await;
        conn.send(&request.bytes()).await;
        let mut stream = conn.stream;
        let mut buffer = Vec::new();
        let end = loop {
            if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break at;
            }
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(PATIENCE, stream.read(&mut chunk)).await.expect("a head in time").expect("read");
            assert!(n > 0, "the connection ended before a head");
            buffer.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let status: u16 = lines.next().and_then(|l| l.split(' ').nth(1)).and_then(|s| s.parse().ok()).expect("a status");
        let headers = lines
            .filter_map(|line| line.split_once(':').map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned())))
            .collect();
        SlowGet { stream, status, headers, early: buffer[end + 4..].to_vec() }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// Reads up to `max` bytes of the body; 0 at the end of the connection.
    pub async fn read(&mut self, max: usize) -> usize {
        let mut chunk = vec![0u8; max];
        match tokio::time::timeout(PATIENCE, self.stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            _ => 0,
        }
    }

    /// Reads to the end of the connection and returns all body bytes, the early ones included.
    pub async fn rest(&mut self) -> Vec<u8> {
        let mut out = std::mem::take(&mut self.early);
        let mut chunk = vec![0u8; 64 * 1024];
        while let Ok(Ok(n)) = tokio::time::timeout(PATIENCE, self.stream.read(&mut chunk)).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        out
    }

    /// Closes the connection.
    pub async fn close(mut self) {
        let _ = self.stream.shutdown().await;
    }
}

/// Shorthand: the JSON of a reply.
pub fn json(reply: &Reply) -> Value {
    reply.json()
}

//! TLS termination from certificate files with rustls on the ring provider.
//!
//! The server speaks TLS 1.3 only, with the suites of the ring provider, no client certificate, no early data (0-RTT) and
//! the ALPN list `h2` then `http/1.1`. The certificate is served for any server name. Nothing in this file parses a
//! certificate or a key by hand: PEM goes through `rustls-pki-types`, keys and chains through `rustls`.
//!
//! A refusal is one sentence that names the file and the fix. No sentence, `Debug` text or log line holds a byte of the
//! key file or of a peer.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::{ServerConfig, version::TLS13};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use zeroize::Zeroizing;

use crate::keyfile::{self, shown};
use crate::log::{self, Level};

/// The largest certificate file or key file that is read, in bytes. A longer file is not a certificate or a key.
pub const MAX_PEM_BYTES: u64 = 256 * 1024;

/// The most certificates a chain file may hold: a leaf and the intermediates of any real hierarchy.
pub const MAX_CHAIN: usize = 8;

/// How often the files are looked at for a change.
pub const RELOAD_INTERVAL: Duration = Duration::from_secs(60);

/// The ALPN protocols the server offers, most preferred first.
pub const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// The name the sentences use for the key file.
const KEY_FILE: &str = "TLS key file";

/// The name the sentences use for the certificate file.
const CERT_FILE: &str = "TLS certificate file";

/// A refusal: one sentence that names the file and the fix. It never holds file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsError(String);

impl TlsError {
    fn new(sentence: String) -> TlsError {
        TlsError(sentence)
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TlsError {}

/// The crypto provider: ring, and nothing else.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Reads at most [`MAX_PEM_BYTES`] of `file` into memory that is wiped when it is dropped.
fn read_bounded(file: &std::fs::File, path: &Path, what: &str) -> Result<Zeroizing<Vec<u8>>, TlsError> {
    let mut content = Zeroizing::new(Vec::new());
    file.take(MAX_PEM_BYTES + 1).read_to_end(&mut content).map_err(|e| {
        TlsError::new(format!("the {what} {} cannot be read ({}); check that the server may read it", shown(path), e.kind()))
    })?;
    if content.len() as u64 > MAX_PEM_BYTES {
        return Err(TlsError::new(format!(
            "the {what} {} is larger than {} KiB, which no {what} is; check that the setting names the right file",
            shown(path),
            MAX_PEM_BYTES / 1024
        )));
    }
    Ok(content)
}

/// Opens the certificate file: a public file, so a link is followed (a mounted secret is a link), but it must be a regular
/// file and opening it never blocks on a pipe.
fn open_public(path: &Path) -> Result<std::fs::File, TlsError> {
    let nonblock = i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap_or(0);
    let file = OpenOptions::new().read(true).custom_flags(nonblock).open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            TlsError::new(format!("the {CERT_FILE} {} does not exist; check tls.cert_file", shown(path)))
        } else {
            TlsError::new(format!(
                "the {CERT_FILE} {} cannot be opened ({}); check the path and that the server may read it",
                shown(path),
                e.kind()
            ))
        }
    })?;
    let regular = file.metadata().map(|m| m.file_type().is_file()).unwrap_or(false);
    if !regular {
        return Err(TlsError::new(format!("the {CERT_FILE} {} is not a regular file; name a regular file", shown(path))));
    }
    Ok(file)
}

/// Builds a certified key from a PEM certificate chain and a PEM private key.
///
/// The key file is opened with the same check as the server key: a regular file, no link, no group write and no access for
/// others. The key must match the first certificate.
pub fn load_certified_key(cert_file: &Path, key_file: &Path) -> Result<CertifiedKey, TlsError> {
    let cert_pem = {
        let file = open_public(cert_file)?;
        read_bounded(&file, cert_file, CERT_FILE)?
    };
    let key_pem = {
        let file = keyfile::open_private(key_file, KEY_FILE).map_err(|e| TlsError::new(e.sentence().to_owned()))?;
        read_bounded(&file, key_file, KEY_FILE)?
    };

    let damaged = |what: &str, path: &Path| {
        TlsError::new(format!("the {what} {} holds a damaged PEM block; create the file again", shown(path)))
    };
    let mut chain: Vec<CertificateDer<'static>> = Vec::new();
    for item in CertificateDer::pem_slice_iter(&cert_pem) {
        let der = item.map_err(|_| damaged(CERT_FILE, cert_file))?;
        if chain.len() == MAX_CHAIN {
            return Err(TlsError::new(format!(
                "the {CERT_FILE} {} holds more than {MAX_CHAIN} certificates; keep the leaf and the intermediates only",
                shown(cert_file)
            )));
        }
        chain.push(der);
    }
    if chain.is_empty() {
        return Err(TlsError::new(format!(
            "the {CERT_FILE} {} holds no certificate; it needs a \"BEGIN CERTIFICATE\" block",
            shown(cert_file)
        )));
    }

    if has(&key_pem, b"-----BEGIN ENCRYPTED PRIVATE KEY-----") || has(&key_pem, b"Proc-Type: 4,ENCRYPTED") {
        return Err(TlsError::new(format!(
            "the {KEY_FILE} {} is encrypted and the server cannot ask for a password; decrypt the key (openssl pkcs8 -in encrypted.pem -out key.pem) and keep the file at mode 0600",
            shown(key_file)
        )));
    }
    let mut keys = PrivateKeyDer::pem_slice_iter(&key_pem);
    let key = match keys.next() {
        None => {
            return Err(TlsError::new(format!(
                "the {KEY_FILE} {} holds no private key; it needs a \"BEGIN PRIVATE KEY\" block",
                shown(key_file)
            )));
        }
        Some(Err(_)) => return Err(damaged(KEY_FILE, key_file)),
        Some(Ok(key)) => key,
    };
    if keys.next().is_some() {
        return Err(TlsError::new(format!(
            "the {KEY_FILE} {} holds more than one private key; keep the one key of the certificate",
            shown(key_file)
        )));
    }
    let signing = provider().key_provider.load_private_key(key).map_err(|_| {
        TlsError::new(format!(
            "the {KEY_FILE} {} holds a key of a type this server does not support; use an RSA, ECDSA P-256, P-384 or Ed25519 key",
            shown(key_file)
        ))
    })?;
    let certified = CertifiedKey::new(chain, signing);
    certified.keys_match().map_err(|_| {
        TlsError::new(format!(
            "the {KEY_FILE} {} does not match the first certificate in {}; use the key that was made for that certificate",
            shown(key_file),
            shown(cert_file)
        ))
    })?;
    Ok(certified)
}

/// Whether `needle` occurs in `haystack`.
fn has(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The hex SHA-256 of the DER of a certificate.
fn fingerprint_of(cert: &CertificateDer<'_>) -> String {
    Sha256::digest(cert.as_ref()).iter().map(|b| format!("{b:02x}")).collect()
}

/// What the resolver serves now.
struct Loaded {
    key: Arc<CertifiedKey>,
    fingerprint: String,
}

/// What the file system says about one file: modification time, length and inode. `None` when it cannot be examined.
type Stamp = Option<(i64, i64, u64, u64)>;

fn stamp_of(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.mtime(), meta.mtime_nsec(), meta.len(), meta.ino()))
}

/// The result of a reload that did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// The files hold a new, valid pair, and the next handshake serves it.
    Changed,
    /// The files hold the pair that is served already.
    Unchanged,
}

/// Serves the certified key that was loaded from two files, and loads it again when the files change.
pub struct FileResolver {
    cert_file: PathBuf,
    key_file: PathBuf,
    current: RwLock<Arc<Loaded>>,
    stamps: Mutex<(Stamp, Stamp)>,
}

impl fmt::Debug for FileResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileResolver")
            .field("cert_file", &self.cert_file)
            .field("key_file", &self.key_file)
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

impl FileResolver {
    /// Loads the pair; the first load must succeed.
    pub fn new(cert_file: &Path, key_file: &Path) -> Result<FileResolver, TlsError> {
        let stamps = (stamp_of(cert_file), stamp_of(key_file));
        let key = load_certified_key(cert_file, key_file)?;
        let fingerprint = leaf_fingerprint(&key);
        Ok(FileResolver {
            cert_file: cert_file.to_owned(),
            key_file: key_file.to_owned(),
            current: RwLock::new(Arc::new(Loaded { key: Arc::new(key), fingerprint })),
            stamps: Mutex::new(stamps),
        })
    }

    fn loaded(&self) -> Arc<Loaded> {
        self.current.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The certified key served now.
    pub fn current(&self) -> Arc<CertifiedKey> {
        self.loaded().key.clone()
    }

    /// The hex SHA-256 of the leaf certificate served now.
    pub fn fingerprint(&self) -> String {
        self.loaded().fingerprint.clone()
    }

    /// Reads both files again. A valid pair that differs from the served one replaces it for the next handshake (a
    /// connection that is open keeps what it negotiated); an invalid or mismatching pair leaves the served pair in place and
    /// returns the sentence. Every outcome writes one log line.
    pub fn reload(&self) -> Result<Reload, TlsError> {
        // The stamps are taken before the files are read, so a change during the read is seen by the next look.
        let stamps = (stamp_of(&self.cert_file), stamp_of(&self.key_file));
        let outcome = self.load_and_swap();
        *self.stamps.lock().unwrap_or_else(PoisonError::into_inner) = stamps;
        match &outcome {
            Ok(Reload::Changed) => log::event(Level::Info, "tls_reload_ok", &[("fingerprint", &self.fingerprint())]),
            Ok(Reload::Unchanged) => log::event(Level::Info, "tls_reload_unchanged", &[]),
            Err(error) => log::event(Level::Warn, "tls_reload_failed", &[("reason", error.sentence())]),
        }
        outcome
    }

    fn load_and_swap(&self) -> Result<Reload, TlsError> {
        let key = load_certified_key(&self.cert_file, &self.key_file)?;
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
        // The whole chain is compared, so a renewed intermediate counts as a change although the leaf is the same.
        if current.key.cert == key.cert {
            return Ok(Reload::Unchanged);
        }
        let fingerprint = leaf_fingerprint(&key);
        *current = Arc::new(Loaded { key: Arc::new(key), fingerprint });
        Ok(Reload::Changed)
    }

    /// Reloads when the modification time, the length or the inode of either file differs from the last look.
    pub fn poll(&self) -> Option<Result<Reload, TlsError>> {
        let now = (stamp_of(&self.cert_file), stamp_of(&self.key_file));
        let last = *self.stamps.lock().unwrap_or_else(PoisonError::into_inner);
        (now != last).then(|| self.reload())
    }
}

/// The task that looks at the files. Dropping the handle stops it.
pub struct Reloader(tokio::task::JoinHandle<()>);

impl Reloader {
    /// Whether the task has ended.
    pub fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Drop for Reloader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts a task that calls [`FileResolver::poll`] every `every` (use [`RELOAD_INTERVAL`]). The files are read on the
/// blocking pool, so a slow disk never holds up the runtime. It must be called inside a Tokio runtime.
pub fn spawn_reloader(resolver: Arc<FileResolver>, every: Duration) -> Reloader {
    Reloader(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate; the files were just loaded.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let resolver = Arc::clone(&resolver);
            let _ = tokio::task::spawn_blocking(move || {
                let _ = resolver.poll();
            })
            .await;
        }
    }))
}

/// The fingerprint of the first certificate of a key (empty only for a chain without a certificate, which cannot be built).
fn leaf_fingerprint(key: &CertifiedKey) -> String {
    key.cert.first().map(fingerprint_of).unwrap_or_default()
}

impl ResolvesServerCert for FileResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current())
    }
}

/// The server configuration: TLS 1.3 only, no client certificate, no early data, the given ALPN list, and the certificate
/// of `resolver` for any server name.
pub fn server_config(resolver: Arc<dyn ResolvesServerCert>, alpn: &[&[u8]]) -> Result<ServerConfig, TlsError> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&TLS13])
        .map_err(|_| TlsError::new("the TLS 1.3 server configuration cannot be built".to_owned()))?
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config.max_early_data_size = 0;
    Ok(config)
}

/// The acceptor of a configuration.
pub fn acceptor(config: Arc<ServerConfig>) -> TlsAcceptor {
    TlsAcceptor::from(config)
}

/// Why a handshake did not finish. The texts are fixed; no peer byte is in them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// The handshake did not finish within the bound.
    TimedOut,
    /// The peer sent something that is not a TLS 1.3 handshake, or offered nothing the server accepts.
    Failed,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::TimedOut => f.write_str("the TLS handshake did not finish in time"),
            HandshakeError::Failed => f.write_str("the TLS handshake failed"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// Runs the TLS handshake on `stream` and gives up after `timeout` in total, however slowly the peer sends. On an error the
/// stream is dropped, which closes the connection; nothing has been served.
pub async fn accept_tls<S>(acceptor: &TlsAcceptor, stream: S, timeout: Duration) -> Result<TlsStream<S>, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, acceptor.accept(stream)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(_)) => Err(HandshakeError::Failed),
        Err(_) => Err(HandshakeError::TimedOut),
    }
}

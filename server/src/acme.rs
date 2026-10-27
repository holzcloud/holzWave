//! Certificate requests and renewal over ACME for the server's own host name.
//!
//! The only challenge the server answers is TLS-ALPN-01 (RFC 8737), on its own TLS port: a client that offers the ALPN
//! protocol `acme-tls/1` and the server name of a pending domain gets a self-signed certificate for that name that holds the
//! critical `acmeIdentifier` extension (1.3.6.1.5.5.7.1.31) with the SHA-256 digest of the key authorization. Nothing else is
//! added to the network surface: there is no plain-HTTP listener, no port 80, and no other challenge type.
//!
//! [`AcmeResolver`] is the certificate resolver of the TLS acceptor. It serves the challenge certificate for a pending name
//! to a challenge handshake, and the issued certificate to every other handshake; before the first issuance a normal
//! handshake fails. A connection that negotiated `acme-tls/1` is recognised by [`is_challenge`] so that the listener closes
//! it after the handshake without serving HTTP.
//!
//! No private key, account credential or challenge secret is in any sentence, `Debug` text or log line.

use std::collections::HashMap;
use std::fmt;
use std::fs::{DirBuilder, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration as StdDuration, SystemTime};

use instant_acme::{Account, AccountBuilder, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus};
use rcgen::{CertificateParams, CustomExtension, DistinguishedName, KeyPair};
use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};
use zeroize::Zeroizing;

use crate::clock::{Clock, unix_seconds};
use crate::config::Refusal;
use crate::keyfile::{self, ServerKey, shown};
use crate::log::{self, Level};
use crate::sealed::{self, Sealer};

/// The ALPN protocol of the TLS-ALPN-01 challenge.
pub const CHALLENGE_PROTOCOL: &[u8] = b"acme-tls/1";

/// The ALPN list of an ACME listener: the challenge protocol first, then what the application speaks.
pub const ALPN: [&[u8]; 3] = [CHALLENGE_PROTOCOL, b"h2", b"http/1.1"];

/// The most challenges that are pending at once, which is the most names a certificate may hold.
pub const MAX_PENDING: usize = 10;

/// How long before now a challenge certificate is valid from (a clock that runs behind must still accept it).
const CHALLENGE_BACKDATE: Duration = Duration::hours(1);

/// How long a challenge certificate is valid: one day, far more than a challenge lasts.
const CHALLENGE_LIFETIME: Duration = Duration::days(1);

/// Why an ACME step did not work: one sentence that says what happened and what to check. It never holds a key, a
/// credential or a challenge value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeError(String);

impl AcmeError {
    /// An error with this sentence. Control characters are replaced, so a sentence is always one line.
    pub fn new(sentence: impl Into<String>) -> AcmeError {
        AcmeError(sentence.into().chars().map(|c| if c.is_control() { ' ' } else { c }).collect())
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AcmeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AcmeError {}

/// The crypto provider: ring, and nothing else.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A certified key from a generated key pair and the DER of its certificate.
fn certified(cert_der: Vec<u8>, key: &KeyPair) -> Result<CertifiedKey, AcmeError> {
    let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let signing = provider()
        .key_provider
        .load_private_key(der)
        .map_err(|_| AcmeError::new("the generated certificate key cannot be used for signing"))?;
    Ok(CertifiedKey::new(vec![cert_der.into()], signing))
}

/// The challenge certificate of RFC 8737 section 3 for `name`: self-signed with a fresh P-256 key, the name as the only
/// subject alternative name, the critical `acmeIdentifier` extension with the digest, valid for one day. The key lives only
/// as long as the returned value.
fn challenge_certificate(name: &str, digest: &[u8; 32]) -> Result<CertifiedKey, AcmeError> {
    let failed = |what: &str| AcmeError::new(format!("the challenge certificate cannot be made ({what})"));
    let mut params = CertificateParams::new(vec![name.to_owned()]).map_err(|_| failed("the name"))?;
    params.distinguished_name = DistinguishedName::new();
    params.custom_extensions.push(CustomExtension::new_acme_identifier(digest));
    let now = OffsetDateTime::now_utc();
    params.not_before = now - CHALLENGE_BACKDATE;
    params.not_after = now + CHALLENGE_LIFETIME;
    let key = KeyPair::generate().map_err(|_| failed("the key"))?;
    let cert = params.self_signed(&key).map_err(|_| failed("the signature"))?;
    certified(cert.der().to_vec(), &key)
}

/// Whether the protocol a handshake negotiated is the challenge protocol. The listener closes such a connection after the
/// handshake and never serves HTTP on it.
pub fn is_challenge(negotiated: Option<&[u8]>) -> bool {
    negotiated == Some(CHALLENGE_PROTOCOL)
}

/// Serves the challenge certificate to a challenge handshake and the issued certificate to every other handshake.
///
/// The challenge answer is decided by the ALPN offer and the server name only: a client that offers `acme-tls/1` for any
/// name that is not pending (or when nothing is pending) gets no certificate, and the handshake fails the same way as when
/// no certificate has been issued yet.
pub struct AcmeResolver {
    pending: Mutex<HashMap<String, Arc<CertifiedKey>>>,
    issued: RwLock<Option<Arc<CertifiedKey>>>,
}

impl fmt::Debug for AcmeResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcmeResolver")
            .field("pending", &self.pending_count())
            .field("issued", &self.has_certificate())
            .finish()
    }
}

impl Default for AcmeResolver {
    fn default() -> Self {
        AcmeResolver::new()
    }
}

impl AcmeResolver {
    /// A resolver with no pending challenge and no issued certificate.
    pub fn new() -> AcmeResolver {
        AcmeResolver { pending: Mutex::new(HashMap::new()), issued: RwLock::new(None) }
    }

    /// Makes `name` answer a challenge handshake with a certificate that holds `digest`. The name is lower-cased. A second
    /// call for the same name replaces the first. At most [`MAX_PENDING`] names are pending.
    pub fn set_challenge(&self, name: &str, digest: [u8; 32]) -> Result<(), AcmeError> {
        let name = name.to_ascii_lowercase();
        if !crate::config::valid_dns_name(&name) {
            return Err(AcmeError::new("a challenge needs a host name; the name given is not one"));
        }
        let key = Arc::new(challenge_certificate(&name, &digest)?);
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.len() >= MAX_PENDING && !pending.contains_key(&name) {
            return Err(AcmeError::new(format!("more than {MAX_PENDING} challenges are pending at once")));
        }
        pending.insert(name, key);
        Ok(())
    }

    /// Removes the challenge of `name`, and its key with it.
    pub fn clear_challenge(&self, name: &str) {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&name.to_ascii_lowercase());
    }

    /// How many challenges are pending.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Serves `key` to every following handshake that is not a challenge; connections that are open keep what they
    /// negotiated.
    pub fn set_certified_key(&self, key: Arc<CertifiedKey>) {
        *self.issued.write().unwrap_or_else(PoisonError::into_inner) = Some(key);
    }

    /// Whether a certificate has been issued (or loaded from the cache) and is served.
    pub fn has_certificate(&self) -> bool {
        self.issued.read().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// The certified key served to normal handshakes, if there is one.
    pub fn certified_key(&self) -> Option<Arc<CertifiedKey>> {
        self.issued.read().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl ResolvesServerCert for AcmeResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let challenge = client_hello.alpn().is_some_and(|mut offered| offered.any(|p| p == CHALLENGE_PROTOCOL));
        if challenge {
            let name = client_hello.server_name()?.to_ascii_lowercase();
            return self.pending.lock().unwrap_or_else(PoisonError::into_inner).get(&name).cloned();
        }
        self.certified_key()
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The operator's settings
// ---------------------------------------------------------------------------------------------------------------------

/// The most names one certificate holds.
pub const MAX_DOMAINS: usize = 10;

/// The longest contact address, in bytes (RFC 5321 path limit).
pub const MAX_CONTACT_BYTES: usize = 254;

/// The longest directory address, in bytes.
pub const MAX_DIRECTORY_BYTES: usize = 2048;

/// The operator, not the software, agrees to the terms of the certificate authority: `true` and nothing else passes.
pub fn check_terms(terms_agreed: Option<bool>, directory: &str) -> Result<(), Refusal> {
    if terms_agreed == Some(true) {
        return Ok(());
    }
    Err(Refusal::new(format!(
        "tls mode \"acme\" needs tls.terms_agreed set to true: by setting it you, the operator, agree to the terms of service of the certificate authority at {}; the server never agrees for you",
        printable(directory)
    )))
}

/// The names of the certificate: from one to ten host names in lower case, none twice, no wildcard and no IP address, and
/// the host of `public_url` among them. A wildcard cannot be proven with TLS-ALPN-01, and a name that the browser does not
/// type would leave the address people use without a certificate.
pub fn check_domains(domains: &[String], public_host: Option<&str>) -> Result<(), Refusal> {
    if domains.is_empty() {
        return Err(Refusal::new("tls mode \"acme\" needs at least one name in tls.domains"));
    }
    if domains.len() > MAX_DOMAINS {
        return Err(Refusal::new(format!("tls.domains lists more than {MAX_DOMAINS} names; one certificate holds {MAX_DOMAINS} at most")));
    }
    let mut seen: Vec<&str> = Vec::with_capacity(domains.len());
    for name in domains {
        if name.starts_with("*.") || name == "*" {
            return Err(Refusal::new(format!(
                "tls.domains entry {name:?} is a wildcard; the challenge this server answers cannot prove a wildcard, so name every host"
            )));
        }
        if name.parse::<IpAddr>().is_ok() {
            return Err(Refusal::new(format!(
                "tls.domains entry {name:?} is an IP address; a certificate from ACME is issued for host names"
            )));
        }
        if !crate::config::valid_dns_name(name) {
            return Err(Refusal::new(format!(
                "tls.domains entry {name:?} is not a host name in lower case (a certificate cannot be issued for it)"
            )));
        }
        if seen.contains(&name.as_str()) {
            return Err(Refusal::new(format!("tls.domains lists {name:?} twice")));
        }
        seen.push(name);
    }
    if let Some(host) = public_host
        && !seen.contains(&host)
    {
        return Err(Refusal::new(format!(
            "the host {host:?} of public_url is not in tls.domains; the certificate must cover the address people type"
        )));
    }
    Ok(())
}

/// The contact of the account as the CA wants it, `mailto:<address>`. The address is a plain e-mail address: letters,
/// digits and `. _ + -` before one `@`, a host name after it, at most 254 bytes, no space and no control character. An
/// optional `mailto:` in front is accepted.
pub fn normalise_contact(contact: &str) -> Result<String, Refusal> {
    let bad = || {
        Refusal::new(format!(
            "tls.contact {:?} is not an email address such as mailto:admin@example.org",
            printable(contact)
        ))
    };
    let address = contact.trim();
    let address = address.strip_prefix("mailto:").unwrap_or(address);
    if address.is_empty() || address.len() > MAX_CONTACT_BYTES {
        return Err(bad());
    }
    let mut parts = address.split('@');
    let (Some(local), Some(host), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad());
    };
    let local_ok = !local.is_empty()
        && local.len() <= 64
        && local.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
    let host_ok = crate::config::valid_dns_name(&host.to_ascii_lowercase());
    if !local_ok || !host_ok {
        return Err(bad());
    }
    Ok(format!("mailto:{address}"))
}

/// The directory address of the CA: https, one line, no space.
pub fn check_directory(directory: &str) -> Result<(), Refusal> {
    let ok = directory.starts_with("https://")
        && directory.len() > "https://".len()
        && directory.len() <= MAX_DIRECTORY_BYTES
        && !directory.chars().any(|c| c.is_control() || c.is_whitespace());
    if ok {
        Ok(())
    } else {
        Err(Refusal::new(format!("tls.directory_url {:?} must be an https address", printable(directory))))
    }
}

/// A text as it appears in a sentence: control characters replaced, long text cut.
fn printable(text: &str) -> String {
    text.chars().take(120).map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// What the operator asked for, after the checks. Nothing in it is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeSettings {
    /// The ACME directory, an https address.
    pub directory_url: String,
    /// The contact of the account as `mailto:<address>`.
    pub contact: String,
    /// The names of the certificate, lower case.
    pub domains: Vec<String>,
    /// A trusted root for the connection to the CA, and for nothing else.
    pub ca_file: Option<PathBuf>,
}

impl AcmeSettings {
    /// Checks the settings before any network call. Domain names are lower-cased first. `terms_agreed` must be `true`, and
    /// the host of `public_url` (when given) must be one of the domains.
    pub fn new(
        directory_url: &str,
        contact: &str,
        domains: &[String],
        ca_file: Option<PathBuf>,
        terms_agreed: bool,
        public_host: Option<&str>,
    ) -> Result<AcmeSettings, Refusal> {
        check_terms(Some(terms_agreed), directory_url)?;
        let domains: Vec<String> = domains.iter().map(|d| d.to_ascii_lowercase()).collect();
        let public_host = public_host.map(str::to_ascii_lowercase);
        check_domains(&domains, public_host.as_deref())?;
        let contact = normalise_contact(contact)?;
        check_directory(directory_url)?;
        Ok(AcmeSettings { directory_url: directory_url.to_owned(), contact, domains, ca_file })
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The validity reader and the schedule
// ---------------------------------------------------------------------------------------------------------------------

/// The largest certificate the validity reader looks at, in bytes.
pub const MAX_CERT_BYTES: usize = 64 * 1024;

/// The validity of a certificate in seconds since the unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Validity {
    /// The first moment the certificate is valid.
    pub not_before: i64,
    /// The last moment the certificate is valid.
    pub not_after: i64,
}

/// One DER element: its tag, its content and what follows it. `None` when the tag is a multi-byte tag, the length is
/// indefinite or longer than three bytes, or the content reaches beyond the input.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 3 || rest.len() < count {
            return None;
        }
        let length = rest[..count].iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (length, &rest[count..])
    };
    if length > rest.len() {
        return None;
    }
    Some((tag, &rest[..length], &rest[length..]))
}

/// A UTCTime (`YYMMDDHHMMSSZ`) or a GeneralizedTime (`YYYYMMDDHHMMSSZ`) as seconds since the epoch. Only the forms RFC 5280
/// allows: UTC, with seconds.
fn read_time(tag: u8, content: &[u8]) -> Option<i64> {
    let (year_digits, wanted) = match tag {
        0x17 => (2, 13),
        0x18 => (4, 15),
        _ => return None,
    };
    if content.len() != wanted || content.last() != Some(&b'Z') {
        return None;
    }
    let digits = &content[..wanted - 1];
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let number = |part: &[u8]| part.iter().fold(0i32, |acc, b| acc * 10 + i32::from(b - b'0'));
    let year = if year_digits == 2 {
        let yy = number(&digits[..2]);
        if yy >= 50 { 1900 + yy } else { 2000 + yy }
    } else {
        number(&digits[..4])
    };
    let rest = &digits[year_digits..];
    let month = time::Month::try_from(u8::try_from(number(&rest[0..2])).ok()?).ok()?;
    let day = u8::try_from(number(&rest[2..4])).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    let moment = date
        .with_hms(u8::try_from(number(&rest[4..6])).ok()?, u8::try_from(number(&rest[6..8])).ok()?, u8::try_from(number(&rest[8..10])).ok()?)
        .ok()?;
    Some(moment.assume_utc().unix_timestamp())
}

/// Reads `notBefore` and `notAfter` from the first certificate of a DER input, and nothing else: it walks the fixed prefix of
/// a TBSCertificate (version, serial number, signature algorithm, issuer) and reads the validity after it. It does not decide
/// what to trust; it only tells the scheduler when to renew. The input is capped at [`MAX_CERT_BYTES`], every length is
/// checked against the input, nothing recurses, and anything unexpected gives `None`.
pub fn read_validity(der: &[u8]) -> Option<Validity> {
    if der.len() > MAX_CERT_BYTES {
        return None;
    }
    let (tag, certificate, _) = element(der)?;
    if tag != 0x30 {
        return None;
    }
    let (tag, tbs, _) = element(certificate)?;
    if tag != 0x30 {
        return None;
    }
    let mut rest = tbs;
    let (tag, _, after) = element(rest)?;
    if tag == 0xa0 {
        rest = after;
    }
    // Serial number, signature algorithm, issuer.
    for expected in [0x02, 0x30, 0x30] {
        let (tag, _, after) = element(rest)?;
        if tag != expected {
            return None;
        }
        rest = after;
    }
    let (tag, validity, _) = element(rest)?;
    if tag != 0x30 {
        return None;
    }
    let (first_tag, first, after) = element(validity)?;
    let (second_tag, second, _) = element(after)?;
    let not_before = read_time(first_tag, first)?;
    let not_after = read_time(second_tag, second)?;
    (not_before <= not_after).then_some(Validity { not_before, not_after })
}

/// How often the server looks at the certificate.
pub const CHECK_INTERVAL: StdDuration = StdDuration::from_secs(6 * 3600);

/// A certificate is renewed when fewer than this many seconds of its life remain.
pub const RENEW_WHEN_REMAINING_SECS: i64 = 7 * 86_400;

/// When the validity cannot be read, a certificate is renewed this many seconds after it was stored.
pub const UNREADABLE_RENEW_AFTER_SECS: i64 = 7 * 86_400;

/// The largest jitter, in thousandths of the time that remains until the renewal is due.
pub const JITTER_MAX_PERMILLE: u32 = 100;

/// The first wait after a failed order.
pub const BACKOFF_FIRST: StdDuration = StdDuration::from_secs(60);

/// The longest wait after failed orders.
pub const BACKOFF_MAX: StdDuration = StdDuration::from_secs(6 * 3600);

/// The moment, in seconds since the epoch, when a certificate is due for renewal, before jitter: two thirds of its lifetime
/// after it began, or seven days before it ends, whichever is earlier; and, when the validity is unreadable, seven days after
/// it was stored.
pub fn renewal_time(validity: Option<Validity>, stored_at: i64) -> i64 {
    match validity {
        Some(v) => {
            let lifetime = v.not_after.saturating_sub(v.not_before);
            let two_thirds = v.not_before.saturating_add(lifetime.saturating_mul(2) / 3);
            two_thirds.min(v.not_after.saturating_sub(RENEW_WHEN_REMAINING_SECS))
        }
        None => stored_at.saturating_add(UNREADABLE_RENEW_AFTER_SECS),
    }
}

/// The planned renewal moment seen from `now`: the due moment, brought forward by up to 10 percent of the time that remains.
/// A due moment that has passed stays as it is. `jitter_permille` above 100 counts as 100.
pub fn planned_renewal(due: i64, now: i64, jitter_permille: u32) -> i64 {
    if due <= now {
        return due;
    }
    let jitter = i64::from(jitter_permille.min(JITTER_MAX_PERMILLE));
    now.saturating_add((due - now).saturating_mul(1000 - jitter) / 1000)
}

/// The wait after the `failures`-th failure in a row: one minute, doubling, at most six hours.
pub fn backoff(failures: u32) -> StdDuration {
    let doublings = failures.saturating_sub(1).min(16);
    BACKOFF_FIRST.saturating_mul(1u32 << doublings).min(BACKOFF_MAX)
}

// ---------------------------------------------------------------------------------------------------------------------
// The sealed cache
// ---------------------------------------------------------------------------------------------------------------------

/// The directory of the cache below the data directory.
pub const CACHE_SUBDIR: &str = "server/acme";

const ACCOUNT_FILE: &str = "account.sealed";
const KEY_FILE: &str = "certificate-key.sealed";
const CHAIN_FILE: &str = "chain.pem";
const META_FILE: &str = "meta.json";

const ACCOUNT_CONTEXT: &str = "acme:account";
const KEY_CONTEXT: &str = "acme:certificate-key";

/// The largest sealed file that is read: the largest sealed value and its framing.
const MAX_SEALED_FILE_BYTES: u64 = sealed::MAX_VALUE_BYTES as u64 + 64;

/// The largest metadata file that is read.
const MAX_META_BYTES: u64 = 4096;

/// What the cache remembers besides the keys: when the certificate was stored, and when the next order may be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    /// The format of the file; 1.
    pub version: u32,
    /// When the certificate was stored, in seconds since the epoch.
    pub stored_at: i64,
    /// How many orders failed in a row.
    pub failures: u32,
    /// The earliest moment of the next order, in seconds since the epoch.
    pub next_attempt: i64,
}

impl Meta {
    /// Metadata with these values.
    pub fn new(stored_at: i64, failures: u32, next_attempt: i64) -> Meta {
        Meta { version: 1, stored_at, failures, next_attempt }
    }
}

/// A certificate chain and its private key as the cache holds them.
pub struct StoredCertificate {
    /// The chain, leaf first.
    pub chain: Vec<CertificateDer<'static>>,
    /// The private key as PKCS #8 DER.
    pub key_pkcs8: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for StoredCertificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCertificate").field("certificates", &self.chain.len()).finish_non_exhaustive()
    }
}

/// The account key and the certificate private key, sealed under the server key, and the public chain, in a directory of
/// their own. The directory is 0700 and every file 0600; a file is written to a temporary name, synced and renamed.
pub struct Cache {
    dir: PathBuf,
    sealer: Sealer,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cache").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Cache {
    /// The cache below `data_dir`, at [`CACHE_SUBDIR`].
    pub fn open(data_dir: &Path, key: &ServerKey) -> Result<Cache, AcmeError> {
        let dir = data_dir.join(CACHE_SUBDIR);
        let failed = |what: &str| AcmeError::new(format!("the ACME cache directory {} {what}; check that the data directory can be written", shown(&dir)));
        DirBuilder::new().recursive(true).mode(0o700).create(&dir).map_err(|_| failed("cannot be created"))?;
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| failed("cannot be examined"))?;
        if !meta.file_type().is_dir() {
            return Err(failed("is not a directory"));
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|_| failed("cannot be made private"))?;
        }
        Ok(Cache { dir, sealer: Sealer::new(key) })
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file `name` of the cache, read up to `cap` bytes. `None` when it does not exist.
    fn read(&self, name: &str, cap: u64) -> Result<Option<Zeroizing<Vec<u8>>>, AcmeError> {
        let path = self.dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(AcmeError::new(format!("the cached file {name} cannot be examined"))),
            Ok(_) => {}
        }
        let file = keyfile::open_private(&path, "ACME cache file")
            .map_err(|_| AcmeError::new(format!("the cached file {name} cannot be opened (it must be a regular file with mode 0600)")))?;
        let mut content = Zeroizing::new(Vec::new());
        file.take(cap + 1)
            .read_to_end(&mut content)
            .map_err(|_| AcmeError::new(format!("the cached file {name} cannot be read")))?;
        if content.len() as u64 > cap {
            return Err(AcmeError::new(format!("the cached file {name} is larger than {} KiB and is ignored", cap.div_ceil(1024))));
        }
        Ok(Some(content))
    }

    /// Writes `content` to `name`: temporary file (0600, exclusive), write, sync, rename, sync of the directory.
    fn write(&self, name: &str, content: &[u8]) -> Result<(), AcmeError> {
        let failed = || AcmeError::new(format!("the cached file {name} cannot be written; check that the ACME cache directory can be written"));
        let mut random = [0u8; 6];
        getrandom::fill(&mut random).map_err(|_| failed())?;
        let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let temporary = self.dir.join(format!(".{name}.{suffix}.tmp"));
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(content)?;
            file.sync_all()?;
            std::fs::rename(&temporary, self.dir.join(name))?;
            File::open(&self.dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(failed());
        }
        Ok(())
    }

    /// Stores the account credentials (a JSON text) sealed.
    pub fn write_account(&self, credentials_json: &[u8]) -> Result<(), AcmeError> {
        let sealed = self.sealer.seal(ACCOUNT_CONTEXT, credentials_json).map_err(|_| AcmeError::new("the account key cannot be sealed"))?;
        self.write(ACCOUNT_FILE, &sealed)
    }

    /// The account credentials, if the cache holds a valid sealed file.
    pub fn read_account(&self) -> Result<Option<Zeroizing<Vec<u8>>>, AcmeError> {
        let Some(sealed) = self.read(ACCOUNT_FILE, MAX_SEALED_FILE_BYTES)? else { return Ok(None) };
        self.sealer.open(ACCOUNT_CONTEXT, &sealed).map(Some).map_err(|_| unsealable(ACCOUNT_FILE))
    }

    /// Stores a certificate: the key sealed, the chain as PEM, and the metadata last, so that metadata that names a stored
    /// certificate is written only after the certificate is.
    pub fn write_certificate(&self, chain: &[CertificateDer<'_>], key_pkcs8: &[u8], meta: &Meta) -> Result<(), AcmeError> {
        let sealed = self.sealer.seal(KEY_CONTEXT, key_pkcs8).map_err(|_| AcmeError::new("the certificate key cannot be sealed"))?;
        let mut pem = String::new();
        for cert in chain {
            pem.push_str(&pem_block(cert.as_ref()));
        }
        self.write(KEY_FILE, &sealed)?;
        self.write(CHAIN_FILE, pem.as_bytes())?;
        self.write_meta(meta)
    }

    /// The stored certificate. `Ok(None)` when neither the chain nor the key exist; an error (the sentence says why the cache
    /// is ignored) when one file is missing, damaged, too large or sealed under another server key.
    pub fn read_certificate(&self) -> Result<Option<StoredCertificate>, AcmeError> {
        let chain = self.read(CHAIN_FILE, tls_pem_cap())?;
        let key = self.read(KEY_FILE, MAX_SEALED_FILE_BYTES)?;
        let (chain, key) = match (chain, key) {
            (None, None) => return Ok(None),
            (Some(chain), Some(key)) => (chain, key),
            _ => return Err(AcmeError::new("the ACME cache holds only part of a certificate and is ignored")),
        };
        let key_pkcs8 = self.sealer.open(KEY_CONTEXT, &key).map_err(|_| unsealable(KEY_FILE))?;
        let chain = parse_chain(&chain).map_err(|_| AcmeError::new(format!("the cached file {CHAIN_FILE} holds no usable certificate chain and is ignored")))?;
        Ok(Some(StoredCertificate { chain, key_pkcs8 }))
    }

    /// Writes the metadata.
    pub fn write_meta(&self, meta: &Meta) -> Result<(), AcmeError> {
        let text = serde_json::to_vec(meta).map_err(|_| AcmeError::new("the cache metadata cannot be written"))?;
        self.write(META_FILE, &text)
    }

    /// Reads the metadata; `Ok(None)` when there is no file.
    pub fn read_meta(&self) -> Result<Option<Meta>, AcmeError> {
        let Some(text) = self.read(META_FILE, MAX_META_BYTES)? else { return Ok(None) };
        let meta: Meta = serde_json::from_slice(&text)
            .map_err(|_| AcmeError::new(format!("the cached file {META_FILE} is damaged and is ignored")))?;
        if meta.version != 1 {
            return Err(AcmeError::new(format!("the cached file {META_FILE} is of a format this server does not know and is ignored")));
        }
        Ok(Some(meta))
    }
}

fn tls_pem_cap() -> u64 {
    crate::tls::MAX_PEM_BYTES
}

fn unsealable(name: &str) -> AcmeError {
    AcmeError::new(format!(
        "the cached file {name} cannot be opened under this server key (it is damaged or was sealed under another key) and is ignored"
    ))
}

/// A certificate as a PEM block of 64-character lines.
fn pem_block(der: &[u8]) -> String {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / 64 + 64);
    out.push_str("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// The certificates of a PEM text: at least one, at most [`crate::tls::MAX_CHAIN`].
fn parse_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, AcmeError> {
    if pem.len() as u64 > tls_pem_cap() {
        return Err(AcmeError::new("the certificate chain is larger than 256 KiB"));
    }
    let mut chain = Vec::new();
    for item in CertificateDer::pem_slice_iter(pem) {
        let der = item.map_err(|_| AcmeError::new("the certificate chain holds a damaged PEM block"))?;
        if chain.len() == crate::tls::MAX_CHAIN {
            return Err(AcmeError::new("the certificate chain holds too many certificates"));
        }
        chain.push(der);
    }
    if chain.is_empty() {
        return Err(AcmeError::new("the certificate chain holds no certificate"));
    }
    Ok(chain)
}

// ---------------------------------------------------------------------------------------------------------------------
// Checks of an issued certificate
// ---------------------------------------------------------------------------------------------------------------------

/// A certified key from a chain and a PKCS #8 key, refused when the key is not the key of the first certificate.
pub fn certified_from(chain: &[CertificateDer<'static>], key_pkcs8: &[u8]) -> Result<CertifiedKey, AcmeError> {
    let der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pkcs8.to_vec()));
    let signing = provider()
        .key_provider
        .load_private_key(der)
        .map_err(|_| AcmeError::new("the certificate key is not a key this server can sign with"))?;
    let certified = CertifiedKey::new(chain.to_vec(), signing);
    certified.keys_match().map_err(|_| AcmeError::new("the certificate does not belong to the key it was ordered with"))?;
    Ok(certified)
}

/// Checks that the leaf is valid now, covers every one of `domains`, and is signed through the chain up to its last
/// certificate (which is used as the anchor: the CA's own issuer, since the chain is what the CA sent). This decides only
/// whether the server is willing to serve the certificate; it is not a trust decision about a peer.
pub fn verify_chain(chain: &[CertificateDer<'static>], domains: &[String], now: SystemTime) -> Result<(), AcmeError> {
    let (Some(leaf), Some(anchor)) = (chain.first(), chain.last()) else {
        return Err(AcmeError::new("the CA sent no certificate"));
    };
    if chain.len() < 2 {
        return Err(AcmeError::new("the CA sent only the leaf certificate and not its issuer"));
    }
    let mut roots = RootCertStore::empty();
    roots.add(anchor.clone()).map_err(|_| AcmeError::new("the last certificate of the chain cannot be read"))?;
    let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .map_err(|_| AcmeError::new("the certificate chain cannot be checked"))?;
    let at = UnixTime::since_unix_epoch(now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default());
    for domain in domains {
        let name = ServerName::try_from(domain.as_str()).map_err(|_| AcmeError::new("a configured name is not a server name"))?;
        verifier
            .verify_server_cert(leaf, &chain[1..chain.len() - 1], &name, &[], at)
            .map_err(|e| match e {
                rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
                | rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForNameContext { .. }) => {
                    AcmeError::new(format!("the certificate does not cover {domain}"))
                }
                rustls::Error::InvalidCertificate(rustls::CertificateError::Expired)
                | rustls::Error::InvalidCertificate(rustls::CertificateError::ExpiredContext { .. }) => {
                    AcmeError::new("the certificate has expired")
                }
                rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidYet)
                | rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidYetContext { .. }) => {
                    AcmeError::new("the certificate is not valid yet")
                }
                other => AcmeError::new(format!("the certificate chain does not verify ({})", printable(&other.to_string()))),
            })?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------------------------------

/// A certificate as an order returns it.
pub struct Issued {
    /// The chain, leaf first.
    pub chain: Vec<CertificateDer<'static>>,
    /// The private key as PKCS #8 DER.
    pub key_pkcs8: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for Issued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Issued").field("certificates", &self.chain.len()).finish_non_exhaustive()
    }
}

/// What to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Serve the cached certificate; it is not due.
    Serve,
    /// Order a new certificate.
    Order,
    /// There is nothing to serve and a new order must wait for the back-off to end.
    Wait,
}

/// The answer of [`AcmeManager::decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// What to do.
    pub action: Action,
    /// How long to wait before looking again.
    pub wait: StdDuration,
    /// Why the cache was ignored, when it was.
    pub ignored: Option<String>,
}

/// What one pass of the manager did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The decision the pass started with.
    pub action: Action,
    /// Whether an order was made.
    pub ordered: bool,
    /// The sentence of a failed order.
    pub error: Option<String>,
    /// How long to wait before the next pass.
    pub wait: StdDuration,
}

/// The longest an order takes in total.
pub const ORDER_LIMIT: StdDuration = StdDuration::from_secs(5 * 60);

/// The longest the server waits for the CA to validate and to issue.
pub const POLL_LIMIT: StdDuration = StdDuration::from_secs(3 * 60);

/// The first and the longest delay between two looks at an order.
const POLL_FIRST: StdDuration = StdDuration::from_secs(1);
const POLL_MAX: StdDuration = StdDuration::from_secs(10);

/// Removes the challenges of an order when it ends, however it ends (including a cancelled future).
struct ChallengeGuard {
    resolver: Arc<AcmeResolver>,
    names: Mutex<Vec<String>>,
}

impl ChallengeGuard {
    fn new(resolver: Arc<AcmeResolver>) -> ChallengeGuard {
        ChallengeGuard { resolver, names: Mutex::new(Vec::new()) }
    }

    fn add(&self, name: &str) {
        self.names.lock().unwrap_or_else(PoisonError::into_inner).push(name.to_owned());
    }
}

impl Drop for ChallengeGuard {
    fn drop(&mut self) {
        for name in self.names.get_mut().unwrap_or_else(PoisonError::into_inner).drain(..) {
            self.resolver.clear_challenge(&name);
        }
    }
}

/// An error of the ACME client as a sentence: what failed, and the CA's own words (which are public) on one line.
fn ca_error(what: &str, error: &instant_acme::Error) -> AcmeError {
    AcmeError::new(format!("{what}: {}", printable(&error.to_string())))
}

/// Gets, caches and renews the certificate of the server.
pub struct AcmeManager {
    settings: AcmeSettings,
    resolver: Arc<AcmeResolver>,
    cache: Cache,
    clock: Arc<dyn Clock>,
    jitter_permille: u32,
    /// The planned renewal moment of the certificate that was stored at the first value.
    planned: Mutex<Option<(i64, i64)>>,
}

impl fmt::Debug for AcmeManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcmeManager").field("domains", &self.settings.domains).field("cache", &self.cache.dir()).finish_non_exhaustive()
    }
}

impl AcmeManager {
    /// A manager for checked settings. The cache directory is made (0700) below `data_dir`; secrets are sealed under `key`.
    /// The jitter of the renewal time is drawn once here.
    pub fn new(
        settings: AcmeSettings,
        resolver: Arc<AcmeResolver>,
        key: &ServerKey,
        data_dir: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<AcmeManager, AcmeError> {
        let cache = Cache::open(data_dir, key)?;
        let mut random = [0u8; 2];
        getrandom::fill(&mut random).map_err(|_| AcmeError::new("the operating system gave no random bytes"))?;
        let jitter_permille = u32::from(u16::from_le_bytes(random)) % (JITTER_MAX_PERMILLE + 1);
        Ok(AcmeManager { settings, resolver, cache, clock, jitter_permille, planned: Mutex::new(None) })
    }

    /// Replaces the drawn jitter (thousandths of the remaining time, at most 100). For tests.
    #[must_use]
    pub fn with_jitter(mut self, permille: u32) -> AcmeManager {
        self.jitter_permille = permille.min(JITTER_MAX_PERMILLE);
        self
    }

    /// The resolver the manager installs certificates into.
    pub fn resolver(&self) -> &Arc<AcmeResolver> {
        &self.resolver
    }

    /// The cache.
    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Whether a certificate is served (the server is ready to take connections).
    pub fn ready(&self) -> bool {
        self.resolver.has_certificate()
    }

    /// The cached certificate, checked: the key belongs to it, it covers every configured name, and it is valid at `now`.
    fn load_cached(&self, now: SystemTime, meta: Option<&Meta>) -> Result<Option<(CertifiedKey, Option<Validity>, i64)>, AcmeError> {
        let Some(stored) = self.cache.read_certificate()? else { return Ok(None) };
        let meta = meta.ok_or_else(|| AcmeError::new(format!("the ACME cache has no {META_FILE} and is ignored")))?;
        let key = certified_from(&stored.chain, &stored.key_pkcs8)?;
        verify_chain(&stored.chain, &self.settings.domains, now)?;
        let validity = stored.chain.first().and_then(|leaf| read_validity(leaf.as_ref()));
        Ok(Some((key, validity, meta.stored_at)))
    }

    /// The planned renewal moment of the certificate stored at `stored_at`: drawn with the jitter once per certificate.
    fn planned_for(&self, validity: Option<Validity>, stored_at: i64, now: i64) -> i64 {
        let mut planned = self.planned.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, when)) = *planned
            && at == stored_at
        {
            return when;
        }
        let when = planned_renewal(renewal_time(validity, stored_at), now, self.jitter_permille);
        *planned = Some((stored_at, when));
        when
    }

    /// Looks at the cache and the time and says what to do. A valid certificate that is not due is installed (when none is
    /// served yet) and kept; a cache that is damaged, sealed under another key, over its cap, incomplete, expired or for other
    /// names is ignored with a sentence and an order is due. It makes no network call.
    pub fn decide(&self) -> Decision {
        let now = self.clock.now();
        let now_s = unix_seconds(now);
        let mut ignored = None;
        let meta = match self.cache.read_meta() {
            Ok(meta) => meta,
            Err(e) => {
                ignored = Some(e.sentence().to_owned());
                None
            }
        };
        let hold_until = meta.map_or(0, |m| m.next_attempt);
        let until = |moment: i64| StdDuration::from_secs(u64::try_from((moment - now_s).max(1)).unwrap_or(1)).min(CHECK_INTERVAL);
        match self.load_cached(now, meta.as_ref()) {
            Ok(Some((key, validity, stored_at))) => {
                if !self.resolver.has_certificate() {
                    self.resolver.set_certified_key(Arc::new(key));
                }
                let due_at = self.planned_for(validity, stored_at, now_s).max(hold_until);
                if now_s < due_at {
                    return Decision { action: Action::Serve, wait: until(due_at), ignored };
                }
                Decision { action: Action::Order, wait: CHECK_INTERVAL, ignored }
            }
            Ok(None) => self.without_certificate(now_s, hold_until, ignored),
            Err(e) => {
                let sentence = e.sentence().to_owned();
                log::event(Level::Warn, "acme_cache_ignored", &[("reason", &sentence)]);
                self.without_certificate(now_s, hold_until, Some(ignored.unwrap_or(sentence)))
            }
        }
    }

    fn without_certificate(&self, now_s: i64, hold_until: i64, ignored: Option<String>) -> Decision {
        if now_s < hold_until {
            let wait = StdDuration::from_secs(u64::try_from(hold_until - now_s).unwrap_or(1).max(1)).min(CHECK_INTERVAL);
            return Decision { action: Action::Wait, wait, ignored };
        }
        Decision { action: Action::Order, wait: CHECK_INTERVAL, ignored }
    }

    /// Takes a certificate an order returned: checks it (key, names, validity, chain), stores it sealed, and serves it from
    /// the next handshake on. Nothing is changed when a check fails.
    pub fn accept(&self, issued: &Issued, now: SystemTime) -> Result<(), AcmeError> {
        let key = certified_from(&issued.chain, &issued.key_pkcs8)?;
        verify_chain(&issued.chain, &self.settings.domains, now)?;
        let now_s = unix_seconds(now);
        let next = now_s.saturating_add(i64::try_from(CHECK_INTERVAL.as_secs()).unwrap_or(i64::MAX));
        self.cache.write_certificate(&issued.chain, &issued.key_pkcs8, &Meta::new(now_s, 0, next))?;
        self.resolver.set_certified_key(Arc::new(key));
        *self.planned.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    /// Remembers a failed order: one more failure, and the next attempt after the back-off. The old certificate stays.
    fn record_failure(&self, now_s: i64) -> StdDuration {
        let previous = self.cache.read_meta().ok().flatten();
        let failures = previous.map_or(0, |m| m.failures).saturating_add(1);
        let wait = backoff(failures);
        let next = now_s.saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX));
        let meta = Meta::new(previous.map_or(0, |m| m.stored_at), failures, next);
        if let Err(e) = self.cache.write_meta(&meta) {
            log::event(Level::Warn, "acme_meta_failed", &[("reason", e.sentence())]);
        }
        wait
    }

    /// One pass: decides, and when an order is due calls `order`, accepts what it returns and records a failure. `order` is
    /// given so that tests count calls; the server passes [`AcmeManager::order_once`].
    pub async fn step<F, Fut>(&self, order: F) -> Step
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Issued, AcmeError>>,
    {
        let decision = self.decide();
        if decision.action != Action::Order {
            return Step { action: decision.action, ordered: false, error: None, wait: decision.wait };
        }
        log::event(Level::Info, "acme_order_started", &[]);
        let outcome = match order().await {
            Ok(issued) => self.accept(&issued, self.clock.now()),
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => {
                log::event(Level::Info, "acme_order_ok", &[]);
                Step { action: Action::Order, ordered: true, error: None, wait: CHECK_INTERVAL }
            }
            Err(e) => {
                let wait = self.record_failure(unix_seconds(self.clock.now()));
                log::event(Level::Warn, "acme_order_failed", &[("reason", e.sentence()), ("retry_in_seconds", &wait.as_secs().to_string())]);
                Step { action: Action::Order, ordered: true, error: Some(e.sentence().to_owned()), wait }
            }
        }
    }

    /// Runs forever: looks at the certificate every [`CHECK_INTERVAL`] (sooner when something is due), and orders when it is
    /// due. Dropping the future stops it.
    pub async fn run(self: Arc<Self>) {
        loop {
            let manager = Arc::clone(&self);
            let step = self.step(|| async move { manager.order_once().await }).await;
            tokio::time::sleep(step.wait).await;
        }
    }

    /// The connection to the CA: the platform's trust, or only `ca_file` when one is named (a private CA). The root file is
    /// used for the CA connection and for nothing else.
    fn builder(&self) -> Result<AccountBuilder, AcmeError> {
        let built = match &self.settings.ca_file {
            Some(path) => Account::builder_with_root(path),
            None => Account::builder(),
        };
        built.map_err(|e| ca_error("the connection to the CA cannot be prepared", &e))
    }

    /// The account: from the sealed cache when it is there and for this directory, otherwise a new one (with the contact and
    /// the operator's agreement to the terms), stored sealed.
    async fn account(&self) -> Result<Account, AcmeError> {
        match self.cache.read_account() {
            Ok(Some(json)) => {
                let same_directory = serde_json::from_slice::<serde_json::Value>(&json)
                    .ok()
                    .and_then(|v| v.get("directory").and_then(|d| d.as_str().map(|d| d == self.settings.directory_url)))
                    .unwrap_or(false);
                if same_directory && let Ok(credentials) = serde_json::from_slice::<AccountCredentials>(&json) {
                    match self.builder()?.from_credentials(credentials).await {
                        Ok(account) => return Ok(account),
                        Err(e) => log::event(Level::Warn, "acme_account_unusable", &[("reason", &printable(&e.to_string()))]),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => log::event(Level::Warn, "acme_cache_ignored", &[("reason", e.sentence())]),
        }
        let contact = [self.settings.contact.as_str()];
        let (account, credentials) = self
            .builder()?
            .create(
                &NewAccount { contact: &contact, terms_of_service_agreed: true, only_return_existing: false },
                self.settings.directory_url.clone(),
                None,
            )
            .await
            .map_err(|e| ca_error("the CA did not create the account", &e))?;
        let json = Zeroizing::new(serde_json::to_vec(&credentials).map_err(|_| AcmeError::new("the account cannot be stored"))?);
        self.cache.write_account(&json)?;
        Ok(account)
    }

    /// Orders the configured names once and returns the certificate. Only the `tls-alpn-01` challenge is answered, with a
    /// challenge set before the CA is told to look; every challenge is cleared when this returns or is cancelled. Every wait
    /// is bounded. The old certificate stays in use whatever happens here.
    pub async fn order_once(&self) -> Result<Issued, AcmeError> {
        let guard = ChallengeGuard::new(Arc::clone(&self.resolver));
        match tokio::time::timeout(ORDER_LIMIT, self.order_inner(&guard)).await {
            Ok(result) => result,
            Err(_) => Err(AcmeError::new("the order did not finish within 5 minutes; the certificate in use stays")),
        }
    }

    async fn order_inner(&self, guard: &ChallengeGuard) -> Result<Issued, AcmeError> {
        let account = self.account().await?;
        let identifiers: Vec<Identifier> = self.settings.domains.iter().cloned().map(Identifier::Dns).collect();
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(|e| ca_error("the CA did not accept the order", &e))?;

        {
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                let mut authorization = result.map_err(|e| ca_error("an authorization cannot be read", &e))?;
                let name = authorization.identifier().to_string();
                if authorization.wildcard {
                    return Err(AcmeError::new(format!("the CA asks to prove a wildcard for {name}, which this server cannot do")));
                }
                match authorization.status {
                    AuthorizationStatus::Pending => {}
                    AuthorizationStatus::Valid => continue,
                    other => {
                        return Err(AcmeError::new(format!("the authorization for {name} is {other:?} and cannot be used")));
                    }
                }
                let mut challenge = authorization
                    .challenge(ChallengeType::TlsAlpn01)
                    .ok_or_else(|| AcmeError::new(format!("the CA offers no tls-alpn-01 challenge for {name}")))?;
                let digest: [u8; 32] = challenge
                    .key_authorization()
                    .digest()
                    .as_ref()
                    .try_into()
                    .map_err(|_| AcmeError::new("the challenge digest has an unexpected length"))?;
                self.resolver.set_challenge(&name, digest)?;
                guard.add(&name);
                challenge.set_ready().await.map_err(|e| ca_error(&format!("the CA did not take the challenge for {name}"), &e))?;
            }
        }

        let deadline = tokio::time::Instant::now() + POLL_LIMIT;
        wait_for_status(&mut order, OrderStatus::Ready, deadline).await?;

        let key = KeyPair::generate().map_err(|_| AcmeError::new("the certificate key cannot be generated"))?;
        let mut params = CertificateParams::new(self.settings.domains.clone())
            .map_err(|_| AcmeError::new("the request for the certificate cannot be made"))?;
        params.distinguished_name = DistinguishedName::new();
        let csr = params.serialize_request(&key).map_err(|_| AcmeError::new("the request for the certificate cannot be signed"))?;
        order.finalize_csr(csr.der()).await.map_err(|e| ca_error("the CA did not accept the certificate request", &e))?;
        wait_for_status(&mut order, OrderStatus::Valid, deadline).await?;

        let pem = order
            .certificate()
            .await
            .map_err(|e| ca_error("the certificate cannot be downloaded", &e))?
            .ok_or_else(|| AcmeError::new("the CA has not issued the certificate yet"))?;
        let chain = parse_chain(pem.as_bytes())?;
        Ok(Issued { chain, key_pkcs8: Zeroizing::new(key.serialize_der()) })
    }
}

/// Looks at the order with a delay that grows from one second to ten, until it has the status `wanted`, becomes invalid, or
/// `deadline` passes.
async fn wait_for_status(order: &mut instant_acme::Order, wanted: OrderStatus, deadline: tokio::time::Instant) -> Result<(), AcmeError> {
    let mut delay = POLL_FIRST;
    loop {
        let state = order.refresh().await.map_err(|e| ca_error("the order cannot be read", &e))?;
        if let Some(problem) = &state.error {
            return Err(AcmeError::new(format!("the CA reports a problem with the order: {}", printable(&problem.to_string()))));
        }
        if state.status == wanted {
            return Ok(());
        }
        match state.status {
            OrderStatus::Invalid => {
                return Err(AcmeError::new(
                    "the CA marked the order invalid: it could not validate the challenge; check that every name resolves to this server and that its TLS port is reachable on port 443 from the internet",
                ));
            }
            OrderStatus::Valid if wanted == OrderStatus::Ready => return Ok(()),
            _ => {}
        }
        if tokio::time::Instant::now() + delay > deadline {
            return Err(AcmeError::new("the CA did not finish in time (3 minutes); the certificate in use stays"));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(POLL_MAX);
    }
}

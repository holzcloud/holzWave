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
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use rcgen::{CertificateParams, CustomExtension, DistinguishedName, KeyPair};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use time::{Duration, OffsetDateTime};

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

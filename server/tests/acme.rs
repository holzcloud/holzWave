//! ACME: the TLS-ALPN-01 responder, the order flow, the sealed cache, the validity reader, the schedule, and (ignored, run
//! by `sh scripts/web-container-test.sh acme`) a real certificate from Pebble.
//!
//! The responder tests run the real acceptor of `server::tls` with the real resolver. The client skips the chain check
//! because a challenge certificate is self-signed by design; it checks the certificate bytes itself.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use holzwave_server::acme::{self, AcmeResolver};
use holzwave_server::tls;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

const NAME: &str = "hw.example.test";

/// A verifier that accepts any chain. The challenge certificate is self-signed by design, and rustls' own verifier refuses
/// its critical extension, so the test reads the certificate itself.
#[derive(Debug)]
struct AcceptAnyChain(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyChain {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn client_config(alpn: &[&[u8]]) -> ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("client protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyChain(provider)))
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config
}

/// A listener on 127.0.0.1 with the real acceptor over `resolver`. A challenge connection is closed after the handshake
/// (as the listener must do); any other connection is closed too, after the handshake, which is all these tests need.
struct Listener {
    addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn listen(resolver: Arc<AcmeResolver>) -> Listener {
    let config = tls::server_config(resolver, &acme::ALPN).expect("server configuration");
    let acceptor = tls::acceptor(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local address");
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = tls::accept_tls(&acceptor, stream, Duration::from_secs(5)).await {
                    let negotiated = tls.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
                    // Challenge or not, nothing is served here.
                    let _ = acme::is_challenge(negotiated.as_deref());
                }
            });
        }
    });
    Listener { addr, task }
}

/// What a client saw after a completed handshake.
struct Seen {
    alpn: Option<Vec<u8>>,
    leaf: Vec<u8>,
}

async fn handshake(listener: &Listener, name: &str, alpn: &[&[u8]]) -> std::io::Result<Seen> {
    let tcp = TcpStream::connect(listener.addr).await?;
    let server_name = ServerName::try_from(name.to_owned()).expect("server name");
    let stream = TlsConnector::from(Arc::new(client_config(alpn))).connect(server_name, tcp).await?;
    let (_, conn) = stream.get_ref();
    let leaf = conn.peer_certificates().and_then(|chain| chain.first()).map(|c| c.as_ref().to_vec()).unwrap_or_default();
    Ok(Seen { alpn: conn.alpn_protocol().map(<[u8]>::to_vec), leaf })
}

/// Whether `needle` occurs in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The DER of the critical acmeIdentifier extension: OID 1.3.6.1.5.5.7.1.31, BOOLEAN TRUE, then an OCTET STRING that holds
/// an OCTET STRING of the 32 digest bytes.
fn acme_identifier_der(digest: &[u8; 32]) -> Vec<u8> {
    let mut der = vec![0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f, 0x01, 0x01, 0xff, 0x04, 0x22, 0x04, 0x20];
    der.extend_from_slice(digest);
    der
}

/// The DER of a subjectAltName extension that holds exactly one dNSName, `name`. The subject is empty, so the extension is
/// critical (RFC 5280 section 4.2.1.6).
fn san_der(name: &str) -> Vec<u8> {
    let inner_len = u8::try_from(name.len() + 2).expect("short name");
    let mut der = vec![0x06, 0x03, 0x55, 0x1d, 0x11, 0x01, 0x01, 0xff, 0x04, inner_len + 2, 0x30, inner_len, 0x82, u8::try_from(name.len()).expect("short name")];
    der.extend_from_slice(name.as_bytes());
    der
}

#[tokio::test]
async fn challenge_handshake_gets_the_certificate_with_the_digest() {
    let resolver = Arc::new(AcmeResolver::new());
    resolver.set_challenge(NAME, [7u8; 32]).expect("set the challenge");
    let listener = listen(resolver.clone()).await;

    let seen = handshake(&listener, NAME, &[b"acme-tls/1"]).await.expect("the challenge handshake completes");
    assert_eq!(seen.alpn.as_deref(), Some(&b"acme-tls/1"[..]), "acme-tls/1 is negotiated");
    assert!(!seen.leaf.is_empty());
    assert!(contains(&seen.leaf, &san_der(NAME)), "the only subject alternative name is the pending name");
    assert!(
        contains(&seen.leaf, &acme_identifier_der(&[7u8; 32])),
        "the critical acmeIdentifier extension holds the 32-byte digest"
    );
    println!(
        "Challenge cert: SAN {NAME}, extension 1.3.6.1.5.5.7.1.31 critical (01 01 ff), digest starts {:02x}{:02x}{:02x}{:02x}, ALPN {}",
        7, 7, 7, 7,
        String::from_utf8_lossy(seen.alpn.as_deref().unwrap_or_default())
    );

    // A server name in capitals reaches the same pending name.
    let upper = handshake(&listener, "HW.Example.Test", &[b"acme-tls/1"]).await.expect("case does not matter");
    assert!(contains(&upper.leaf, &acme_identifier_der(&[7u8; 32])));

    // The challenge protocol among others still gets the challenge, and the server's own order of preference wins.
    let both = handshake(&listener, NAME, &[b"http/1.1", b"acme-tls/1"]).await.expect("offering several protocols");
    assert_eq!(both.alpn.as_deref(), Some(&b"acme-tls/1"[..]));
}

#[tokio::test]
async fn challenge_for_another_name_or_nothing_pending_or_no_certificate_fails_alike() {
    let resolver = Arc::new(AcmeResolver::new());
    resolver.set_challenge(NAME, [7u8; 32]).expect("set the challenge");
    let listener = listen(resolver.clone()).await;

    let other = handshake(&listener, "other.example.test", &[b"acme-tls/1"]).await.err().expect("another name fails");
    let only_http = handshake(&listener, NAME, &[b"http/1.1"]).await.err().expect("no production certificate yet fails");
    resolver.clear_challenge(NAME);
    assert_eq!(resolver.pending_count(), 0);
    let cleared = handshake(&listener, NAME, &[b"acme-tls/1"]).await.err().expect("a cleared challenge fails");

    // The three cases and the case of a missing certificate end the same way, with the same error class and text.
    assert_eq!(other.to_string(), cleared.to_string());
    assert_eq!(other.to_string(), only_http.to_string());
    println!("Failures: other name -> {other}; cleared -> {cleared}; http/1.1 only before any issuance -> {only_http}");
    assert!(other.to_string().contains("AccessDenied") || other.to_string().contains("alert"), "{other}");

    // No server name at all is not a pending name either.
    let tcp = TcpStream::connect(listener.addr).await.expect("connect");
    let mut config = client_config(&[b"acme-tls/1"]);
    config.enable_sni = false;
    let server_name = ServerName::try_from(NAME.to_owned()).expect("server name");
    resolver.set_challenge(NAME, [1u8; 32]).expect("the challenge can be set again");
    assert!(TlsConnector::from(Arc::new(config)).connect(server_name, tcp).await.is_err(), "no SNI, no certificate");
}

#[test]
fn challenge_protocol_is_recognised_for_the_listener() {
    assert!(acme::is_challenge(Some(b"acme-tls/1")));
    assert!(!acme::is_challenge(Some(b"h2")));
    assert!(!acme::is_challenge(Some(b"http/1.1")));
    assert!(!acme::is_challenge(None));
}

#[tokio::test]
async fn challenge_table_is_bounded_and_names_are_checked() {
    let resolver = AcmeResolver::new();
    for n in 0..acme::MAX_PENDING {
        resolver.set_challenge(&format!("n{n}.example.test"), [n as u8; 32]).expect("within the bound");
    }
    assert_eq!(resolver.pending_count(), acme::MAX_PENDING);
    assert!(resolver.set_challenge("eleven.example.test", [1u8; 32]).is_err(), "the eleventh name is refused");
    resolver.set_challenge("N0.example.test", [9u8; 32]).expect("a name that is pending again replaces its entry");
    assert_eq!(resolver.pending_count(), acme::MAX_PENDING);
    for bad in ["*.example.test", "192.0.2.1", "a..b", "", "Exa_mple.test"] {
        assert!(resolver.set_challenge(bad, [1u8; 32]).is_err(), "{bad:?} is not a host name");
    }
    resolver.clear_challenge("n0.EXAMPLE.test");
    assert_eq!(resolver.pending_count(), acme::MAX_PENDING - 1);
    let text = format!("{resolver:?}");
    assert!(!text.contains("example"), "Debug names no pending domain or key: {text}");
}

#[tokio::test]
async fn challenge_resolver_serves_the_issued_certificate_to_a_normal_handshake_only_after_issuance() {
    let resolver = Arc::new(AcmeResolver::new());
    let listener = listen(resolver.clone()).await;
    assert!(handshake(&listener, NAME, &[b"h2", b"http/1.1"]).await.is_err(), "nothing issued yet: not reachable");
    assert!(!resolver.has_certificate());

    // A certificate arrives (a stand-in made here): the next handshake gets it, a challenge for it is still refused.
    let made = rcgen::generate_simple_self_signed(vec![NAME.to_owned()]).expect("a certificate");
    let der = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(made.signing_key.serialize_der()));
    let signing = rustls::crypto::ring::default_provider().key_provider.load_private_key(der).expect("signing key");
    resolver.set_certified_key(Arc::new(rustls::sign::CertifiedKey::new(vec![made.cert.der().clone()], signing)));
    let seen = handshake(&listener, NAME, &[b"h2", b"http/1.1"]).await.expect("served after issuance");
    assert_eq!(seen.leaf, made.cert.der().as_ref());
    assert_eq!(seen.alpn.as_deref(), Some(&b"h2"[..]));
    assert!(handshake(&listener, NAME, &[b"acme-tls/1"]).await.is_err(), "the issued certificate is never a challenge answer");
}

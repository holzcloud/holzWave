//! ACME: the TLS-ALPN-01 responder, the order flow, the sealed cache, the validity reader, the schedule, and (ignored, run
//! by `sh scripts/web-container-test.sh acme`) a real certificate from Pebble.
//!
//! The responder tests run the real acceptor of `server::tls` with the real resolver. The client skips the chain check
//! because a challenge certificate is self-signed by design; it checks the certificate bytes itself.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use holzwave_server::acme::{self, AcmeError, AcmeManager, AcmeResolver, AcmeSettings, Action, Cache, Issued, Meta, Validity};
use holzwave_server::clock::ManualClock;
use holzwave_server::keyfile::ServerKey;
use holzwave_server::tls;
use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use scratch::Scratch;
use time::OffsetDateTime;
use zeroize::Zeroizing;
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

// ---------------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------------

/// Seconds since the epoch of a UTC calendar moment.
fn ts(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> i64 {
    time::Date::from_calendar_date(year, time::Month::try_from(month).expect("month"), day)
        .expect("date")
        .with_hms(hour, minute, second)
        .expect("time")
        .assume_utc()
        .unix_timestamp()
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(seconds).expect("a moment rcgen can write")
}

const DAY: i64 = 86_400;

/// "Now" of the manager tests: 2027-01-15 12:00:00 UTC.
fn now0() -> i64 {
    ts(2027, 1, 15, 12, 0, 0)
}

/// A certificate for `names` valid from `not_before` to `not_after`, signed by a fresh CA, as DER chain (leaf, CA) and the
/// leaf key as PKCS #8.
fn issue(names: &[&str], not_before: i64, not_after: i64) -> (Vec<rustls::pki_types::CertificateDer<'static>>, Vec<u8>) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("ca parameters");
    ca_params.distinguished_name = DistinguishedName::new();
    ca_params.distinguished_name.push(rcgen::DnType::CommonName, "Test CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = at(ts(2000, 1, 1, 0, 0, 0));
    ca_params.not_after = at(ts(2090, 1, 1, 0, 0, 0));
    let ca_key = KeyPair::generate().expect("ca key");
    let ca_cert = ca_params.self_signed(&ca_key).expect("ca certificate");
    let issuer = Issuer::from_params(&ca_params, &ca_key);

    let mut params = CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).expect("leaf parameters");
    params.distinguished_name = DistinguishedName::new();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_before = at(not_before);
    params.not_after = at(not_after);
    let key = KeyPair::generate().expect("leaf key");
    let leaf = params.signed_by(&key, &issuer).expect("leaf certificate");
    (vec![leaf.der().clone(), ca_cert.der().clone()], key.serialize_der())
}

fn issued(names: &[&str], not_before: i64, not_after: i64) -> Issued {
    let (chain, key) = issue(names, not_before, not_after);
    Issued { chain, key_pkcs8: Zeroizing::new(key) }
}

/// A certificate that is valid around `now0()` for 90 days.
fn fresh(names: &[&str]) -> Issued {
    issued(names, now0() - 3600, now0() - 3600 + 90 * DAY)
}

fn server_key(byte: u8) -> ServerKey {
    ServerKey::from_bytes([byte; 32]).expect("a non-zero key")
}

fn settings() -> AcmeSettings {
    AcmeSettings::new("https://ca.example.test/dir", "ops@example.test", &[NAME.to_owned()], None, true, Some(NAME))
        .expect("valid settings")
}

/// A manager over a scratch data directory with a clock the test moves, and a jitter of zero.
struct Rig {
    dir: Scratch,
    clock: Arc<ManualClock>,
    manager: AcmeManager,
}

fn rig_with(settings: AcmeSettings, dir: Scratch, key: &ServerKey, clock: Arc<ManualClock>) -> Rig {
    let manager = AcmeManager::new(settings, Arc::new(AcmeResolver::new()), key, dir.path(), clock.clone())
        .expect("manager")
        .with_jitter(0);
    Rig { dir, clock, manager }
}

fn rig() -> Rig {
    rig_with(settings(), Scratch::new("acme"), &server_key(5), Arc::new(ManualClock::new(now0())))
}

/// A second manager over the same data directory, as after a restart (a new resolver, the same clock).
fn restart(rig: &Rig, key: &ServerKey) -> AcmeManager {
    AcmeManager::new(settings(), Arc::new(AcmeResolver::new()), key, rig.dir.path(), rig.clock.clone())
        .expect("manager after a restart")
        .with_jitter(0)
}

/// Runs one step with an order function that counts its calls and returns what `make` makes.
async fn step_counting(manager: &AcmeManager, calls: &AtomicUsize, make: impl FnOnce() -> Result<Issued, AcmeError>) -> acme::Step {
    manager
        .step(|| async {
            calls.fetch_add(1, Ordering::SeqCst);
            make()
        })
        .await
}

fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

// ---------------------------------------------------------------------------------------------------------------------
// The operator's settings
// ---------------------------------------------------------------------------------------------------------------------

fn try_settings(domains: &[&str], contact: &str, directory: &str, terms: bool, public: Option<&str>) -> Result<AcmeSettings, String> {
    let domains: Vec<String> = domains.iter().map(|d| (*d).to_owned()).collect();
    AcmeSettings::new(directory, contact, &domains, None, terms, public).map_err(|e| e.sentence().to_owned())
}

#[test]
fn settings_terms_must_be_agreed_by_the_operator() {
    let refused = try_settings(&[NAME], "ops@example.test", "https://ca.example.test/dir", false, Some(NAME)).expect_err("terms false");
    assert!(refused.contains("tls.terms_agreed"), "{refused}");
    assert!(!refused.contains('\n'));
    let accepted = try_settings(&[NAME], "ops@example.test", "https://ca.example.test/dir", true, Some(NAME)).expect("terms true");
    assert_eq!(accepted.contact, "mailto:ops@example.test");
}

#[test]
fn settings_domains_are_checked_before_any_network_call() {
    let long_name = format!("{}.test", ["abcdefghi"; 25].join("."));
    let long_label = format!("{}.test", "a".repeat(64));
    let eleven: Vec<String> = (0..11).map(|n| format!("n{n}.example.test")).collect();
    let eleven_refs: Vec<&str> = eleven.iter().map(String::as_str).collect();
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        ("empty", vec![], "at least one name"),
        ("eleven names", eleven_refs, "more than 10 names"),
        ("wildcard", vec![NAME, "*.example.test"], "wildcard"),
        ("ipv4", vec![NAME, "192.0.2.1"], "IP address"),
        ("ipv6", vec![NAME, "2001:db8::1"], "IP address"),
        ("underscore", vec![NAME, "Exa_mple.test"], "not a host name"),
        ("empty label", vec![NAME, "a..b"], "not a host name"),
        ("duplicate, other case", vec![NAME, "HW.example.test"], "twice"),
        ("a name over 253 characters", vec![NAME, long_name.as_str()], "not a host name"),
        ("a label over 63 characters", vec![NAME, long_label.as_str()], "not a host name"),
        ("the public host is not among them", vec!["www.example.test"], "public_url"),
    ];
    for (name, domains, word) in cases {
        let sentence = try_settings(&domains, "ops@example.test", "https://ca.example.test/dir", true, Some(NAME))
            .expect_err(name);
        assert!(sentence.contains(word), "{name}: {sentence:?} lacks {word:?}");
        assert!(!sentence.contains('\n'), "{name}");
    }
    // Upper-case input is lower-cased, also for the public host.
    let ok = try_settings(&["HW.Example.TEST", "www.example.test"], "ops@example.test", "https://ca.example.test/dir", true, Some("Hw.example.test"))
        .expect("capitals are lowered");
    assert_eq!(ok.domains, ["hw.example.test", "www.example.test"]);
    // Ten names are accepted.
    let ten: Vec<String> = (0..10).map(|n| format!("n{n}.example.test")).collect();
    let ten_refs: Vec<&str> = ten.iter().map(String::as_str).collect();
    try_settings(&ten_refs, "ops@example.test", "https://ca.example.test/dir", true, Some("n3.example.test")).expect("ten names");
}

#[test]
fn settings_contact_is_a_plain_email_address() {
    let ok = |c: &str| try_settings(&[NAME], c, "https://ca.example.test/dir", true, Some(NAME)).map(|s| s.contact);
    assert_eq!(ok("ops@example.test").as_deref(), Ok("mailto:ops@example.test"));
    assert_eq!(ok("mailto:ops@example.test").as_deref(), Ok("mailto:ops@example.test"));
    assert_eq!(ok("o.p-s+acme@Example.Test").as_deref(), Ok("mailto:o.p-s+acme@Example.Test"));
    let too_long = format!("{}@example.test", "a".repeat(240));
    let bad = ["ops", "a@b@c", "ops @example.test", "ops@example.test\nBcc: x@example.test", "@example.test",
        "ops@", "ops@exa_mple.test", "ops@exam\rple.test", "ops@192.0.2.1", "ops?cc=x@example.test", "ops@example.test?subject=x", "", "  ", "mailto:",
        too_long.as_str(), "o\u{7f}ps@example.test"];
    for contact in bad {
        let sentence = ok(contact).expect_err(contact);
        assert!(sentence.contains("email address") || sentence.contains("tls.contact"), "{contact:?}: {sentence}");
        assert!(!sentence.contains('\n'));
    }
    // A local part of 64 characters is the limit; surrounding white space is trimmed.
    assert!(ok(&format!("{}@example.test", "a".repeat(64))).is_ok());
    assert!(ok(&format!("{}@example.test", "a".repeat(65))).is_err());
    assert_eq!(ok("  ops@example.test\n").as_deref(), Ok("mailto:ops@example.test"));
}

#[test]
fn settings_directory_must_be_https() {
    for bad in ["http://ca.example.test/dir", "ftp://x", "ca.example.test", "https://", "https://ca.example.test/a b", "https://ca\nexample.test"] {
        let sentence = try_settings(&[NAME], "ops@example.test", bad, true, Some(NAME)).expect_err(bad);
        assert!(sentence.contains("https address"), "{bad:?}: {sentence}");
    }
    try_settings(&[NAME], "ops@example.test", "https://acme-v02.api.letsencrypt.org/directory", true, Some(NAME)).expect("https");
}

// ---------------------------------------------------------------------------------------------------------------------
// The validity reader
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn validity_fixtures_are_read_exactly() {
    // (name, notBefore, notAfter): a UTCTime pair, a pair that crosses 2050 (UTCTime then GeneralizedTime), a pair that is
    // GeneralizedTime in both fields, and a pair that starts in the last second before the year 2000 (UTCTime "99").
    let fixtures = [
        ("utc pair", ts(2026, 3, 1, 8, 30, 0), ts(2026, 5, 30, 8, 30, 0)),
        ("crosses 2050", ts(2049, 12, 31, 23, 59, 59), ts(2050, 1, 1, 0, 0, 0)),
        ("generalized pair", ts(2060, 2, 29, 1, 2, 3), ts(2070, 12, 31, 23, 59, 59)),
        ("1999", ts(1999, 12, 31, 23, 59, 59), ts(2000, 1, 1, 0, 0, 0)),
    ];
    for (name, not_before, not_after) in fixtures {
        let (chain, _) = issue(&[NAME], not_before, not_after);
        let read = acme::read_validity(chain[0].as_ref()).unwrap_or_else(|| panic!("{name}: the validity is read"));
        assert_eq!(read, Validity { not_before, not_after }, "{name}");
        println!("Validity fixtures: {name}: set {not_before}..{not_after}, read {}..{}", read.not_before, read.not_after);
    }
}

#[test]
fn validity_reader_refuses_what_is_not_a_certificate_and_never_reads_past_its_input() {
    let (chain, _) = issue(&[NAME], ts(2026, 3, 1, 8, 30, 0), ts(2026, 5, 30, 8, 30, 0));
    let der = chain[0].as_ref().to_vec();
    assert!(acme::read_validity(&der).is_some());
    assert!(acme::read_validity(&[]).is_none(), "empty");
    assert!(acme::read_validity(b"not a certificate at all").is_none(), "text");
    assert!(acme::read_validity(&[0x30]).is_none(), "a tag alone");
    assert!(acme::read_validity(&[0x30, 0x80, 0x00, 0x00]).is_none(), "an indefinite length");
    assert!(acme::read_validity(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none(), "a length of four bytes");
    // Every truncation fails (the outer length then points beyond the input), none panics.
    for n in 0..der.len() {
        assert!(acme::read_validity(&der[..n]).is_none(), "a prefix of {n} bytes");
    }
    // Over the cap, even when the certificate is intact in front of the padding.
    let mut padded = der.clone();
    padded.resize(acme::MAX_CERT_BYTES + 1, 0);
    assert!(acme::read_validity(&padded).is_none(), "over 64 KiB");
    // Lengths that point beyond the input.
    let mut long_outer = der.clone();
    long_outer[1] = 0x82;
    long_outer.splice(2..4, [0xff, 0xff]);
    assert!(acme::read_validity(&long_outer).is_none());
    // Every single-byte change either gives some answer or none; it never panics or loops.
    for position in 0..der.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut changed = der.clone();
            changed[position] ^= flip;
            let _ = acme::read_validity(&changed);
        }
    }
    // A GeneralizedTime in the place of a UTCTime with the wrong length, and a month 13, are refused.
    let mut bad_month = der.clone();
    let needle = b"260301083000Z";
    let at_pos = bad_month.windows(needle.len()).position(|w| w == needle).expect("the notBefore text");
    bad_month[at_pos + 2] = b'1';
    bad_month[at_pos + 3] = b'3';
    assert!(acme::read_validity(&bad_month).is_none(), "month 13");
}

// ---------------------------------------------------------------------------------------------------------------------
// The schedule
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn schedule_table_renews_at_two_thirds_or_seven_days_before_the_end() {
    // (lifetime in days, age in days, due?). The age counts from notBefore, which is also the day the certificate was stored.
    let table = [
        (90i64, 0i64, false), (90, 59, false), (90, 60, true), (90, 83, true), (90, 89, true),
        (30, 19, false), (30, 20, true),
        (10, 2, false), (10, 3, true),
        (6, 0, true), (6, 5, true),
        (365, 242, false), (365, 244, true),
    ];
    for (lifetime, age, expect_due) in table {
        let validity = Validity { not_before: 0, not_after: lifetime * DAY };
        let due = acme::renewal_time(Some(validity), 0);
        let is_due = age * DAY >= due;
        println!("Schedule table: lifetime {lifetime} days, age {age} days -> {}", if is_due { "due" } else { "not due" });
        assert_eq!(is_due, expect_due, "lifetime {lifetime}, age {age}");
    }
    // The 90-day certificate is due exactly on day 60 and the 6-day one at once (the due moment is before it began).
    assert_eq!(acme::renewal_time(Some(Validity { not_before: 0, not_after: 90 * DAY }), 0), 60 * DAY);
    assert!(acme::renewal_time(Some(Validity { not_before: 0, not_after: 6 * DAY }), 0) <= 0);
}

#[test]
fn schedule_jitter_moves_the_plan_by_at_most_a_tenth_of_what_remains() {
    let due = 1_000_000;
    for now in [0i64, 500_000, 999_000] {
        let remaining = due - now;
        assert_eq!(acme::planned_renewal(due, now, 0), due, "no jitter");
        for permille in [1u32, 50, 100, 101, 5000] {
            let planned = acme::planned_renewal(due, now, permille);
            assert!(planned <= due, "never later than due");
            assert!(planned >= now + remaining * 9 / 10, "never earlier than 90 percent of what remains: now {now}, jitter {permille}");
        }
        assert_eq!(acme::planned_renewal(due, now, 100), now + remaining * 9 / 10);
    }
    assert_eq!(acme::planned_renewal(500, 1000, 100), 500, "a moment that has passed stays");
}

#[test]
fn schedule_an_unreadable_validity_renews_after_seven_days() {
    let stored = 1_000_000;
    assert_eq!(acme::renewal_time(None, stored), stored + 7 * DAY);
}

#[test]
fn schedule_failures_back_off_from_one_minute_to_six_hours_and_stay() {
    let secs: Vec<u64> = (1..=14).map(|n| acme::backoff(n).as_secs()).collect();
    assert_eq!(secs[..9], [60, 120, 240, 480, 960, 1920, 3840, 7680, 15360]);
    assert!(secs[9..].iter().all(|s| *s == 6 * 3600), "{secs:?}");
    assert_eq!(acme::backoff(0).as_secs(), 60);
    assert_eq!(acme::backoff(u32::MAX).as_secs(), 6 * 3600);
}

// ---------------------------------------------------------------------------------------------------------------------
// The sealed cache
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn cache_round_trips_and_every_file_is_private() {
    let dir = Scratch::new("acme-cache");
    let cache = Cache::open(dir.path(), &server_key(5)).expect("open");
    assert_eq!(mode_of(cache.dir()), 0o700, "the directory is private");
    assert!(cache.dir().ends_with("server/acme"));

    let item = fresh(&[NAME]);
    let marker = br#"{"id":"SECRETMARKER-account","key_pkcs8":"SECRETMARKER-key"}"#;
    cache.write_account(marker).expect("write the account");
    cache.write_certificate(&item.chain, &item.key_pkcs8, &Meta::new(now0(), 2, now0() + 120)).expect("write the certificate");

    let account = cache.read_account().expect("read").expect("present");
    assert_eq!(&account[..], &marker[..]);
    let stored = cache.read_certificate().expect("read").expect("present");
    assert_eq!(stored.chain, item.chain);
    assert_eq!(&stored.key_pkcs8[..], &item.key_pkcs8[..]);
    assert_eq!(cache.read_meta().expect("read").expect("present"), Meta::new(now0(), 2, now0() + 120));

    for name in ["account.sealed", "certificate-key.sealed", "chain.pem", "meta.json"] {
        assert_eq!(mode_of(&cache.dir().join(name)), 0o600, "{name}");
    }
    // No temporary file is left behind, and no secret is in a file in the clear.
    let names: Vec<String> = std::fs::read_dir(cache.dir()).expect("list").map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), 4, "{names:?}");
    for name in &names {
        let bytes = std::fs::read(cache.dir().join(name)).expect("read the file");
        assert!(!bytes.windows(12).any(|w| w == b"SECRETMARKER"), "{name} holds the marker in the clear");
        let key = &item.key_pkcs8[..16];
        assert!(!bytes.windows(16).any(|w| w == key), "{name} holds the certificate key in the clear");
    }
    // The chain is public PEM.
    let chain = std::fs::read_to_string(cache.dir().join("chain.pem")).expect("pem");
    assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2);
}

#[test]
fn cache_directory_is_made_private_and_a_link_in_its_place_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new("acme-cache-dir");
    let path = dir.path().join("server/acme");
    std::fs::create_dir_all(&path).expect("make the directory");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("loosen");
    Cache::open(dir.path(), &server_key(5)).expect("open an existing loose directory");
    assert_eq!(mode_of(&path), 0o700, "a loose directory is tightened");

    let other = Scratch::new("acme-cache-link");
    std::fs::create_dir_all(other.path().join("server")).expect("server dir");
    let target = other.path().join("elsewhere");
    std::fs::create_dir(&target).expect("target");
    std::os::unix::fs::symlink(&target, other.path().join("server/acme")).expect("link");
    let error = Cache::open(other.path(), &server_key(5)).expect_err("a link is refused");
    assert!(error.sentence().contains("not a directory"), "{error}");
}

/// Writes a valid cache and returns the directory path of the cache.
fn valid_cache(rig: &Rig) -> std::path::PathBuf {
    let item = fresh(&[NAME]);
    rig.manager.cache().write_account(b"{}").expect("account");
    rig.manager.cache().write_certificate(&item.chain, &item.key_pkcs8, &Meta::new(now0(), 0, 0)).expect("certificate");
    rig.manager.cache().dir().to_path_buf()
}

#[test]
fn cache_damage_is_ignored_with_a_sentence_and_an_order_is_due() {
    use std::os::unix::fs::PermissionsExt;
    type Damage = Box<dyn Fn(&std::path::Path)>;
    let flip = |name: &'static str| -> Damage {
        Box::new(move |dir| {
            let path = dir.join(name);
            let mut bytes = std::fs::read(&path).expect("read");
            let middle = bytes.len() / 2;
            bytes[middle] ^= 0x01;
            std::fs::write(&path, bytes).expect("write");
        })
    };
    let cases: Vec<(&str, Damage)> = vec![
        ("a flipped byte in the sealed key", flip("certificate-key.sealed")),
        ("a flipped byte in the chain", flip("chain.pem")),
        ("a sealed key over its cap", Box::new(|dir| std::fs::write(dir.join("certificate-key.sealed"), vec![7u8; 100 * 1024]).expect("write"))),
        ("a chain over its cap", Box::new(|dir| std::fs::write(dir.join("chain.pem"), vec![b'A'; 300 * 1024]).expect("write"))),
        ("a missing metadata file", Box::new(|dir| std::fs::remove_file(dir.join("meta.json")).expect("remove"))),
        ("damaged metadata", Box::new(|dir| std::fs::write(dir.join("meta.json"), b"{not json").expect("write"))),
        ("metadata of another format", Box::new(|dir| std::fs::write(dir.join("meta.json"), br#"{"version":9,"stored_at":0,"failures":0,"next_attempt":0}"#).expect("write"))),
        ("a missing chain", Box::new(|dir| std::fs::remove_file(dir.join("chain.pem")).expect("remove"))),
        ("a missing sealed key", Box::new(|dir| std::fs::remove_file(dir.join("certificate-key.sealed")).expect("remove"))),
        ("a chain that is text", Box::new(|dir| std::fs::write(dir.join("chain.pem"), b"hello").expect("write"))),
        ("a chain with mode 0644", Box::new(|dir| std::fs::set_permissions(dir.join("chain.pem"), std::fs::Permissions::from_mode(0o644)).expect("chmod"))),
        ("a sealed key that is a link", Box::new(|dir| {
            let real = dir.join("real");
            std::fs::rename(dir.join("certificate-key.sealed"), &real).expect("rename");
            std::os::unix::fs::symlink(&real, dir.join("certificate-key.sealed")).expect("link");
        })),
    ];
    for (name, damage) in cases {
        let rig = rig();
        let cache_dir = valid_cache(&rig);
        // Undamaged, the cache is served.
        assert_eq!(restart(&rig, &server_key(5)).decide().action, Action::Serve, "{name}: the intact cache serves");
        damage(&cache_dir);
        let manager = restart(&rig, &server_key(5));
        let decision = manager.decide();
        assert_eq!(decision.action, Action::Order, "{name}");
        let sentence = decision.ignored.unwrap_or_else(|| panic!("{name}: a sentence says why"));
        assert!(!sentence.contains('\n') && !sentence.is_empty(), "{name}");
        assert!(!manager.ready(), "{name}: nothing damaged is served");
        println!("Cache: {name}: Order, {sentence}");
    }

    // Sealed under another server key: ignored the same way.
    let rig = rig();
    valid_cache(&rig);
    let other = restart(&rig, &server_key(6));
    let decision = other.decide();
    assert_eq!(decision.action, Action::Order);
    assert!(decision.ignored.expect("a sentence").contains("another key"), "the sentence names the likely cause");
    // The account file is a part of the cache in the same way.
    let account = rig.manager.cache().dir().join("account.sealed");
    let mut bytes = std::fs::read(&account).expect("read");
    bytes[30] ^= 0x01;
    std::fs::write(&account, bytes).expect("write");
    assert!(rig.manager.cache().read_account().is_err());
}

// ---------------------------------------------------------------------------------------------------------------------
// The manager
// ---------------------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn manager_a_valid_cache_that_is_not_due_is_served_and_makes_no_order() {
    let rig = rig();
    valid_cache(&rig);
    let calls = AtomicUsize::new(0);
    let step = step_counting(&rig.manager, &calls, || panic!("no order is made for a valid cache")).await;
    assert_eq!(step.action, Action::Serve);
    assert!(!step.ordered && step.error.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the ordering function was not called");
    assert!(rig.manager.ready(), "the cached certificate is served at once");
    assert!(step.wait <= acme::CHECK_INTERVAL && step.wait.as_secs() > 0);
    // Looking again later, still before the due day, changes nothing.
    rig.clock.advance(30 * DAY);
    let step = step_counting(&rig.manager, &calls, || panic!("still not due")).await;
    assert_eq!(step.action, Action::Serve);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn manager_a_due_certificate_is_renewed_and_the_new_one_is_served_without_a_restart() {
    let rig = rig();
    let calls = AtomicUsize::new(0);

    // Nothing cached: the first order.
    assert_eq!(rig.manager.decide().action, Action::Order);
    assert!(!rig.manager.ready(), "nothing is served before the first issuance");
    let step = step_counting(&rig.manager, &calls, || Ok(fresh(&[NAME]))).await;
    assert!(step.ordered && step.error.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(rig.manager.ready());
    let first = rig.manager.resolver().certified_key().expect("served").cert[0].clone();
    assert_eq!(rig.manager.decide().action, Action::Serve);
    assert!(rig.manager.cache().read_certificate().expect("read").is_some(), "the certificate is cached");

    // 61 days later (past two thirds of 90 days) the order is made again, and a different certificate is served.
    rig.clock.advance(61 * DAY);
    let renewed = issued(&[NAME], now0() + 61 * DAY - 3600, now0() + 61 * DAY - 3600 + 90 * DAY);
    let step = step_counting(&rig.manager, &calls, || Ok(renewed)).await;
    assert!(step.ordered && step.error.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let second = rig.manager.resolver().certified_key().expect("served").cert[0].clone();
    assert_ne!(first, second, "the new certificate is served");
    let meta = rig.manager.cache().read_meta().expect("read").expect("present");
    assert_eq!((meta.failures, meta.stored_at), (0, now0() + 61 * DAY));
    assert_eq!(rig.manager.decide().action, Action::Serve);
}

#[tokio::test]
async fn manager_a_failed_order_keeps_the_old_certificate_and_the_back_off_survives_a_restart() {
    let rig = rig();
    let calls = AtomicUsize::new(0);
    step_counting(&rig.manager, &calls, || Ok(fresh(&[NAME]))).await;
    let served = rig.manager.resolver().certified_key().expect("served").cert[0].clone();
    rig.clock.advance(61 * DAY);

    // First failure: one minute.
    let step = step_counting(&rig.manager, &calls, || Err(AcmeError::new("the CA said no"))).await;
    assert_eq!(step.error.as_deref(), Some("the CA said no"));
    assert_eq!(step.wait.as_secs(), 60);
    let now = now0() + 61 * DAY;
    assert_eq!(rig.manager.cache().read_meta().expect("read").expect("present"), Meta::new(now0(), 1, now + 60));
    assert_eq!(rig.manager.resolver().certified_key().expect("still served").cert[0], served, "the old certificate stays");

    // Inside the back-off nothing is ordered, also after a restart (the next attempt time is on disk).
    let restarted = restart(&rig, &server_key(5));
    let before = calls.load(Ordering::SeqCst);
    let step = step_counting(&restarted, &calls, || panic!("the back-off holds across a restart")).await;
    assert_eq!(step.action, Action::Serve);
    assert_eq!(calls.load(Ordering::SeqCst), before);
    assert!(restarted.ready(), "the old certificate is served after the restart");

    // After the minute the order is made again; a second failure waits two minutes.
    rig.clock.advance(61);
    let step = step_counting(&restarted, &calls, || Err(AcmeError::new("again"))).await;
    assert_eq!(step.wait.as_secs(), 120);
    assert_eq!(rig.manager.cache().read_meta().expect("read").expect("present").failures, 2);

    // A success resets the count.
    rig.clock.advance(121);
    let renewed = issued(&[NAME], now0() + 61 * DAY, now0() + 151 * DAY);
    let step = step_counting(&restarted, &calls, || Ok(renewed)).await;
    assert!(step.error.is_none());
    assert_eq!(rig.manager.cache().read_meta().expect("read").expect("present").failures, 0);
}

#[tokio::test]
async fn manager_without_a_certificate_waits_out_the_back_off_and_is_not_ready() {
    let rig = rig();
    let calls = AtomicUsize::new(0);
    let step = step_counting(&rig.manager, &calls, || Err(AcmeError::new("no luck"))).await;
    assert!(step.error.is_some());
    assert!(!rig.manager.ready(), "a server without a certificate reports not-ready");
    let decision = rig.manager.decide();
    assert_eq!(decision.action, Action::Wait);
    assert_eq!(decision.wait.as_secs(), 60);
    rig.clock.advance(60);
    assert_eq!(rig.manager.decide().action, Action::Order);
}

#[tokio::test]
async fn manager_a_certificate_that_misses_a_name_or_has_another_key_is_never_served() {
    let rig = rig();
    let calls = AtomicUsize::new(0);

    let wrong_name = step_counting(&rig.manager, &calls, || Ok(fresh(&["other.example.test"]))).await;
    assert!(wrong_name.error.as_deref().expect("refused").contains("does not cover hw.example.test"), "{wrong_name:?}");

    let (chain_a, _) = issue(&[NAME], now0() - 3600, now0() + DAY);
    let (_, key_b) = issue(&[NAME], now0() - 3600, now0() + DAY);
    let mismatched = rig.manager.accept(&Issued { chain: chain_a.clone(), key_pkcs8: Zeroizing::new(key_b) }, rig.clock.now_for_test());
    assert!(mismatched.expect_err("another key").sentence().contains("does not belong"), "key mismatch");

    let (chain, key) = issue(&[NAME], now0() - 3600, now0() + DAY);
    let leaf_only = rig.manager.accept(&Issued { chain: chain[..1].to_vec(), key_pkcs8: Zeroizing::new(key.clone()) }, rig.clock.now_for_test());
    assert!(leaf_only.expect_err("no issuer").sentence().contains("only the leaf"));

    let (expired_chain, expired_key) = issue(&[NAME], now0() - 10 * DAY, now0() - DAY);
    let expired = rig.manager.accept(&Issued { chain: expired_chain, key_pkcs8: Zeroizing::new(expired_key) }, rig.clock.now_for_test());
    assert!(expired.expect_err("expired").sentence().contains("expired"));

    assert!(!rig.manager.ready(), "nothing refused was served");
    assert!(rig.manager.cache().read_certificate().expect("read").is_none(), "nothing refused was stored");
    // A good one passes through the same door.
    rig.manager.accept(&Issued { chain, key_pkcs8: Zeroizing::new(key) }, rig.clock.now_for_test()).expect("a good certificate");
    assert!(rig.manager.ready());
}

/// A small extension for the clock so a test can pass its time as a `SystemTime`.
trait NowForTest {
    fn now_for_test(&self) -> std::time::SystemTime;
}

impl NowForTest for Arc<ManualClock> {
    fn now_for_test(&self) -> std::time::SystemTime {
        use holzwave_server::clock::Clock;
        self.now()
    }
}

#[tokio::test]
async fn manager_an_expired_cache_is_ignored() {
    let rig = rig();
    let old = issued(&[NAME], now0() - 100 * DAY, now0() - DAY);
    rig.manager.cache().write_certificate(&old.chain, &old.key_pkcs8, &Meta::new(now0() - 100 * DAY, 0, 0)).expect("write");
    let decision = rig.manager.decide();
    assert_eq!(decision.action, Action::Order);
    assert!(decision.ignored.expect("a sentence").contains("expired"));
    assert!(!rig.manager.ready());
}

#[tokio::test]
async fn manager_a_cache_for_other_names_is_ignored() {
    let rig = rig();
    let other = fresh(&["other.example.test"]);
    rig.manager.cache().write_certificate(&other.chain, &other.key_pkcs8, &Meta::new(now0(), 0, 0)).expect("write");
    let decision = rig.manager.decide();
    assert_eq!(decision.action, Action::Order);
    assert!(decision.ignored.expect("a sentence").contains("does not cover"));
}

#[tokio::test]
async fn manager_the_jitter_is_drawn_once_per_certificate_and_stays_in_bounds() {
    // With a jitter of 100 permille the renewal is planned at 90 percent of what remains when the certificate is first seen,
    // and a restart with a jitter of zero plans it at the due moment: the plan is a property of the manager, not of the file.
    let rig = rig();
    let item = fresh(&[NAME]);
    rig.manager.cache().write_certificate(&item.chain, &item.key_pkcs8, &Meta::new(now0(), 0, 0)).expect("write");
    let jittered = AcmeManager::new(settings(), Arc::new(AcmeResolver::new()), &server_key(5), rig.dir.path(), rig.clock.clone())
        .expect("manager")
        .with_jitter(100);
    let due = acme::renewal_time(Some(Validity { not_before: now0() - 3600, not_after: now0() - 3600 + 90 * DAY }), now0());
    let planned = now0() + (due - now0()) * 9 / 10;
    let calls = AtomicUsize::new(0);
    // The first look draws the plan; later looks reuse it, so the order is due at the planned moment and not after another draw.
    assert_eq!(step_counting(&jittered, &calls, || panic!("not yet")).await.action, Action::Serve);
    rig.clock.set(planned - 5);
    assert_eq!(step_counting(&jittered, &calls, || panic!("not yet")).await.action, Action::Serve);
    rig.clock.set(planned);
    let outcome = step_counting(&jittered, &calls, || Ok(fresh(&[NAME]))).await;
    assert!(outcome.ordered, "due at the planned moment");
}

#[tokio::test]
async fn order_against_an_unreachable_ca_ends_in_a_sentence_and_leaves_nothing_pending() {
    // Nothing listens on port 1: the directory cannot be fetched. The failure is a sentence, nothing panics, no challenge is
    // left, and no secret is in the text.
    let settings = AcmeSettings::new("https://127.0.0.1:1/dir", "ops@example.test", &[NAME.to_owned()], None, true, Some(NAME)).expect("settings");
    let dir = Scratch::new("acme-unreachable");
    let manager = AcmeManager::new(settings, Arc::new(AcmeResolver::new()), &server_key(5), dir.path(), Arc::new(ManualClock::new(now0())))
        .expect("manager");
    let error = tokio::time::timeout(Duration::from_secs(30), manager.order_once()).await.expect("bounded").expect_err("unreachable");
    println!("Unreachable CA: {error}");
    assert!(!error.sentence().contains('\n') && !error.sentence().is_empty());
    assert_eq!(manager.resolver().pending_count(), 0);
    assert!(manager.cache().read_account().expect("read").is_none(), "no account was stored");
}

#[test]
fn challenge_only_the_alpn_challenge_type_is_answered() {
    let source = include_str!("../src/acme.rs");
    for forbidden in ["Http01", "Dns01", "DnsAccount01", "DeviceAttest01", "TcpListener", "port 80"] {
        let hits = source.lines().filter(|l| !l.trim_start().starts_with("//")).filter(|l| l.contains(forbidden)).count();
        assert_eq!(hits, 0, "{forbidden} appears in code of acme.rs");
    }
    assert!(source.contains("ChallengeType::TlsAlpn01"));
}

#[tokio::test]
async fn secrets_never_appear_in_debug_text_or_in_sentences() {
    let rig = rig();
    let item = fresh(&[NAME]);
    let key_hex: String = item.key_pkcs8[..24].iter().map(|b| format!("{b:?}")).collect::<Vec<_>>().join(", ");
    rig.manager.cache().write_account(br#"{"id":"https://ca/acct/1","key_pkcs8":"SECRETMARKER"}"#).expect("account");
    rig.manager.cache().write_certificate(&item.chain, &item.key_pkcs8, &Meta::new(now0(), 0, 0)).expect("certificate");
    let stored = rig.manager.cache().read_certificate().expect("read").expect("present");
    let texts = [
        format!("{:?}", rig.manager),
        format!("{:?}", rig.manager.cache()),
        format!("{:?}", rig.manager.resolver()),
        format!("{item:?}"),
        format!("{stored:?}"),
        format!("{:?}", settings()),
    ];
    for text in &texts {
        assert!(!text.contains("SECRETMARKER") && !text.contains(&key_hex), "{text}");
    }
    // The sentences of damaged files do not carry file content either.
    std::fs::write(rig.manager.cache().dir().join("meta.json"), b"{\"SECRETMARKER\":1}").expect("write");
    let decision = rig.manager.decide();
    assert!(!decision.ignored.expect("a sentence").contains("SECRETMARKER"));
}

// ---------------------------------------------------------------------------------------------------------------------
// Pebble (a real ACME server), run in containers by `sh scripts/web-container-test.sh acme`
// ---------------------------------------------------------------------------------------------------------------------

fn env_or_fail(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; run this test with `sh scripts/web-container-test.sh acme`"))
}

/// One DER element: tag, content and the rest.
fn der_element(input: &[u8]) -> (u8, &[u8], &[u8]) {
    let tag = input[0];
    let (length, header) = if input[1] < 0x80 {
        (usize::from(input[1]), 2)
    } else {
        let count = usize::from(input[1] & 0x7f);
        (input[2..2 + count].iter().fold(0usize, |a, b| (a << 8) | usize::from(*b)), 2 + count)
    };
    (tag, &input[header..header + length], &input[header + length..])
}

/// The serial number of a certificate, in hex.
fn serial_hex(cert: &[u8]) -> String {
    let (_, certificate, _) = der_element(cert);
    let (_, tbs, _) = der_element(certificate);
    let (tag, _, after_version) = der_element(tbs);
    let rest = if tag == 0xa0 { after_version } else { tbs };
    let (_, serial, _) = der_element(rest);
    serial.iter().map(|b| format!("{b:02x}")).collect()
}

/// The text of the issuer's common name as it appears in the certificate (the run of printable bytes that starts at
/// "Pebble").
fn issuer_text(cert: &[u8]) -> String {
    let start = cert.windows(6).position(|w| w == b"Pebble").expect("the issuer name contains Pebble");
    cert[start..].iter().take_while(|b| (0x20..0x7f).contains(*b)).map(|b| char::from(*b)).collect()
}

/// A client configuration that trusts exactly these roots.
fn trusting(roots: &[rustls::pki_types::CertificateDer<'static>], alpn: &[&[u8]]) -> ClientConfig {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add(root.clone()).expect("a root certificate");
    }
    let mut config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("versions")
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    config
}

/// The roots of Pebble's current chain, from its management interface (they change at every start of Pebble). The
/// connection to that interface trusts only the file the test was given.
async fn pebble_roots(ca_file: &str, management: &str) -> Vec<rustls::pki_types::CertificateDer<'static>> {
    use rustls::pki_types::pem::PemObject;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let minica = rustls::pki_types::CertificateDer::from_pem_file(ca_file).expect("the CA file of Pebble");
    let (host, _) = management.split_once(':').expect("host:port");
    let tcp = TcpStream::connect(management).await.expect("connect to the management interface");
    let name = ServerName::try_from(host.to_owned()).expect("name");
    let mut tls = TlsConnector::from(Arc::new(trusting(&[minica], &[b"http/1.1"]))).connect(name, tcp).await.expect("TLS to Pebble");
    tls.write_all(format!("GET /roots/0 HTTP/1.1\r\nHost: {management}\r\nConnection: close\r\n\r\n").as_bytes()).await.expect("request");
    let mut response = Vec::new();
    let _ = tls.read_to_end(&mut response).await;
    let body_at = response.windows(4).position(|w| w == b"\r\n\r\n").expect("a response with a body") + 4;
    let roots: Vec<_> = rustls::pki_types::CertificateDer::pem_slice_iter(&response[body_at..]).collect::<Result<_, _>>().expect("PEM roots");
    assert!(!roots.is_empty(), "Pebble sent a root");
    roots
}

/// A listener on all addresses of the validation port with the real acceptor. A challenge connection is closed after the
/// handshake; any other connection gets a fixed line and is then echoed until the client closes.
async fn listen_on_all(resolver: Arc<AcmeResolver>, port: u16) -> Listener {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let config = tls::server_config(resolver, &acme::ALPN).expect("server configuration");
    let acceptor = tls::acceptor(Arc::new(config));
    let listener = TcpListener::bind(("0.0.0.0", port)).await.expect("bind the validation port");
    let addr = listener.local_addr().expect("local address");
    let task = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = tls::accept_tls(&acceptor, stream, Duration::from_secs(10)).await else { return };
                if acme::is_challenge(tls.get_ref().1.alpn_protocol()) {
                    return;
                }
                let _ = tls.write_all(b"hello from holzwave\n").await;
                let mut byte = [0u8; 1];
                while matches!(tls.read(&mut byte).await, Ok(1)) {
                    if tls.write_all(&byte).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    Listener { addr, task }
}

#[tokio::test]
#[ignore = "needs Pebble: run `sh scripts/web-container-test.sh acme`"]
async fn acme_pebble_issues_serves_renews_and_keeps_the_old_certificate_when_an_order_fails() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = env_or_fail("HW_ACME_DIRECTORY");
    let ca_file = env_or_fail("HW_ACME_CA_FILE");
    let domain = env_or_fail("HW_ACME_DOMAIN");
    let port: u16 = env_or_fail("HW_ACME_PORT").parse().expect("the validation port");
    let management = env_or_fail("HW_ACME_MANAGEMENT");

    let dir = Scratch::new("acme-pebble");
    let clock: Arc<dyn holzwave_server::clock::Clock> = Arc::new(holzwave_server::clock::SystemClock);
    let resolver = Arc::new(AcmeResolver::new());
    let settings = AcmeSettings::new(&directory, "ops@example.test", std::slice::from_ref(&domain), Some(ca_file.clone().into()), true, Some(&domain))
        .expect("settings");
    let manager = AcmeManager::new(settings, resolver.clone(), &server_key(9), dir.path(), clock.clone()).expect("manager");
    let _listener = listen_on_all(resolver.clone(), port).await;
    let local = SocketAddr::from(([127, 0, 0, 1], port));
    let roots = pebble_roots(&ca_file, &management).await;

    // Before the first issuance a normal handshake fails: the server is not ready.
    assert!(!manager.ready());

    // 1. The first order, through the real challenge on the TLS port.
    let step = manager.step(|| manager.order_once()).await;
    assert!(step.ordered && step.error.is_none(), "the first order: {:?}", step.error);
    assert!(manager.ready());
    assert_eq!(resolver.pending_count(), 0, "no challenge is left");

    // 2. A client that asks for the domain gets the issued certificate, chained to Pebble's root.
    let connect = |alpn: &'static [&'static [u8]]| {
        let roots = roots.clone();
        let domain = domain.clone();
        async move {
            let tcp = TcpStream::connect(local).await.expect("connect");
            let name = ServerName::try_from(domain).expect("name");
            TlsConnector::from(Arc::new(trusting(&roots, alpn))).connect(name, tcp).await
        }
    };
    let mut first = connect(&[b"http/1.1"]).await.expect("a client that verifies the chain to Pebble's root");
    let first_leaf = first.get_ref().1.peer_certificates().expect("certificates")[0].as_ref().to_vec();
    let mut hello = [0u8; 20];
    first.read_exact(&mut hello).await.expect("hello");
    assert_eq!(&hello, b"hello from holzwave\n");
    assert!(!first_leaf.windows(10).any(|w| w == [0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f]), "not a challenge certificate");
    let first_serial = serial_hex(&first_leaf);
    let issuer = issuer_text(&first_leaf);
    assert!(issuer.contains("Pebble"), "{issuer}");
    // A client that offers the challenge protocol for the domain gets nothing now that no challenge is pending.
    assert!(connect(&[b"acme-tls/1"]).await.is_err(), "no challenge is pending, so no certificate");

    // 3. A renewal on demand: a different serial; the old connection goes on.
    let again = manager.order_once().await.expect("the second order");
    manager.accept(&again, clock.now()).expect("the second certificate is accepted");
    assert_eq!(resolver.pending_count(), 0);
    first.write_all(b"x").await.expect("the old connection still writes");
    let mut echo = [0u8; 1];
    first.read_exact(&mut echo).await.expect("the old connection still reads");
    assert_eq!(&echo, b"x");
    let second = connect(&[b"http/1.1"]).await.expect("the second handshake");
    let second_leaf = second.get_ref().1.peer_certificates().expect("certificates")[0].as_ref().to_vec();
    let second_serial = serial_hex(&second_leaf);
    assert_ne!(first_serial, second_serial, "a renewal is a different certificate");
    println!("Pebble: serial {} then {}, issuer {issuer}", &first_serial[..8], &second_serial[..8]);

    // 4. A name that does not resolve: the order fails with a sentence, the certificate in use stays, nothing is pending.
    let other_dir = Scratch::new("acme-pebble-failure");
    let broken_name = "does-not-resolve.example.test".to_owned();
    let broken = AcmeSettings::new(&directory, "ops@example.test", std::slice::from_ref(&broken_name), Some(ca_file.into()), true, Some(&broken_name))
        .expect("settings");
    let broken = AcmeManager::new(broken, resolver.clone(), &server_key(9), other_dir.path(), clock).expect("manager");
    let failure = broken.order_once().await.expect_err("an order for a name that does not resolve fails");
    println!("Failure case: {failure}");
    assert!(!failure.sentence().is_empty() && !failure.sentence().contains('\n'));
    assert_eq!(resolver.pending_count(), 0, "the challenge of the failed order was cleared");
    let still = resolver.certified_key().expect("a certificate is still served");
    assert_eq!(still.cert[0].as_ref(), second_leaf.as_slice(), "the previous certificate is still the one served");
    let third = connect(&[b"http/1.1"]).await.expect("a handshake after the failed order");
    assert_eq!(third.get_ref().1.peer_certificates().expect("certificates")[0].as_ref(), second_leaf.as_slice());
}

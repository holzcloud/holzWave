//! The one place every byte to a model provider passes through.
//!
//! TLS comes from `rustls` with the `ring` provider, passed to the configuration explicitly, and from the certificate
//! verifier of the operating system (`rustls-platform-verifier`). Nothing is installed as the process-wide default
//! crypto provider, so the SSH client and the WebDAV client keep the configuration they have. The client follows no
//! redirect, sends no `Accept-Encoding` and has no overall deadline: a stream may be long, and Stop is the user's way
//! to end it. There is no switch that turns the certificate check off.
//!
//! Where a request may go is decided here and nowhere else, from the address the host name resolves to and never from
//! a label:
//!
//! - the host is resolved once per request and every address is classified; one refused address (link-local, cloud
//!   metadata, unspecified, multicast, reserved) refuses the host, and the widest class of the answer is the locality;
//! - plain http is allowed to the loopback class, and to the local network only when the profile says so; the cloud
//!   class needs https, and the local-only switch refuses everything that leaves the device before a connection exists;
//! - loopback and local-network requests are pinned to the checked addresses and never use a proxy, so a name cannot
//!   answer differently a second time; cloud requests use the system proxy, which resolves the name itself;
//! - a key travels only in a [`SecretHeader`] bound to the origin it was filed for, and never in an address.
//!
//! This is the only first-party file that names the HTTP client crate (`core/tests/project_policy.rs` enforces it).

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use rustls_platform_verifier::BuilderVerifierExt;
use zeroize::Zeroizing;

use super::provider::{AiError, CancelToken, Detail, Flow};
use super::sse::{FrameError, NdjsonDecoder, SseDecoder, SseEvent, StreamEnd};
use super::types::{ApiKey, Locality, Size};
use crate::Error;

/// How long a connection attempt may take, the TCP connect and the TLS handshake together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The longest silence between two reads of a response. It restarts after every successful read, so a long answer that
/// keeps arriving never hits it; a server that goes quiet does.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long an unused connection stays in the pool.
pub const POOL_IDLE: Duration = Duration::from_secs(60);

/// The longest base address or path that is accepted, in bytes.
pub const MAX_URL: usize = 2048;
/// The largest request body: 16 MiB.
pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// The most bytes one response stream may carry: 64 MiB.
pub const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// The most bytes of an error body that are read: 64 KiB.
pub const MAX_ERROR_BODY: usize = 64 * 1024;
/// How long a host-name lookup may take.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest header name, in characters.
pub const MAX_HEADER_NAME: usize = 64;
/// The longest header value, in bytes.
pub const MAX_HEADER_VALUE: usize = 8192;
/// How many times a request that failed before its first body byte is tried again.
pub const MAX_RETRIES: u32 = 2;
/// The wait before the first retry; it doubles for the next one.
pub const RETRY_BASE: Duration = Duration::from_millis(500);
/// The longest wait before a retry. A server that asks for more is reported, not waited for.
pub const RETRY_MAX_WAIT: Duration = Duration::from_secs(20);

/// A key at least this long is looked for in the request address; a shorter one would match by accident.
const MIN_KEY_IN_URL: usize = 8;

/// The most addresses of one answer that are kept and pinned.
const MAX_ADDRESSES: usize = 16;

/// Whether a request follows the proxy settings of the environment and the operating system.
///
/// There is no default on purpose: a caller states the choice. A proxy resolves the host name itself, so an address that
/// the caller resolved and pinned would not be the one used, and a proxy would receive the traffic of a loopback or
/// private-network server. Those requests use [`ProxyPolicy::Direct`]; [`ProxyPolicy::System`] is for cloud
/// destinations, which a corporate network may force through its proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyPolicy {
    /// Follow the environment and the operating system.
    System,
    /// Connect directly, whatever the environment says.
    Direct,
}

/// What a caller chooses when it builds a client.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Refuse every request whose scheme is not https, before any connection is made.
    pub https_only: bool,
    /// Whether the proxy settings apply.
    pub proxy: ProxyPolicy,
    /// The bound on one connection attempt.
    pub connect_timeout: Duration,
    /// The bound on the silence between two reads.
    pub read_timeout: Duration,
}

impl ClientOptions {
    /// The options with the default timeouts ([`CONNECT_TIMEOUT`] and [`IDLE_TIMEOUT`]). There is no `Default`: the
    /// caller says whether https is required and whether the proxy applies.
    pub fn new(https_only: bool, proxy: ProxyPolicy) -> Self {
        Self { https_only, proxy, connect_timeout: CONNECT_TIMEOUT, read_timeout: IDLE_TIMEOUT }
    }
}

/// The TLS configuration of the client: TLS 1.2 and 1.3 with the `ring` provider and the platform verifier.
///
/// The provider is handed to the builder; nothing is installed as the process default. The configuration offers no way
/// to skip the certificate check.
pub fn tls_config() -> crate::Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Unsupported(format!("ai http: tls: {e}")))?;
    let builder = builder.with_platform_verifier().map_err(|e| Error::Unsupported(format!("ai http: tls: {e}")))?;
    Ok(builder.with_no_client_auth())
}

/// A host name pinned to the addresses that were checked: the client connects to these and never looks the name up.
struct Pin<'a> {
    host: &'a str,
    addrs: &'a [SocketAddr],
}

/// The one builder behind [`build_client`] and the per-request clients of [`Http`].
///
/// A `single_use` client keeps no idle connection: it serves one request and is dropped with it, so a pinned client
/// cannot hold a connection for a later request that should have been looked up again.
fn build(
    options: &ClientOptions,
    tls: rustls::ClientConfig,
    pin: Option<Pin<'_>>,
    single_use: bool,
) -> crate::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(options.https_only)
        .connect_timeout(options.connect_timeout)
        .read_timeout(options.read_timeout)
        .pool_idle_timeout(POOL_IDLE);
    if options.proxy == ProxyPolicy::Direct {
        builder = builder.no_proxy();
    }
    if let Some(pin) = pin {
        builder = builder.resolve_to_addrs(pin.host, pin.addrs);
    }
    if single_use {
        builder = builder.pool_max_idle_per_host(0);
    }
    builder.build().map_err(|e| Error::Unsupported(format!("ai http: client: {}", e.without_url())))
}

/// Builds the HTTP client of the AI chat.
///
/// Building opens no socket. The client is cheap to clone and should be built once and cloned. A redirect is never
/// followed (it would send the prompt to another host), there is no overall timeout, and no compression feature is
/// compiled, so no `Accept-Encoding` is sent and a decompressor can never hold streamed chunks back; a feature added
/// later must keep that (a test pins it). The HTTP crate is named in this file only.
///
/// Errors are `Error::Unsupported` with the fixed phrases `ai http: tls: ` and `ai http: client: ` and the reason of
/// the library; they never carry a URL.
pub fn build_client(options: &ClientOptions) -> crate::Result<reqwest::Client> {
    build(options, tls_config()?, None, false)
}

// ---- refusals -------------------------------------------------------------------------------------------

/// Why a destination, an address or a header is refused. The text is a fixed phrase and never holds the input, so it
/// is safe to show and to log. `as_str` is the stable name the settings dialog keys its text on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The text is not an address.
    Syntax,
    /// The text is longer than [`MAX_URL`].
    TooLong,
    /// The scheme is not https or http.
    Scheme,
    /// The address carries a user name or a password.
    Credentials,
    /// There is no host.
    Host,
    /// The port is zero.
    Port,
    /// The address carries a query or a fragment.
    Query,
    /// The path holds a backslash, an encoded separator or dot, or a dot segment.
    Path,
    /// Plain http where only https (or loopback) is allowed.
    PlainHttp,
    /// A link-local address.
    LinkLocal,
    /// A cloud metadata address.
    Metadata,
    /// The unspecified address.
    Unspecified,
    /// A multicast address.
    Multicast,
    /// A reserved address, including the broadcast address.
    Reserved,
    /// The local-only switch is on and the destination is not on this computer.
    LocalOnly,
    /// A key was filed for another origin.
    KeyScope,
    /// A key is part of the request address.
    KeyInUrl,
    /// A header name or value is not allowed.
    Header,
}

impl Refusal {
    /// Every variant, in the order of the type.
    pub const ALL: [Refusal; 18] = [
        Refusal::Syntax,
        Refusal::TooLong,
        Refusal::Scheme,
        Refusal::Credentials,
        Refusal::Host,
        Refusal::Port,
        Refusal::Query,
        Refusal::Path,
        Refusal::PlainHttp,
        Refusal::LinkLocal,
        Refusal::Metadata,
        Refusal::Unspecified,
        Refusal::Multicast,
        Refusal::Reserved,
        Refusal::LocalOnly,
        Refusal::KeyScope,
        Refusal::KeyInUrl,
        Refusal::Header,
    ];

    /// The stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Syntax => "syntax",
            Refusal::TooLong => "too_long",
            Refusal::Scheme => "scheme",
            Refusal::Credentials => "credentials",
            Refusal::Host => "host",
            Refusal::Port => "port",
            Refusal::Query => "query",
            Refusal::Path => "path",
            Refusal::PlainHttp => "plain_http",
            Refusal::LinkLocal => "link_local",
            Refusal::Metadata => "metadata",
            Refusal::Unspecified => "unspecified",
            Refusal::Multicast => "multicast",
            Refusal::Reserved => "reserved",
            Refusal::LocalOnly => "local_only",
            Refusal::KeyScope => "key_scope",
            Refusal::KeyInUrl => "key_in_url",
            Refusal::Header => "header",
        }
    }

    fn phrase(self) -> &'static str {
        match self {
            Refusal::Syntax => "the address is not a valid address",
            Refusal::TooLong => "the address is too long",
            Refusal::Scheme => "the address must start with https or http",
            Refusal::Credentials => "the address must not hold a user name or a password",
            Refusal::Host => "the address has no usable host name",
            Refusal::Port => "the port of the address is not valid",
            Refusal::Query => "the address must not hold a query or a fragment",
            Refusal::Path => "the path of the address is not allowed",
            Refusal::PlainHttp => {
                "plain http is allowed only to this computer, or to the local network when the profile allows it"
            }
            Refusal::LinkLocal => "link-local addresses are refused",
            Refusal::Metadata => "cloud metadata addresses are refused",
            Refusal::Unspecified => "the unspecified address is refused",
            Refusal::Multicast => "multicast addresses are refused",
            Refusal::Reserved => "reserved addresses are refused",
            Refusal::LocalOnly => "local only is on, and this destination is not on this computer",
            Refusal::KeyScope => "the key was filed for another address",
            Refusal::KeyInUrl => "a key must not be part of an address",
            Refusal::Header => "a header name or value is not allowed",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.phrase())
    }
}

impl std::error::Error for Refusal {}

impl From<Refusal> for AiError {
    fn from(refusal: Refusal) -> Self {
        AiError::Policy(Detail::new(refusal.phrase()))
    }
}

// ---- where a request may go -----------------------------------------------------------------------------

/// The scheme of a destination: the only two the chat speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain http, allowed to the loopback class and, by opt-in, to the local network.
    Http,
    /// http over TLS.
    Https,
}

impl Scheme {
    /// `http` or `https`.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
}

/// The scheme, host and port a key is filed for and a request goes to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    scheme: Scheme,
    host: String,
    port: u16,
}

impl Origin {
    /// The scheme.
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The host in lower case, without brackets and without a trailing dot.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port, the default one filled in.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The text `scheme://host:port` (an IPv6 host in brackets). The port is always written, also the default one.
    /// This is the string the profile files next to a key.
    pub fn scope(&self) -> String {
        if self.host.contains(':') {
            format!("{}://[{}]:{}", self.scheme.as_str(), self.host, self.port)
        } else {
            format!("{}://{}:{}", self.scheme.as_str(), self.host, self.port)
        }
    }

    /// The exact inverse of [`Origin::scope`].
    pub fn from_scope(text: &str) -> Result<Origin, Refusal> {
        let (scheme_text, rest) = text.split_once("://").ok_or(Refusal::Syntax)?;
        let scheme = match scheme_text {
            "https" => Scheme::Https,
            "http" => Scheme::Http,
            _ => return Err(Refusal::Syntax),
        };
        if rest.is_empty() || rest.chars().any(|c| c.is_whitespace() || c.is_control() || "/@?#\\".contains(c)) {
            return Err(Refusal::Syntax);
        }
        let (host, port_text) = if let Some(inner) = rest.strip_prefix('[') {
            let (host, after) = inner.split_once(']').ok_or(Refusal::Syntax)?;
            let port_text = after.strip_prefix(':').ok_or(Refusal::Syntax)?;
            let address: Ipv6Addr = host.parse().map_err(|_| Refusal::Syntax)?;
            (address.to_string(), port_text)
        } else {
            let (host, port_text) = rest.rsplit_once(':').ok_or(Refusal::Syntax)?;
            if host.is_empty() || host.contains(':') || host.contains('[') || host.contains(']') {
                return Err(Refusal::Syntax);
            }
            (host.to_ascii_lowercase(), port_text)
        };
        if port_text.is_empty() || !port_text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Refusal::Syntax);
        }
        let port: u16 = port_text.parse().map_err(|_| Refusal::Syntax)?;
        if port == 0 {
            return Err(Refusal::Syntax);
        }
        Ok(Origin { scheme, host, port })
    }
}

/// Where a request is allowed to go. There is no `Default`: the caller states both switches.
///
/// `allow_lan_http` is the profile's opt-in for plain http to the local network. `local_only` is the privacy switch:
/// it refuses every destination that leaves this computer, the local network included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkPolicy {
    /// Plain http to the local network is allowed.
    pub allow_lan_http: bool,
    /// Only destinations on this computer are allowed.
    pub local_only: bool,
}

impl NetworkPolicy {
    /// A policy.
    pub fn new(allow_lan_http: bool, local_only: bool) -> Self {
        Self { allow_lan_http, local_only }
    }

    /// Whether a request with `scheme` may go to a destination of class `locality`. A refused address class never
    /// reaches this function: [`classify_ip`] has already turned it into a [`Refusal`].
    pub fn permits(&self, scheme: Scheme, locality: Locality) -> Result<(), Refusal> {
        // The local-only switch is decided first and on the class alone: one line decides what leaves the device.
        if self.local_only && locality.leaves_device() {
            return Err(Refusal::LocalOnly);
        }
        match scheme {
            Scheme::Https => Ok(()),
            Scheme::Http => match locality {
                Locality::OnDevice | Locality::Loopback => Ok(()),
                Locality::Lan if self.allow_lan_http => Ok(()),
                Locality::Lan | Locality::Cloud => Err(Refusal::PlainHttp),
            },
        }
    }
}

/// Metadata endpoints of the large cloud hosts and the addresses that give instance credentials, each with the reason
/// it is refused. They are checked before the local-network ranges, because one of them (100.100.100.200) lies inside one.
const METADATA_V4: [([u8; 4], &str); 5] = [
    ([169, 254, 169, 254], "the instance metadata service of the large cloud hosts"),
    ([169, 254, 170, 2], "the container credentials endpoint"),
    ([100, 100, 100, 200], "the instance metadata service of Alibaba Cloud"),
    ([168, 63, 129, 16], "the virtual public address of the Azure platform services"),
    ([192, 0, 0, 192], "the instance metadata service of Oracle Cloud"),
];

/// The IPv6 metadata endpoint of the large cloud hosts (`fd00:ec2::254`).
const METADATA_V6: [u16; 8] = [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254];

/// The class of an address, or the reason it is refused.
///
/// An IPv4-mapped IPv6 address and an address in the well-known NAT64 prefix (`64:ff9b::/96`) are classified by the
/// IPv4 address they carry. Precedence: unspecified, metadata, link-local, multicast, reserved, loopback, local network
/// (10/8, 172.16/12, 192.168/16, 100.64/10, `fc00::/7`, `fec0::/10`), everything else is the cloud. The answer is
/// never `OnDevice`.
pub fn classify_ip(ip: IpAddr) -> Result<Locality, Refusal> {
    match canonical(ip) {
        IpAddr::V4(address) => classify_v4(address),
        IpAddr::V6(address) => classify_v6(address),
    }
}

/// The address with an embedded IPv4 address unwrapped.
fn canonical(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(address) = ip else { return ip };
    if let Some(mapped) = address.to_ipv4_mapped() {
        return IpAddr::V4(mapped);
    }
    let parts = address.segments();
    if parts[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let octets = address.octets();
        return IpAddr::V4(Ipv4Addr::new(octets[12], octets[13], octets[14], octets[15]));
    }
    ip
}

fn classify_v4(address: Ipv4Addr) -> Result<Locality, Refusal> {
    let [a, b, _, _] = address.octets();
    if a == 0 {
        return Err(Refusal::Unspecified);
    }
    if METADATA_V4.iter().any(|(octets, _)| Ipv4Addr::from(*octets) == address) {
        return Err(Refusal::Metadata);
    }
    if a == 169 && b == 254 {
        return Err(Refusal::LinkLocal);
    }
    if (224..=239).contains(&a) {
        return Err(Refusal::Multicast);
    }
    if a >= 240 {
        return Err(Refusal::Reserved);
    }
    if a == 127 {
        return Ok(Locality::Loopback);
    }
    let private = a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168);
    let shared = a == 100 && (64..=127).contains(&b);
    if private || shared { Ok(Locality::Lan) } else { Ok(Locality::Cloud) }
}

fn classify_v6(address: Ipv6Addr) -> Result<Locality, Refusal> {
    let parts = address.segments();
    if address.is_unspecified() {
        return Err(Refusal::Unspecified);
    }
    if parts == METADATA_V6 {
        return Err(Refusal::Metadata);
    }
    if parts[0] & 0xffc0 == 0xfe80 {
        return Err(Refusal::LinkLocal);
    }
    if parts[0] & 0xff00 == 0xff00 {
        return Err(Refusal::Multicast);
    }
    if address.is_loopback() {
        return Ok(Locality::Loopback);
    }
    let unique_local = parts[0] & 0xfe00 == 0xfc00;
    let site_local = parts[0] & 0xffc0 == 0xfec0;
    if unique_local || site_local { Ok(Locality::Lan) } else { Ok(Locality::Cloud) }
}

/// The proxy rule: loopback and local-network traffic never goes through a proxy (the proxy would resolve the name
/// itself and would receive the traffic of a local server); only the cloud class follows the system.
pub fn proxy_for(locality: Locality) -> ProxyPolicy {
    match locality {
        Locality::OnDevice | Locality::Loopback | Locality::Lan => ProxyPolicy::Direct,
        Locality::Cloud => ProxyPolicy::System,
    }
}

fn rank(locality: Locality) -> u8 {
    match locality {
        Locality::OnDevice => 0,
        Locality::Loopback => 1,
        Locality::Lan => 2,
        Locality::Cloud => 3,
    }
}

/// The wider of two classes: loopback, then the local network, then the cloud.
fn widest(a: Locality, b: Locality) -> Locality {
    if rank(b) > rank(a) { b } else { a }
}

/// A base address that passed the syntax and literal-address rules.
///
/// Parsing never looks a name up and never consults the local-only switch; both happen at send time, on the resolved
/// addresses. `Debug` prints the origin only: a base address may hold a path that names a private gateway.
#[derive(Clone)]
pub struct Target {
    url: reqwest::Url,
    origin: Origin,
    base_path: String,
    literal: Option<Locality>,
}

/// Whether `text` holds a percent-encoded slash, backslash or dot, in any case.
fn has_encoded_separator(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(3).any(|w| {
        w[0] == b'%'
            && matches!(
                (w[1].to_ascii_lowercase(), w[2].to_ascii_lowercase()),
                (b'2', b'f') | (b'5', b'c') | (b'2', b'e')
            )
    })
}

impl Target {
    /// Checks a base address typed by the user. `allow_lan_http` is the profile's opt-in for plain http to the local
    /// network.
    ///
    /// Rules, in order: the text is trimmed; empty is `Syntax`; over [`MAX_URL`] is `TooLong`; a control character or
    /// white space inside is `Syntax`; a backslash or a percent-encoded slash, backslash or dot is `Path`; a text the
    /// URL parser rejects is `Syntax`; a scheme other than https and http is `Scheme`; a user name or password is
    /// `Credentials`; no host is `Host`; port 0 is `Port`; a query or fragment is `Query`; an IP literal must be an
    /// allowed class for the scheme; a host name with plain http is allowed only for `localhost` (and `*.localhost`) or
    /// with the opt-in.
    pub fn parse(raw: &str, allow_lan_http: bool) -> Result<Target, Refusal> {
        let raw = raw.trim_matches(|c: char| c.is_ascii_whitespace());
        if raw.is_empty() {
            return Err(Refusal::Syntax);
        }
        if raw.len() > MAX_URL {
            return Err(Refusal::TooLong);
        }
        if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(Refusal::Syntax);
        }
        // The URL parser would rewrite these, so they are looked for in the text as typed.
        if raw.contains('\\') || has_encoded_separator(raw) {
            return Err(Refusal::Path);
        }
        let url = reqwest::Url::parse(raw).map_err(|_| Refusal::Syntax)?;
        let scheme = match url.scheme() {
            "https" => Scheme::Https,
            "http" => Scheme::Http,
            _ => return Err(Refusal::Scheme),
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Refusal::Credentials);
        }
        let shown = url.host_str().ok_or(Refusal::Host)?;
        let host = shown.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(shown);
        let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        if host.is_empty() {
            return Err(Refusal::Host);
        }
        if url.port() == Some(0) {
            return Err(Refusal::Port);
        }
        let port = url.port_or_known_default().ok_or(Refusal::Port)?;
        if url.query().is_some() || url.fragment().is_some() {
            return Err(Refusal::Query);
        }
        let base_path = url.path().trim_end_matches('/').to_owned();
        let literal = match host.parse::<IpAddr>() {
            Ok(ip) => {
                let locality = classify_ip(ip)?;
                NetworkPolicy::new(allow_lan_http, false).permits(scheme, locality)?;
                Some(locality)
            }
            Err(_) => {
                let local_name = host == "localhost" || host.ends_with(".localhost");
                if scheme == Scheme::Http && !local_name && !allow_lan_http {
                    return Err(Refusal::PlainHttp);
                }
                None
            }
        };
        Ok(Target { url, origin: Origin { scheme, host, port }, base_path, literal })
    }

    /// The scheme, host and port.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The path of the base address with no trailing slash: empty, or for example `/v1`.
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// The class of the host when it is an IP literal; `None` for a name (only the lookup can tell).
    pub fn literal_locality(&self) -> Option<Locality> {
        self.literal
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target").field("origin", &self.origin).finish()
    }
}

// ---- resolution -----------------------------------------------------------------------------------------

/// Turns a host name into addresses. The chokepoint calls it from a blocking thread, once per request.
pub trait Resolver: Send + Sync {
    /// The addresses of `host`. It may block.
    fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>>;
}

/// The resolver of the operating system.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn lookup(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        Ok((host, 0u16).to_socket_addrs()?.map(|address| address.ip()).collect())
    }
}

/// The checked answer of one lookup: the widest class and the addresses to pin.
#[derive(Clone)]
pub struct Resolved {
    /// The widest class of the answer.
    pub locality: Locality,
    /// The checked addresses with the port of the target.
    pub addrs: Vec<SocketAddr>,
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved").field("locality", &self.locality).field("addresses", &self.addrs.len()).finish()
    }
}

// ---- headers --------------------------------------------------------------------------------------------

/// Names a caller may never set: the request framing and the headers this file writes itself.
const FORBIDDEN_HEADERS: [&str; 14] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "accept-encoding",
    "content-type",
    "accept",
    "user-agent",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "expect",
    "proxy-connection",
];

/// Name fragments that mark a header as one that carries a secret. Such a header is a [`SecretHeader`] or nothing.
const SECRET_FRAGMENTS: [&str; 6] = ["auth", "key", "token", "secret", "password", "cookie"];

/// The name rule shared by both header types: lower case, 1 to [`MAX_HEADER_NAME`] characters of `a-z 0-9 -`, none of
/// the forbidden names.
fn header_name(name: &str) -> Result<String, Refusal> {
    let name = name.to_ascii_lowercase();
    let valid = !name.is_empty()
        && name.len() <= MAX_HEADER_NAME
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid || FORBIDDEN_HEADERS.contains(&name.as_str()) {
        return Err(Refusal::Header);
    }
    Ok(name)
}

/// Printable ASCII only (0x20 to 0x7E): no line break, no NUL, nothing above.
fn header_text(value: &str) -> bool {
    value.len() <= MAX_HEADER_VALUE && value.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// A request header that carries no secret, for example `anthropic-version`.
///
/// A name that looks like a secret (it holds `auth`, `key`, `token`, `secret`, `password` or `cookie`) is refused:
/// credentials travel in a [`SecretHeader`], which is bound to an origin. `Debug` prints the name and the size of the
/// value.
#[derive(Clone)]
pub struct Header {
    name: String,
    value: String,
}

impl Header {
    /// Checks and builds a header.
    pub fn new(name: &str, value: &str) -> Result<Header, Refusal> {
        let name = header_name(name)?;
        if SECRET_FRAGMENTS.iter().any(|fragment| name.contains(fragment)) || !header_text(value) {
            return Err(Refusal::Header);
        }
        Ok(Header { name, value: value.to_owned() })
    }

    /// The lower-case name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Header").field("name", &self.name).field("value", &Size(self.value.len())).finish()
    }
}

/// A header that carries a key, bound to the origin the key was filed for.
///
/// The value sent is `prefix` plus the key. A call refuses to send it to any other origin and refuses a key that
/// appears in the request address. The value is marked sensitive, so the client library never prints it. The name rule
/// is the [`Header`] rule without the refusal of secret-looking names, since this is where such names belong
/// (`authorization`, `x-api-key`). `Debug` prints the name and the scope, never the key.
pub struct SecretHeader {
    name: String,
    prefix: String,
    key: ApiKey,
    scope: Origin,
}

impl SecretHeader {
    /// Checks and builds a secret header. The key must not be empty and, with the prefix, must fit a header value.
    pub fn new(name: &str, prefix: &str, key: ApiKey, scope: Origin) -> Result<SecretHeader, Refusal> {
        let name = header_name(name)?;
        let value_ok = !key.is_empty()
            && header_text(prefix)
            && header_text(key.expose())
            && prefix.len() + key.expose().len() <= MAX_HEADER_VALUE;
        if !value_ok {
            return Err(Refusal::Header);
        }
        Ok(SecretHeader { name, prefix: prefix.to_owned(), key, scope })
    }

    /// The lower-case name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The origin the key was filed for.
    pub fn scope(&self) -> &Origin {
        &self.scope
    }

    /// `text` with every occurrence of the key replaced, for text that may echo it.
    pub fn redact_in(&self, text: &str) -> String {
        self.key.redact_in(text)
    }
}

impl fmt::Debug for SecretHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretHeader")
            .field("name", &self.name)
            .field("value", &format_args!("<redacted>"))
            .field("scope", &self.scope)
            .finish()
    }
}

// ---- one call -------------------------------------------------------------------------------------------

/// How the response body is cut into frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// Server-sent events.
    Sse,
    /// One JSON record per line.
    Ndjson,
}

/// One frame of a response body, borrowed for the duration of the callback.
pub enum Frame<'a> {
    /// A complete server-sent event.
    Event(SseEvent<'a>),
    /// A complete line, without its line end.
    Line(&'a [u8]),
}

impl fmt::Debug for Frame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Frame::Event(event) => f.debug_tuple("Event").field(event).finish(),
            Frame::Line(line) => f.debug_tuple("Line").field(&Size(line.len())).finish(),
        }
    }
}

/// One POST: where, what and how the answer is framed.
pub struct Call<'a> {
    target: &'a Target,
    policy: NetworkPolicy,
    path: &'a str,
    body: &'a [u8],
    framing: Framing,
    headers: Vec<&'a Header>,
    secrets: Vec<&'a SecretHeader>,
}

impl<'a> Call<'a> {
    /// A call. `path` starts with `/`, may carry a query, and is appended to the base path of `target`.
    pub fn new(target: &'a Target, policy: NetworkPolicy, path: &'a str, body: &'a [u8], framing: Framing) -> Self {
        Self { target, policy, path, body, framing, headers: Vec::new(), secrets: Vec::new() }
    }

    /// Adds a header.
    #[must_use]
    pub fn header(mut self, header: &'a Header) -> Self {
        self.headers.push(header);
        self
    }

    /// Adds a header that carries a key. The call is refused unless the key was filed for the origin of the target.
    #[must_use]
    pub fn secret(mut self, secret: &'a SecretHeader) -> Self {
        self.secrets.push(secret);
        self
    }
}

impl fmt::Debug for Call<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("target", &self.target)
            .field("path", &Size(self.path.len()))
            .field("body", &Size(self.body.len()))
            .field("framing", &self.framing)
            .field("headers", &self.headers.len())
            .field("secrets", &self.secrets.len())
            .finish()
    }
}

/// An answer that was not a success: the status, what the server asked for, and the start of its body.
#[derive(Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The status code.
    pub status: u16,
    /// The wait the server asked for, at most a day.
    pub retry_after: Option<Duration>,
    /// The first bytes of the body, at most the configured bound. It is untrusted and may echo a key.
    pub body: Vec<u8>,
}

impl fmt::Debug for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rejection")
            .field("status", &self.status)
            .field("retry_after", &self.retry_after)
            .field("body", &Size(self.body.len()))
            .finish()
    }
}

/// How a call ended when it ended without an error.
#[derive(Debug)]
pub enum Outcome {
    /// The body arrived and was fed to the callback; the end says whether it was clean.
    Streamed(StreamEnd),
    /// The server answered with a status that is not a success.
    Rejected(Rejection),
}

/// What a call returns: where the request went and how it ended.
#[derive(Debug)]
pub struct Exchange {
    /// The class of the addresses the request went to.
    pub locality: Locality,
    /// How it ended.
    pub outcome: Outcome,
}

/// How a failed request is repeated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The most repeats.
    pub max_retries: u32,
    /// The wait before the first repeat.
    pub base_delay: Duration,
    /// The longest wait; a longer `retry-after` is reported, not waited for.
    pub max_wait: Duration,
}

/// Every size and time of the chokepoint. The defaults are the constants of this file; a test passes small values.
#[derive(Clone, Copy, Debug)]
pub struct HttpConfig {
    /// The bound on one connection attempt.
    pub connect_timeout: Duration,
    /// The bound on the silence between two reads.
    pub idle_timeout: Duration,
    /// The retry rule.
    pub retry: RetryPolicy,
    /// The largest request body.
    pub max_request_bytes: usize,
    /// The most bytes of one response stream.
    pub max_stream_bytes: usize,
    /// The most bytes of an error body that are read.
    pub max_error_body: usize,
}

impl HttpConfig {
    /// The defaults.
    pub fn standard() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            retry: RetryPolicy { max_retries: MAX_RETRIES, base_delay: RETRY_BASE, max_wait: RETRY_MAX_WAIT },
            max_request_bytes: MAX_REQUEST_BYTES,
            max_stream_bytes: MAX_STREAM_BYTES,
            max_error_body: MAX_ERROR_BODY,
        }
    }
}

// ---- errors ---------------------------------------------------------------------------------------------

const PHRASE_REFUSED: &str = "the server refused the connection";
const PHRASE_RESET: &str = "the connection was reset";
const PHRASE_TIMED_OUT: &str = "the connection timed out";
const PHRASE_CERTIFICATE: &str = "the server certificate was not accepted";
const PHRASE_RESOLVE: &str = "the host name could not be resolved";
const PHRASE_FAILED: &str = "the connection failed";

/// One of six fixed phrases for a transport failure. Only `io::ErrorKind` values and the lower-cased text of the error
/// chain are read to choose it; the library's text, an address or a port is never returned.
pub fn transport_phrase(error: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut chain: Vec<&(dyn std::error::Error + 'static)> = Vec::new();
    let mut next = Some(error);
    while let Some(current) = next {
        chain.push(current);
        next = current.source();
        if chain.len() > 16 {
            break;
        }
    }
    let texts: Vec<String> = chain.iter().map(|e| e.to_string().to_ascii_lowercase()).collect();
    // A refused certificate is decided first: it is never retried, whatever else the chain says.
    if texts.iter().any(|text| text.contains("certificate")) {
        return PHRASE_CERTIFICATE;
    }
    for item in &chain {
        if let Some(io) = item.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::ConnectionRefused => return PHRASE_REFUSED,
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted => return PHRASE_RESET,
                std::io::ErrorKind::TimedOut => return PHRASE_TIMED_OUT,
                std::io::ErrorKind::NotFound => return PHRASE_RESOLVE,
                _ => {}
            }
        }
    }
    if texts.iter().any(|text| text.contains("dns") || text.contains("resolve")) {
        return PHRASE_RESOLVE;
    }
    PHRASE_FAILED
}

/// The stable error of a status that is not a success. The text is a fixed phrase with the number and never holds body
/// text; an adapter that can read the provider's own error body maps it through `Endpoint::scrub` instead.
pub fn status_error(rejection: &Rejection) -> AiError {
    let status = rejection.status;
    let said = |what: &str| Detail::new(&format!("the server answered with status {status}{what}"));
    match status {
        300..=399 => AiError::Policy(said(", a redirect that is never followed; use the final address")),
        401..=403 => AiError::Auth(said("")),
        408 => AiError::Timeout,
        413 => AiError::ContextLength,
        429 => AiError::RateLimited { retry_after: rejection.retry_after },
        503 | 529 => AiError::Overloaded,
        400..=499 => AiError::InvalidRequest(said("")),
        500..=599 => AiError::Server { status, detail: said("") },
        _ => AiError::Protocol(said("")),
    }
}

/// Whether a status is worth another try before any body byte: 429, 500, 502, 503, 504 and 529.
pub fn retries_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504 | 529)
}

/// Whether a connect failure with this phrase is worth another try: every one except a refused certificate.
pub fn retries_phrase(phrase: &str) -> bool {
    phrase != PHRASE_CERTIFICATE
}

fn policy_error(text: &str) -> AiError {
    AiError::Policy(Detail::new(text))
}

fn network_error(phrase: &str) -> AiError {
    AiError::Network(Detail::new(phrase))
}

fn unavailable(text: &str) -> AiError {
    AiError::Unavailable(Detail::new(text))
}

/// The error of a failed request, with the library's text left out.
fn map_error(error: reqwest::Error) -> AiError {
    let error = error.without_url();
    if error.is_timeout() {
        AiError::Timeout
    } else if error.is_builder() || error.is_redirect() {
        policy_error("the request was refused by the client")
    } else {
        network_error(transport_phrase(&error))
    }
}

/// The `retry-after` header as whole seconds, at most a day. A date or other text is `None`.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: u64 = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds.min(86_400)))
}

// ---- the chokepoint -------------------------------------------------------------------------------------

/// Runs `future` unless the token fires first. Cancel wins a tie.
async fn raced<T>(cancel: &CancelToken, future: impl Future<Output = T>) -> Result<T, AiError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(AiError::Cancelled),
        output = future => Ok(output),
    }
}

/// The request as it will be sent.
struct Prepared {
    url: reqwest::Url,
    headers: HeaderMap,
}

/// The frame decoder of a stream: the response is pushed into it chunk by chunk, as it arrives.
enum Decoder {
    Sse(SseDecoder),
    Ndjson(NdjsonDecoder),
}

impl Decoder {
    fn new(framing: Framing) -> Self {
        match framing {
            Framing::Sse => Decoder::Sse(SseDecoder::new()),
            Framing::Ndjson => Decoder::Ndjson(NdjsonDecoder::new()),
        }
    }

    fn push(&mut self, chunk: &[u8], on_frame: &mut dyn FnMut(Frame<'_>) -> Flow) -> Result<Flow, FrameError> {
        match self {
            Decoder::Sse(decoder) => decoder.push(chunk, &mut |event| on_frame(Frame::Event(event))),
            Decoder::Ndjson(decoder) => decoder.push(chunk, &mut |line| on_frame(Frame::Line(line))),
        }
    }

    fn finish(self) -> StreamEnd {
        match self {
            Decoder::Sse(decoder) => decoder.finish(),
            Decoder::Ndjson(decoder) => decoder.finish(),
        }
    }
}

/// The HTTP chokepoint: resolves, classifies, pins and sends, and feeds the answer to a callback as it arrives.
///
/// Construction opens no socket and starts no thread; the private runtime, the TLS configuration and the shared cloud
/// client are built on the first request. One `Http` serves several threads at once.
pub struct Http {
    config: HttpConfig,
    resolver: Arc<dyn Resolver>,
    attempts: AtomicU64,
    runtime: OnceLock<tokio::runtime::Runtime>,
    tls: OnceLock<Arc<rustls::ClientConfig>>,
    cloud: OnceLock<reqwest::Client>,
}

impl Http {
    /// A chokepoint that resolves names with the operating system.
    pub fn new(config: HttpConfig) -> Self {
        Self::with_resolver(config, Arc::new(SystemResolver))
    }

    /// A chokepoint with its own resolver, for tests.
    pub fn with_resolver(config: HttpConfig, resolver: Arc<dyn Resolver>) -> Self {
        Self {
            config,
            resolver,
            attempts: AtomicU64::new(0),
            runtime: OnceLock::new(),
            tls: OnceLock::new(),
            cloud: OnceLock::new(),
        }
    }

    /// How many requests were sent. A refusal before the send does not count.
    pub fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    fn runtime(&self) -> Result<&tokio::runtime::Runtime, AiError> {
        if let Some(runtime) = self.runtime.get() {
            return Ok(runtime);
        }
        let built = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hw-ai-http")
            .enable_all()
            .build()
            .map_err(|_| unavailable("the network runtime could not be started"))?;
        Ok(self.runtime.get_or_init(|| built))
    }

    fn tls(&self) -> Result<Arc<rustls::ClientConfig>, AiError> {
        if let Some(config) = self.tls.get() {
            return Ok(config.clone());
        }
        let built = Arc::new(tls_config().map_err(|error| unavailable(&error.to_string()))?);
        Ok(self.tls.get_or_init(|| built).clone())
    }

    /// Resolves and classifies the host of `target` without sending anything. Blocking. This performs a name lookup, so
    /// it is for an explicit user action only.
    pub fn classify(&self, target: &Target, policy: &NetworkPolicy, cancel: &CancelToken) -> Result<Resolved, AiError> {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        self.runtime()?.block_on(async { raced(cancel, self.resolve(target, policy)).await? })
    }

    /// One lookup, every address classified, one refused address refuses the host.
    async fn resolve(&self, target: &Target, policy: &NetworkPolicy) -> Result<Resolved, AiError> {
        let origin = target.origin();
        let found: Vec<IpAddr> = match origin.host().parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => {
                let resolver = self.resolver.clone();
                let name = origin.host().to_owned();
                let lookup = tokio::task::spawn_blocking(move || resolver.lookup(&name));
                match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await {
                    Ok(Ok(Ok(found))) if !found.is_empty() => found,
                    _ => return Err(network_error(PHRASE_RESOLVE)),
                }
            }
        };
        let mut locality = Locality::Loopback;
        let mut addrs = Vec::with_capacity(found.len().min(MAX_ADDRESSES));
        for ip in found.into_iter().take(MAX_ADDRESSES) {
            locality = widest(locality, classify_ip(ip).map_err(AiError::from)?);
            addrs.push(SocketAddr::new(ip, origin.port()));
        }
        policy.permits(origin.scheme(), locality).map_err(AiError::from)?;
        Ok(Resolved { locality, addrs })
    }

    /// The client for one request. Loopback and local-network requests get a fresh client pinned to the checked
    /// addresses and no proxy; the cloud class shares one client that follows the system proxy and requires https.
    fn client_for(&self, target: &Target, resolved: &Resolved) -> Result<reqwest::Client, AiError> {
        let tls = self.tls()?;
        let build_error = |error: Error| unavailable(&error.to_string());
        let proxy = proxy_for(resolved.locality);
        let origin = target.origin();
        if proxy == ProxyPolicy::System {
            if let Some(client) = self.cloud.get() {
                return Ok(client.clone());
            }
            let options = ClientOptions {
                https_only: true,
                proxy,
                connect_timeout: self.config.connect_timeout,
                read_timeout: self.config.idle_timeout,
            };
            let built = build(&options, (*tls).clone(), None, false).map_err(build_error)?;
            return Ok(self.cloud.get_or_init(|| built).clone());
        }
        let options = ClientOptions {
            https_only: origin.scheme() == Scheme::Https,
            proxy,
            connect_timeout: self.config.connect_timeout,
            read_timeout: self.config.idle_timeout,
        };
        // An IP literal needs no lookup, so there is nothing to pin.
        let pinned = origin.host().parse::<IpAddr>().is_err();
        let pin = pinned.then_some(Pin { host: origin.host(), addrs: &resolved.addrs });
        build(&options, (*tls).clone(), pin, true).map_err(build_error)
    }

    /// The address and headers of a call.
    fn prepare(&self, call: &Call<'_>) -> Result<Prepared, AiError> {
        let (path, query) = split_path(call.path)?;
        let mut url = call.target.url.clone();
        url.set_path(&format!("{}{}", call.target.base_path, path));
        url.set_query(query);
        // A key goes to the origin it was filed for and never into an address; both are decided before anything is
        // resolved or connected.
        for secret in &call.secrets {
            if secret.scope != call.target.origin {
                return Err(Refusal::KeyScope.into());
            }
            let key = secret.key.expose();
            if key.len() >= MIN_KEY_IN_URL && (url.as_str().contains(key) || call.path.contains(key)) {
                return Err(Refusal::KeyInUrl.into());
            }
        }
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(match call.framing {
                Framing::Sse => "text/event-stream",
                Framing::Ndjson => "application/x-ndjson",
            }),
        );
        headers.insert(USER_AGENT, HeaderValue::from_static(concat!("holzWave/", env!("CARGO_PKG_VERSION"))));
        for header in &call.headers {
            let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| AiError::from(Refusal::Header))?;
            let value = HeaderValue::from_str(&header.value).map_err(|_| AiError::from(Refusal::Header))?;
            headers.append(name, value);
        }
        for secret in &call.secrets {
            let name = HeaderName::from_bytes(secret.name.as_bytes()).map_err(|_| AiError::from(Refusal::Header))?;
            let text = Zeroizing::new(format!("{}{}", secret.prefix, secret.key.expose()));
            let mut value = HeaderValue::from_str(&text).map_err(|_| AiError::from(Refusal::Header))?;
            value.set_sensitive(true);
            headers.append(name, value);
        }
        Ok(Prepared { url, headers })
    }

    /// Sends one POST and feeds the response to `on_frame` as it arrives. Blocking: run it on a worker thread.
    ///
    /// The callback answers [`Flow::Stop`] to end the call; that, and the token, close the connection and return
    /// `Err(AiError::Cancelled)`. A status that is not a success comes back as [`Outcome::Rejected`], not as an error.
    pub fn post_stream(
        &self,
        call: &Call<'_>,
        on_frame: &mut dyn FnMut(Frame<'_>) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Exchange, AiError> {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        let prepared = self.prepare(call)?;
        self.runtime()?.block_on(self.exchange(call, &prepared, on_frame, cancel))
    }

    async fn exchange(
        &self,
        call: &Call<'_>,
        prepared: &Prepared,
        on_frame: &mut dyn FnMut(Frame<'_>) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Exchange, AiError> {
        let resolved = raced(cancel, self.resolve(call.target, &call.policy)).await??;
        let client = self.client_for(call.target, &resolved)?;
        let outcome = self.attempt(&client, call, prepared, on_frame, cancel).await?;
        Ok(Exchange { locality: resolved.locality, outcome })
    }

    /// One request on one connection, up to the end of the body.
    async fn attempt(
        &self,
        client: &reqwest::Client,
        call: &Call<'_>,
        prepared: &Prepared,
        on_frame: &mut dyn FnMut(Frame<'_>) -> Flow,
        cancel: &CancelToken,
    ) -> Result<Outcome, AiError> {
        let request = client.post(prepared.url.clone()).headers(prepared.headers.clone()).body(call.body.to_vec());
        self.attempts.fetch_add(1, Ordering::Relaxed);
        let mut response = raced(cancel, request.send()).await?.map_err(map_error)?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            return Ok(Outcome::Rejected(Rejection { status: status.as_u16(), retry_after, body: Vec::new() }));
        }
        // Each chunk goes straight into the decoder, so a frame reaches the callback when it arrives.
        let mut decoder = Decoder::new(call.framing);
        loop {
            let chunk = match raced(cancel, response.chunk()).await?.map_err(map_error)? {
                Some(chunk) => chunk,
                None => return Ok(Outcome::Streamed(decoder.finish())),
            };
            match decoder.push(&chunk, on_frame) {
                Ok(Flow::Continue) => {}
                // Leaving drops the response, which closes the connection.
                Ok(Flow::Stop) => return Err(AiError::Cancelled),
                Err(error) => return Err(AiError::from(error)),
            }
        }
    }
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Http")
            .field("attempts", &self.attempts())
            .field("started", &self.runtime.get().is_some())
            .finish()
    }
}

/// Splits a call path into the path and the query, and refuses a path that could leave the base path or hide a
/// separator.
fn split_path(path: &str) -> Result<(&str, Option<&str>), Refusal> {
    let (route, query) = match path.split_once('?') {
        Some((route, query)) => (route, Some(query)),
        None => (path, None),
    };
    let bad = !route.starts_with('/')
        || route.starts_with("//")
        || route.contains("..")
        || route.contains('\\')
        || path.contains('#')
        || path.chars().any(|c| c.is_control() || c.is_whitespace())
        || has_encoded_separator(route);
    if bad { Err(Refusal::Path) } else { Ok((route, query)) }
}

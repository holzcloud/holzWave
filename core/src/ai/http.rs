//! The one place that builds the HTTP client of the AI chat.
//!
//! TLS comes from `rustls` with the `ring` provider, passed to the configuration explicitly, and from the certificate
//! verifier of the operating system (`rustls-platform-verifier`). Nothing is installed as the process-wide default
//! crypto provider, so the SSH client and the WebDAV client keep the configuration they have. The client follows no
//! redirect, sends no `Accept-Encoding` and has no overall deadline: a stream may be long, and Stop is the user's way
//! to end it. There is no switch that turns the certificate check off.
//!
//! This is the only first-party file that names the HTTP client crate (`core/tests/project_policy.rs` enforces it).

use std::sync::Arc;
use std::time::Duration;

use rustls_platform_verifier::BuilderVerifierExt;

use crate::{Error, Result};

/// How long a connection attempt may take, the TCP connect and the TLS handshake together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The longest silence between two reads of a response. It restarts after every successful read, so a long answer that
/// keeps arriving never hits it; a server that goes quiet does.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long an unused connection stays in the pool.
pub const POOL_IDLE: Duration = Duration::from_secs(60);

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
pub fn tls_config() -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Unsupported(format!("ai http: tls: {e}")))?;
    let builder = builder.with_platform_verifier().map_err(|e| Error::Unsupported(format!("ai http: tls: {e}")))?;
    Ok(builder.with_no_client_auth())
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
pub fn build_client(options: &ClientOptions) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config()?)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(options.https_only)
        .connect_timeout(options.connect_timeout)
        .read_timeout(options.read_timeout)
        .pool_idle_timeout(POOL_IDLE);
    if options.proxy == ProxyPolicy::Direct {
        builder = builder.no_proxy();
    }
    builder.build().map_err(|e| Error::Unsupported(format!("ai http: client: {}", e.without_url())))
}

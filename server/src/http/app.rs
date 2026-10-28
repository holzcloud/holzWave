//! The shared state that every request handler reads, and the construction of the application.
//!
//! [`App`] is built once and held in an `Arc`. The router puts it into every request as an `Extension<Arc<App>>`, so a route
//! reads what it needs from it and adds no field to it: the services a route owns travel inside its own router state.
//!
//! [`StoreView`] is the one seam to the worker side. The HTTP side asks it one thing about a signed-in account, whether the
//! account's secret store is open or locked, and the worker side implements it when it exists.

use std::fmt;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

use crate::audit::Audit;
use crate::auth::session::SessionManager;
use crate::clock::Clock;
use crate::config::{self, Auth, Origin, Sessions};
use crate::keyfile::{KEY_BYTES, Purpose, ServerKey};
use crate::limits::Limits;
use crate::proxy::{Admission, Mode, TrustedProxies};
use crate::store::{AccountId, Session, Store};

use super::csrf;

/// The address people type in the browser, as the HTTP side compares it: the exact `Origin` text, the authority a `Host`
/// header must name, the host name and the port. The default port is never part of the authority text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicOrigin {
    origin: Origin,
}

impl PublicOrigin {
    /// Reads `public_url` with the rules of the configuration (`https`, a host, an optional port, no path).
    pub fn parse(url: &str) -> Result<PublicOrigin, config::Refusal> {
        config::public_origin(url).map(PublicOrigin::from_origin)
    }

    /// The origin of an already checked configuration.
    pub fn from_origin(origin: Origin) -> PublicOrigin {
        PublicOrigin { origin }
    }

    /// The exact value of an `Origin` header for this server, such as `https://hw.example.org:8443`.
    pub fn text(&self) -> &str {
        self.origin.as_str()
    }

    /// The host, with the port unless it is 443: what a `Host` header names and what `connect-src` allows.
    pub fn authority(&self) -> &str {
        self.origin.authority()
    }

    /// The host name in lower case, without brackets and without the port.
    pub fn host(&self) -> &str {
        self.origin.host()
    }

    /// The port; 443 when the address names none.
    pub fn port(&self) -> u16 {
        self.origin.port()
    }
}

/// Whether the secret store of an account can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreState {
    /// The store is open.
    Open,
    /// The store is locked: the person has to enter the passphrase or the recovery code.
    Locked,
}

/// The state of the secret store of one account. The worker side implements this; the HTTP side only reads it.
pub trait StoreView: Send + Sync {
    /// Whether the store of `account` is open or locked.
    fn state(&self, account: AccountId) -> StoreState;
}

/// A [`StoreView`] that reports every store as open: for tests and for a server whose worker side is not running yet.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysOpen;

impl StoreView for AlwaysOpen {
    fn state(&self, _account: AccountId) -> StoreState {
        StoreState::Open
    }
}

/// The already built services an [`App`] is made from.
pub struct AppInit {
    /// The public address.
    pub origin: PublicOrigin,
    /// The sign-in methods of the configuration.
    pub auth: Auth,
    /// The session times of the configuration.
    pub sessions: Sessions,
    /// The limits in force.
    pub limits: Limits,
    /// How the listener is secured.
    pub mode: Mode,
    /// The proxies whose word about the client counts (none unless the mode is the proxy mode).
    pub proxies: TrustedProxies,
    /// The database.
    pub store: Arc<Store>,
    /// The sessions.
    pub session_manager: Arc<SessionManager>,
    /// The audit log.
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The state of the secret stores.
    pub view: Arc<dyn StoreView>,
}

/// The application state of the HTTP side.
pub struct App {
    /// The public address.
    pub origin: PublicOrigin,
    /// The sign-in methods of the configuration.
    pub auth: Auth,
    /// The session times of the configuration.
    pub sessions: Sessions,
    /// The limits in force.
    pub limits: Limits,
    /// How the listener is secured.
    pub mode: Mode,
    /// The proxies whose word about the client counts.
    pub proxies: TrustedProxies,
    /// The database.
    pub store: Arc<Store>,
    /// The sessions.
    pub session_manager: Arc<SessionManager>,
    /// The audit log.
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The state of the secret stores.
    pub view: Arc<dyn StoreView>,
    csrf_key: Zeroizing<[u8; KEY_BYTES]>,
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("App(..)")
    }
}

impl App {
    /// Builds the application state. The anti-forgery key is the `csrf` purpose key of the server key.
    pub fn new(init: AppInit, key: &ServerKey) -> Arc<App> {
        Arc::new(App {
            origin: init.origin,
            auth: init.auth,
            sessions: init.sessions,
            limits: init.limits,
            mode: init.mode,
            proxies: init.proxies,
            store: init.store,
            session_manager: init.session_manager,
            audit: init.audit,
            clock: init.clock,
            view: init.view,
            csrf_key: key.derive(Purpose::Csrf),
        })
    }

    /// The anti-forgery token of a session: 43 characters, stateless, different for every session.
    pub fn csrf_token(&self, session: &Session) -> String {
        csrf::token_for(&self.csrf_key, &session.id_hash)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The accept loop
// ---------------------------------------------------------------------------------------------------------------------

/// The signal that ends the accept loop. Every clone sees the same state; triggering twice changes nothing.
#[derive(Clone)]
pub struct Shutdown {
    sender: Arc<watch::Sender<bool>>,
    receiver: watch::Receiver<bool>,
}

impl fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Shutdown(..)")
    }
}

impl Default for Shutdown {
    fn default() -> Shutdown {
        Shutdown::new()
    }
}

impl Shutdown {
    /// A signal that has not been triggered.
    pub fn new() -> Shutdown {
        let (sender, receiver) = watch::channel(false);
        Shutdown { sender: Arc::new(sender), receiver }
    }

    /// Triggers the signal.
    pub fn trigger(&self) {
        self.sender.send_replace(true);
    }

    /// Whether the signal was triggered.
    pub fn is_triggered(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Resolves when the signal is triggered (at once if it was).
    pub async fn wait(&self) {
        let mut receiver = self.receiver.clone();
        let _ = receiver.wait_for(|triggered| *triggered).await;
    }
}

/// Why an [`Edge`] cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeError {
    /// The modes that serve TLS themselves need an acceptor.
    NeedsTls,
    /// The proxy mode serves plain HTTP and must not have an acceptor.
    NoTlsBehindProxy,
}

impl fmt::Display for EdgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EdgeError::NeedsTls => "this mode serves TLS itself and needs a TLS acceptor",
            EdgeError::NoTlsBehindProxy => "the proxy mode serves plain HTTP to trusted proxies and takes no TLS acceptor",
        })
    }
}

impl std::error::Error for EdgeError {}

/// What the accept loop needs, all of it handed in: it reads no configuration itself.
pub struct Edge {
    router: Router,
    acceptor: Option<TlsAcceptor>,
    admission: Admission,
    mode: Mode,
}

impl Edge {
    /// The listener of a router. `acceptor` is `Some` in the `files` and `acme` modes and `None` in the proxy mode.
    pub fn new(router: Router, acceptor: Option<TlsAcceptor>, admission: Admission, mode: Mode) -> Result<Edge, EdgeError> {
        match (mode, &acceptor) {
            (Mode::Proxy, Some(_)) => Err(EdgeError::NoTlsBehindProxy),
            (Mode::Files | Mode::Acme, None) => Err(EdgeError::NeedsTls),
            _ => Ok(Edge { router, acceptor, admission, mode }),
        }
    }
}

/// Accepts connections on `listener` until `shutdown` is triggered.
pub async fn serve(listener: TcpListener, edge: Edge, shutdown: Shutdown) {
    let _ = (listener, edge, shutdown);
}

//! The assembly of the server: everything the checked configuration describes, built in one order and stopped in the reverse.
//!
//! [`assemble`] takes the configuration that `prepare` accepted and builds, in this order: the store, the audit log and the
//! clock; the sessions; the supervisor with the launcher the configuration names; the accounts service (told about accounts
//! that end); the sign-in services; the supervisor side of the secret store; the application state; the routes, in the one
//! list of [`router_parts`]; TLS by mode; the accept loop; the health socket with one readiness check per subsystem; and last
//! the one-time token of the first administrator, printed once. [`run`] is the binary's entry: it assembles with the default
//! [`Overrides`], waits for a signal and stops in order.
//!
//! A start that cannot be completed is one sentence, and nothing is left listening: every step that fails returns before the
//! next one begins, and what was built is dropped.
//!
//! The shutdown has one order: the accept loop stops, every socket is closed 1001, every worker is asked to shut down (one that
//! does not close its link in time is killed), the sockets' tasks are waited for, the background tasks end, the health socket
//! is removed, and [`Running::join`] returns. A second signal changes nothing.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use openidconnect::{AsyncHttpClient, HttpRequest};
use tokio::task::JoinHandle;

use super::gate::{GateServices, SupervisorGate};
use super::registry::{Supervisor, SupervisorSettings};
use super::launcher::WorkerLauncher;
use crate::accounts::{self, Accounts};
use crate::acme::{self, AcmeManager, AcmeResolver, AcmeSettings};
use crate::audit::Audit;
use crate::auth::oidc::{Http, OidcService};
use crate::auth::oidc_http::{HttpError, HttpFuture, HttpsClient};
use crate::auth::passkey::Passkeys;
use crate::auth::password::ArgonGate;
use crate::auth::session::SessionManager;
use crate::auth::signin::SignIn;
use crate::clock::{Clock, SystemClock};
use crate::config::{Auth, Prepared, Refusal, TlsSetup, Upload};
use crate::http::app::{App, AppInit, Edge, PublicOrigin, Shutdown, Timing};
use crate::http::features::DeviceSettings;
use crate::http::health::{self, Readiness};
use crate::http::routes_auth::{AuthServices, LandingPage, OidcFlow};
use crate::http::routes_enrol::{Enrol, EnrolServices};
use crate::http::routes_me::MeServices;
use crate::http::routes_admin::AdminServices;
use crate::http::router::{self, Part};
use crate::http::transport;
use crate::http::ws::{self, WsState};
use crate::http::ws_device;
use crate::http::{embed, routes_admin, routes_auth, routes_device, routes_enrol, routes_files, routes_me, routes_unlock};
use crate::log::{self, Level};
use crate::proxy::{Admission, Mode, TrustedProxies};
use crate::sealed::Sealer;
use crate::store::Store;
use crate::tls::{self, FileResolver};
use crate::unlock::Kdf;

/// A client for identity providers behind one type, so that the single sign-on service does not change with the adapter. The
/// product's adapter is the HTTPS-only client of `oidc_http`; a test hands in an in-memory one.
#[derive(Clone)]
pub struct OidcClient(Arc<dyn Http>);

impl OidcClient {
    /// Wraps any client of the service.
    pub fn new<H: Http>(client: H) -> OidcClient {
        OidcClient(Arc::new(client))
    }
}

impl std::fmt::Debug for OidcClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OidcClient(..)")
    }
}

impl<'c> AsyncHttpClient<'c> for OidcClient {
    type Error = HttpError;
    type Future = HttpFuture<'c>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        self.0.call(request)
    }
}

/// What a test may inject. An ordinary argument: nothing selects it and nothing in the assembly reads a field except where it
/// is used below. The binary always passes [`run_overrides`], which is the default.
#[derive(Default)]
pub struct Overrides {
    /// Listens here instead of at the configured address (a test asks for port 0).
    pub listen: Option<SocketAddr>,
    /// Where the one-time token is written instead of standard output.
    pub token_writer: Option<Box<dyn Write + Send>>,
    /// How long requests in flight may finish after the stop signal.
    pub grace: Option<Duration>,
    /// The clock of the sessions, the audit log and the supervisor.
    pub clock: Option<Arc<dyn Clock>>,
    /// The client for identity providers; the product's HTTPS-only client when `None`. The redirect, state, nonce, PKCE and
    /// claim rules stay in the service whatever the client is.
    pub oidc_client: Option<OidcClient>,
}

impl std::fmt::Debug for Overrides {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Overrides")
            .field("listen", &self.listen)
            .field("token_writer", &self.token_writer.is_some())
            .field("grace", &self.grace)
            .field("clock", &self.clock.is_some())
            .field("oidc_client", &self.oidc_client.is_some())
            .finish()
    }
}

/// The overrides the binary runs with: none.
pub fn run_overrides() -> Overrides {
    Overrides::default()
}

/// The shared handles of the running services, for the route lists and for the suites of later plans.
#[derive(Clone)]
pub struct Services {
    /// The workers.
    pub supervisor: Supervisor,
    /// The application state; it holds the configuration the routes read.
    pub app: Arc<App>,
    /// The sessions.
    pub sessions: Arc<SessionManager>,
    /// The store.
    pub store: Arc<Store>,
    /// The accounts.
    pub accounts: Arc<Accounts>,
    /// The audit log.
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The supervisor side of the secret store.
    pub gate: Arc<SupervisorGate>,
    /// The password and code sign-in.
    pub signin: Arc<SignIn>,
    /// The passkeys.
    pub passkeys: Arc<Passkeys>,
    /// Single sign-on, when a provider is configured.
    pub oidc: Option<Arc<dyn OidcFlow>>,
    /// The door to Argon2.
    pub argon: ArgonGate,
    /// Seals the authenticator secrets.
    pub sealer: Arc<Sealer>,
    /// The WebSocket endpoint.
    pub ws: Arc<WsState>,
    /// The upload settings, for the file routes.
    pub upload: Upload,
}

/// The login page: the one the build embedded.
struct EmbeddedLanding;

impl LandingPage for EmbeddedLanding {
    fn login_page(&self) -> axum::body::Bytes {
        embed::Assets::embedded().get("/login.html").map(|asset| asset.body.clone()).unwrap_or_default()
    }
}

/// The routes of the server: one line for each route module. A route plan adds its line here and nothing else.
pub fn router_parts(services: &Services) -> Vec<Part> {
    let auth = Arc::new(AuthServices {
        app: services.app.clone(),
        signin: services.signin.clone(),
        passkeys: services.passkeys.clone(),
        oidc: services.oidc.clone(),
        gate: services.gate.clone(),
        page: Arc::new(EmbeddedLanding),
    });
    let enrol = Enrol::new(EnrolServices {
        auth: auth.clone(),
        accounts: services.accounts.clone(),
        argon: services.argon.clone(),
        sealer: services.sealer.clone(),
    });
    let me = Arc::new(MeServices::new(
        services.app.clone(),
        services.signin.clone(),
        services.passkeys.clone(),
        services.argon.clone(),
        services.sealer.clone(),
        services.gate.clone(),
    ));
    let admin = Arc::new(AdminServices::new(
        services.accounts.clone(),
        services.signin.clone(),
        services.app.origin.clone(),
        services.clock.clone(),
        services.gate.clone(),
    ));
    let mut parts = Vec::new();
    parts.extend(embed::parts());
    parts.extend(routes_auth::parts(auth.clone()));
    parts.extend(routes_unlock::parts(auth));
    parts.extend(routes_enrol::parts(&enrol));
    parts.extend(routes_me::parts(me));
    parts.extend(routes_admin::parts(admin));
    parts.extend(routes_device::parts());
    parts.extend(ws::parts(services.ws.clone()));
    parts.extend(ws_device::parts(services.ws.clone()));
    parts.extend(routes_files::parts(services));
    parts
}

/// A running server.
pub struct Running {
    /// The address the listener is bound to.
    pub addr: SocketAddr,
    /// The stop signal: triggering it starts the ordered shutdown.
    pub shutdown: Shutdown,
    /// The shared handles of the services.
    pub services: Services,
    /// The slots of the accept loop.
    pub admission: Admission,
    /// The readiness the health socket reports.
    pub readiness: Arc<Readiness>,
    /// The path of the health socket.
    pub health_path: std::path::PathBuf,
    resolver: Option<Arc<FileResolver>>,
    done: JoinHandle<Result<(), Refusal>>,
}

impl Running {
    /// Starts the ordered shutdown. A second call changes nothing.
    pub fn trigger(&self) {
        self.shutdown.trigger();
    }

    /// Reads the certificate files again (the SIGHUP of the operator). `Ok(false)` when this server has no certificate files.
    pub fn reload_certificates(&self) -> Result<bool, Refusal> {
        match &self.resolver {
            Some(resolver) => resolver.reload().map(|_| true).map_err(|e| Refusal::new(e.sentence())),
            None => Ok(false),
        }
    }

    /// Waits until the shutdown is done. Returns after [`Running::trigger`] (or a signal) and the end of everything.
    pub async fn join(self) -> Result<(), Refusal> {
        self.done.await.unwrap_or_else(|_| Err(Refusal::new("the shutdown task ended unexpectedly")))
    }
}

fn sentence(error: impl std::fmt::Display) -> Refusal {
    Refusal::new(error.to_string())
}

/// The launcher the configuration names: the spawn helper by default, and the server's own process only in a build with the
/// feature and a configuration that asks for it.
async fn launcher(prepared: &Prepared) -> Result<Arc<dyn WorkerLauncher>, Refusal> {
    #[cfg(feature = "in-process-workers")]
    if prepared.workers.mode == crate::config::WorkersMode::InProcess {
        let shell = holzwave_host::state::ShellSpec::System;
        return Ok(Arc::new(super::launcher::InProcessLauncher::new(prepared.data_dir.clone(), shell)));
    }
    helper_launcher(prepared).await
}

#[cfg(target_os = "linux")]
async fn helper_launcher(prepared: &Prepared) -> Result<Arc<dyn WorkerLauncher>, Refusal> {
    let workers = &prepared.workers;
    let settings = super::helper_client::HelperSettings {
        program: workers.helper_path.clone(),
        worker_path: workers.worker_path.clone(),
        home_base: prepared.data_dir.join("homes"),
        uid_range: Some((workers.uid_min, workers.uid_max)),
        nproc: Some(workers.nproc),
        nofile: Some(workers.nofile),
        ..super::helper_client::HelperSettings::default()
    };
    let client = super::helper_client::HelperClient::start(&settings).await.map_err(|e| Refusal::new(e.sentence()))?;
    Ok(Arc::new(super::launcher::HelperLauncher::new(client)))
}

#[cfg(not(target_os = "linux"))]
async fn helper_launcher(_prepared: &Prepared) -> Result<Arc<dyn WorkerLauncher>, Refusal> {
    Err(Refusal::new("the spawn helper needs Linux; this build can run workers only in its own process"))
}

fn supervisor_settings(prepared: &Prepared) -> SupervisorSettings {
    SupervisorSettings {
        max_panes: prepared.workers.max_panes,
        max_scrollback_lines: prepared.workers.max_scrollback_lines,
        max_image_mib: prepared.workers.max_image_mib,
        sessions: prepared.remote.session_limits(),
        sockets_per_person: usize::try_from(prepared.limits.sockets_per_person).unwrap_or(usize::MAX),
        socket_queue_bytes: usize::try_from(prepared.limits.ws_queue_bytes).unwrap_or(usize::MAX),
        idle_timeout: prepared.workers.idle_timeout,
        ..SupervisorSettings::default()
    }
}

/// What the certificates are, by mode.
struct Certificates {
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    mode: Mode,
    proxies: TrustedProxies,
    resolver: Option<Arc<FileResolver>>,
    reloader: Option<tls::Reloader>,
    acme: Option<Arc<AcmeManager>>,
}

fn certificates(prepared: &Prepared, clock: &Arc<dyn Clock>) -> Result<Certificates, Refusal> {
    match &prepared.tls {
        TlsSetup::Files { cert_file, key_file } => {
            let resolver = Arc::new(FileResolver::new(cert_file, key_file).map_err(|e| Refusal::new(e.sentence()))?);
            let config = tls::server_config(resolver.clone(), &tls::ALPN).map_err(|e| Refusal::new(e.sentence()))?;
            let reloader = tls::spawn_reloader(resolver.clone(), tls::RELOAD_INTERVAL);
            Ok(Certificates {
                acceptor: Some(tls::acceptor(Arc::new(config))),
                mode: Mode::Files,
                proxies: TrustedProxies::none(),
                resolver: Some(resolver),
                reloader: Some(reloader),
                acme: None,
            })
        }
        TlsSetup::Acme { directory_url, contact, domains, ca_file } => {
            let settings = AcmeSettings::new(directory_url, contact, domains, ca_file.clone(), true, Some(prepared.origin.host()))?;
            let resolver = Arc::new(AcmeResolver::new());
            let manager = Arc::new(
                AcmeManager::new(settings, resolver.clone(), &prepared.key, &prepared.data_dir, clock.clone())
                    .map_err(|e| Refusal::new(e.sentence()))?,
            );
            let config = tls::server_config(resolver, &acme::ALPN).map_err(|e| Refusal::new(e.sentence()))?;
            Ok(Certificates {
                acceptor: Some(tls::acceptor(Arc::new(config))),
                mode: Mode::Acme,
                proxies: TrustedProxies::none(),
                resolver: None,
                reloader: None,
                acme: Some(manager),
            })
        }
        TlsSetup::Proxy { trusted_proxies } => Ok(Certificates {
            acceptor: None,
            mode: Mode::Proxy,
            proxies: TrustedProxies::new(trusted_proxies).map_err(sentence)?,
            resolver: None,
            reloader: None,
            acme: None,
        }),
    }
}

/// The single sign-on service over the given client, or over the product's when there is none.
fn single_sign_on(
    auth: &Auth,
    public_url: &str,
    accounts: &Arc<Accounts>,
    sessions: &Arc<SessionManager>,
    audit: &Arc<Audit>,
    clock: &Arc<dyn Clock>,
    client: Option<OidcClient>,
) -> Result<Option<Arc<dyn OidcFlow>>, Refusal> {
    if auth.oidc.is_empty() {
        return Ok(None);
    }
    let http = match client {
        Some(client) => client,
        None => {
            // The product's client speaks https only; a provider at another address is refused at the start, not at the
            // first sign-in.
            if let Some(provider) = auth.oidc.iter().find(|p| !p.issuer.to_ascii_lowercase().starts_with("https://")) {
                return Err(Refusal::new(format!(
                    "the single sign-on provider {} has an issuer that is not an https address; identity providers are reached over https only",
                    provider.id
                )));
            }
            OidcClient::new(HttpsClient::new(true).map_err(|_| Refusal::new("the client for identity providers cannot be built"))?)
        }
    };
    let service = OidcService::new(
        &auth.oidc,
        public_url,
        accounts.clone(),
        sessions.clone(),
        audit.clone(),
        clock.clone(),
        http,
    )?;
    Ok(Some(Arc::new(service)))
}

/// Builds the whole server from a checked configuration and starts it.
pub async fn assemble(prepared: Prepared, overrides: Overrides) -> Result<Running, Refusal> {
    transport::select(prepared.transport.compression).map_err(Refusal::new)?;
    embed::verify().map_err(sentence)?;

    let Overrides { listen, token_writer, grace, clock, oidc_client } = overrides;
    let clock: Arc<dyn Clock> = clock.unwrap_or_else(|| Arc::new(SystemClock));

    // The store, the audit log and the sessions.
    let store = Arc::new(Store::open(&prepared.data_dir).map_err(sentence)?);
    let audit = Arc::new(Audit::open(&prepared.data_dir, clock.clone()).map_err(sentence)?);
    let sessions = Arc::new(SessionManager::new(store.clone(), clock.clone(), prepared.sessions).with_audit(audit.clone()));

    // The workers.
    let launcher = launcher(&prepared).await?;
    let supervisor = Supervisor::new(launcher, supervisor_settings(&prepared), clock.clone());
    let sweeper = supervisor.start_sweeper();

    // The services of the sign-in.
    let accounts = Arc::new(
        Accounts::new(store.clone(), clock.clone(), audit.clone(), prepared.workers.uid_min..=prepared.workers.uid_max)
            .with_observer(Arc::new(supervisor.clone())),
    );
    let argon = ArgonGate::new(prepared.limits.argon2_concurrency, Kdf::default());
    let sealer = Arc::new(Sealer::new(&prepared.key));
    let signin = Arc::new(
        SignIn::new(store.clone(), clock.clone(), audit.clone(), sessions.clone(), argon.clone(), sealer.clone())
            .with_limits(prepared.limits),
    );
    let passkeys = Arc::new(
        Passkeys::new(prepared.origin.as_str(), "holzWave", store.clone(), sessions.clone(), audit.clone(), clock.clone())
            .map_err(sentence)?,
    );
    let oidc = single_sign_on(&prepared.auth, prepared.origin.as_str(), &accounts, &sessions, &audit, &clock, oidc_client)?;
    let gate = Arc::new(SupervisorGate::new(GateServices {
        supervisor: supervisor.clone(),
        store: store.clone(),
        sessions: sessions.clone(),
        audit: audit.clone(),
        clock: clock.clone(),
        argon: argon.clone(),
    }));

    // The application state and the routes.
    let certs = certificates(&prepared, &clock)?;
    let app = App::new(
        AppInit {
            origin: PublicOrigin::parse(prepared.origin.as_str())?,
            auth: prepared.auth.clone(),
            sessions: prepared.sessions,
            limits: prepared.limits,
            mode: certs.mode,
            proxies: certs.proxies.clone(),
            store: store.clone(),
            session_manager: sessions.clone(),
            audit: audit.clone(),
            clock: clock.clone(),
            view: gate.clone(),
            devices: DeviceSettings { ceiling: prepared.device_access.ceiling(), idle: prepared.devices.idle },
        },
        &prepared.key,
    );
    let ws = WsState::new(app.clone(), supervisor.clone());
    let services = Services {
        supervisor: supervisor.clone(),
        app: app.clone(),
        sessions,
        store: store.clone(),
        accounts: accounts.clone(),
        audit,
        clock: clock.clone(),
        gate,
        signin,
        passkeys,
        oidc,
        argon,
        sealer,
        ws,
        upload: prepared.upload.clone(),
    };
    let router = router::build(app.clone(), router_parts(&services));

    // The listener.
    let admission = Admission::from_limits(certs.mode, certs.proxies.clone(), &prepared.limits).map_err(sentence)?;
    let mut timing = Timing::from_limits(&prepared.limits);
    if let Some(grace) = grace {
        timing.grace = grace;
    }
    let edge = Edge::new(router, certs.acceptor.clone(), admission.clone(), certs.mode).map_err(sentence)?.with_timing(timing);
    let bind = listen.unwrap_or(prepared.listen);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|e| Refusal::new(format!("the address {bind} cannot be listened on ({e})")))?;
    let addr = listener.local_addr().map_err(sentence)?;
    let shutdown = Shutdown::new();
    let accepting = tokio::spawn(crate::http::app::serve(listener, edge, shutdown.clone()));

    // The health socket, with one readiness check for each subsystem.
    let readiness = Arc::new(Readiness::new());
    {
        let store = store.clone();
        readiness.register("store", move || store.read(|tx| tx.account_count()).is_ok());
        let supervisor = supervisor.clone();
        readiness.register("supervisor", move || supervisor.stats().helper_alive);
        let (resolver, manager) = (certs.resolver.clone(), certs.acme.clone());
        readiness.register("certificate", move || match (&resolver, &manager) {
            (Some(resolver), _) => !resolver.fingerprint().is_empty(),
            (None, Some(manager)) => manager.ready(),
            (None, None) => true,
        });
        let stop = shutdown.clone();
        readiness.register("running", move || !stop.is_triggered());
    }
    let health_stop = Shutdown::new();
    let health_server = health::serve(&prepared.runtime_dir, readiness.clone(), health_stop.clone()).await.map_err(sentence)?;
    let health_path = health_server.path().to_path_buf();
    let acme_task = certs.acme.clone().map(|manager| tokio::spawn(manager.run()));

    // The first start prints the token of the first administrator, once.
    announce_bootstrap(&services.accounts, prepared.origin.as_str(), token_writer);

    log::event(Level::Info, "serve.listening", &[("addr", &addr.to_string())]);

    let done = tokio::spawn(finish(Ending {
        shutdown: shutdown.clone(),
        supervisor,
        ws: services.ws.clone(),
        accepting,
        sweeper,
        acme_task,
        reloader: certs.reloader,
        health_server,
        health_stop,
        wait: grace.unwrap_or(Timing::DEFAULT.grace),
    }));
    Ok(Running { addr, shutdown, services, admission, readiness, health_path, resolver: certs.resolver, done })
}

fn announce_bootstrap(accounts: &Accounts, public_url: &str, writer: Option<Box<dyn Write + Send>>) {
    match accounts.bootstrap_for_start() {
        Ok(Some(issued)) => {
            let line = accounts::announcement(public_url, &issued);
            let written = match writer {
                Some(mut writer) => writeln!(writer, "{line}").and_then(|()| writer.flush()),
                None => {
                    let mut out = std::io::stdout().lock();
                    writeln!(out, "{line}").and_then(|()| out.flush())
                }
            };
            if written.is_err() {
                log::event(Level::Warn, "serve.bootstrap_not_written", &[]);
            }
        }
        Ok(None) => {}
        Err(_) => log::event(Level::Warn, "serve.bootstrap_not_issued", &[]),
    }
}

/// What the shutdown task owns.
struct Ending {
    shutdown: Shutdown,
    supervisor: Supervisor,
    ws: Arc<WsState>,
    accepting: JoinHandle<()>,
    sweeper: JoinHandle<()>,
    acme_task: Option<JoinHandle<()>>,
    reloader: Option<tls::Reloader>,
    health_server: health::HealthServer,
    health_stop: Shutdown,
    wait: Duration,
}

/// The ordered shutdown, from the stop signal to the removal of the health socket.
async fn finish(ending: Ending) -> Result<(), Refusal> {
    let Ending { shutdown, supervisor, ws, accepting, sweeper, acme_task, reloader, health_server, health_stop, wait } = ending;
    shutdown.wait().await;
    // The accept loop has stopped taking connections; every socket is closed 1001 and every worker asked to end.
    supervisor.shutdown().await;
    // The sockets' tasks are waited for, and the accept loop gets its grace time to finish what it was doing.
    let _ = ws.tracker.wait_empty(wait).await;
    let _ = tokio::time::timeout(wait + Duration::from_secs(1), accepting).await;
    sweeper.abort();
    if let Some(task) = acme_task {
        task.abort();
    }
    drop(reloader);
    health_stop.trigger();
    health_server.finished().await;
    Ok(())
}

/// The binary's `serve`: assembles with the default overrides, reloads the certificates on SIGHUP, and stops in order on
/// SIGTERM or SIGINT.
pub async fn run(prepared: Prepared) -> Result<(), Refusal> {
    use tokio::signal::unix::{SignalKind, signal};
    let running = assemble(prepared, run_overrides()).await?;
    let signals = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    );
    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = signals else {
        running.trigger();
        let _ = running.join().await;
        return Err(Refusal::new("the signal handlers cannot be installed"));
    };
    loop {
        tokio::select! {
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = hup.recv() => match running.reload_certificates() {
                Ok(_) => log::event(Level::Info, "serve.certificates_reloaded", &[]),
                Err(_) => log::event(Level::Warn, "serve.certificates_not_reloaded", &[]),
            },
        }
    }
    running.trigger();
    running.join().await
}

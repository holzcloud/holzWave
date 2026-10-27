//! The routes of sign-in, sign-out, passkey and single sign-on ceremonies.

use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Bytes;

use crate::auth::oidc::{OidcCallback, OidcError, OidcGrant, OidcService, OidcStart};
use crate::auth::oidc::Http;
use crate::auth::passkey::Passkeys;
use crate::auth::signin::SignIn;
use crate::http::app::App;
use crate::http::router::Part;

use super::routes_unlock::{GateFuture, UnlockGate};

/// The single sign-on service as the routes use it, without its HTTP client type.
pub trait OidcFlow: Send + Sync {
    /// Starts a sign-in at the provider `id`.
    fn start<'a>(&'a self, id: &'a str, addr: IpAddr) -> GateFuture<'a, Result<OidcStart, OidcError>>;

    /// Finishes a sign-in from the callback of the provider.
    fn finish<'a>(
        &'a self,
        transaction: &'a str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &'a str,
    ) -> GateFuture<'a, Result<OidcGrant, OidcError>>;
}

impl<H: Http> OidcFlow for OidcService<H> {
    fn start<'a>(&'a self, id: &'a str, addr: IpAddr) -> GateFuture<'a, Result<OidcStart, OidcError>> {
        Box::pin(OidcService::start(self, id, addr))
    }

    fn finish<'a>(
        &'a self,
        transaction: &'a str,
        callback: OidcCallback,
        addr: IpAddr,
        device: &'a str,
    ) -> GateFuture<'a, Result<OidcGrant, OidcError>> {
        Box::pin(OidcService::finish(self, transaction, callback, addr, device))
    }
}

/// The login page, as the callback of a successful single sign-on answers it.
pub trait LandingPage: Send + Sync {
    /// The bytes of the login page.
    fn login_page(&self) -> Bytes;
}

/// The services the sign-in routes call.
pub struct AuthServices {
    /// The application state; its configuration says which methods are on.
    pub app: Arc<App>,
    /// The password and code sign-in.
    pub signin: Arc<SignIn>,
    /// The passkey ceremonies.
    pub passkeys: Arc<Passkeys>,
    /// Single sign-on; `None` when no provider is configured.
    pub oidc: Option<Arc<dyn OidcFlow>>,
    /// The seam to the secret store.
    pub gate: Arc<dyn UnlockGate>,
    /// The login page.
    pub page: Arc<dyn LandingPage>,
}

/// The device summary for the text of a `User-Agent` header.
pub fn device_of(_user_agent: &str) -> String {
    String::new()
}

/// The sign-in, passkey, single sign-on, logout and step-up routes.
pub fn parts(_services: Arc<AuthServices>) -> Vec<Part> {
    Vec::new()
}

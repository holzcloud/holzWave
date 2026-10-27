//! The route that unlocks the secret store of a person after a restart, and the gate the sign-in routes tell about a new
//! session.
//!
//! The secret store lives on the worker side. The routes never decide whether it opens: after a sign-in they hand the key
//! material of the method that was used to an [`UnlockGate`] and go on whatever the gate says, and the explicit unlock hands
//! the typed secret to the same gate and reports its verdict. The implementation of the gate, not the route, writes the
//! `secrets.*` audit lines.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::auth::passkey::PrfMaterial;
use crate::auth::signin::UnlockMaterial;
use crate::http::app::StoreView;
use crate::http::router::Part;
use crate::store::AccountId;

use super::routes_auth::AuthServices;

/// A boxed future, so that the gate can be held as a trait object.
pub type GateFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The key material a sign-in produced, by the method that produced it. Not `Clone`; every variant wipes itself.
pub enum Material {
    /// The key-encryption key of the password.
    Password(UnlockMaterial),
    /// The credential, the PRF output and the salt of a passkey.
    Passkey(PrfMaterial),
    /// Single sign-on, or a passkey that sent no usable PRF output: nothing that could open the store.
    None,
}

impl Material {
    /// A word for the kind, for tests and diagnostics: `password`, `passkey` or `none`.
    pub fn kind(&self) -> &'static str {
        match self {
            Material::Password(_) => "password",
            Material::Passkey(_) => "passkey",
            Material::None => "none",
        }
    }
}

impl std::fmt::Debug for Material {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Material(..)")
    }
}

/// The secret a person typed to unlock the store. Borrowed from the request, printed as a marker.
pub enum UnlockSecret<'a> {
    /// The passphrase of the store.
    Passphrase(&'a str),
    /// A recovery code.
    RecoveryCode(&'a str),
}

impl std::fmt::Debug for UnlockSecret<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnlockSecret(..)")
    }
}

/// Why the gate did not do what it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// The secret does not open the store.
    Refused,
    /// The worker side could not be reached or failed.
    Unavailable,
}

/// The seam between the sign-in routes and the secret store of the worker side. It is also the [`StoreView`] the application
/// state reads, so one object serves both.
pub trait UnlockGate: StoreView {
    /// A person signed in; `material` is what the method produced. A failure changes nothing for the sign-in: the store stays
    /// locked and the page shows the unlock dialog.
    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>>;

    /// An explicit unlock with a typed secret.
    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>>;
}

/// The routes of the unlock: `POST /auth/unlock`.
pub fn parts(_services: Arc<AuthServices>) -> Vec<Part> {
    Vec::new()
}

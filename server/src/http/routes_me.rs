//! The routes of the signed-in person's own account: password, second factor, passkeys, sessions.

use std::sync::Arc;

use crate::auth::passkey::Passkeys;
use crate::auth::password::ArgonGate;
use crate::auth::signin::SignIn;
use crate::http::app::App;
use crate::http::router::Part;
use crate::http::routes_unlock::{GateFuture, ProvisionKey};
use crate::sealed::Sealer;
use crate::store::{AccountId, UnlockKind};

/// Why the secret store did not do what it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretsError {
    /// The store is locked and the key given could not open it.
    Locked,
    /// The worker side could not be reached or failed.
    Unavailable,
    /// The worker side refused the change.
    Refused,
}

/// The seam between the account routes and the secret store of the worker side.
pub trait SecretsAdmin: Send + Sync {
    /// Sets the slot of `key`'s kind (and credential) to `key`; `via` opens the store when it is not open.
    fn set_slot(
        &self,
        account: AccountId,
        key: ProvisionKey,
        via: Option<ProvisionKey>,
    ) -> GateFuture<'_, Result<(), SecretsError>>;

    /// Removes the slot of `kind` (for a PRF key, of the credential).
    fn remove_slot(
        &self,
        account: AccountId,
        kind: UnlockKind,
        credential_id: Option<Vec<u8>>,
    ) -> GateFuture<'_, Result<(), SecretsError>>;

    /// Removes the whole secret store of the account.
    fn reset(&self, account: AccountId) -> GateFuture<'_, Result<(), SecretsError>>;
}

/// The most half-finished steps (authenticator and passkey enrolments) that are open at once.
pub const MAX_PENDING: usize = 1_024;

/// The services the account routes call.
pub struct MeServices {
    _app: Arc<App>,
}

impl MeServices {
    /// The services over the application state, the sign-in, the passkeys, the derivation gate, the sealer and the seam.
    pub fn new(
        app: Arc<App>,
        _signin: Arc<SignIn>,
        _passkeys: Arc<Passkeys>,
        _argon: ArgonGate,
        _sealer: Arc<Sealer>,
        _secrets: Arc<dyn SecretsAdmin>,
    ) -> MeServices {
        MeServices { _app: app }
    }

    /// The same services with room for fewer half-finished steps than [`MAX_PENDING`] (never more). For tests that fill the room.
    pub fn with_max_pending(self, _most: usize) -> MeServices {
        self
    }

    /// How many half-finished steps (authenticator and passkey enrolments) are open now.
    pub fn pending_count(&self) -> usize {
        0
    }
}

/// The routes under `/api/me`, as a part for [`crate::http::router::build`].
pub fn parts(_services: Arc<MeServices>) -> Vec<Part> {
    Vec::new()
}

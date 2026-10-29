//! The routes that make the first administrator and every invited person. Not implemented yet.

use std::sync::Arc;

use crate::accounts::Accounts;
use crate::auth::password::ArgonGate;
use crate::http::router::Part;
use crate::http::routes_auth::AuthServices;
use crate::sealed::Sealer;

/// The services the enrolment routes call.
pub struct EnrolServices {
    /// The sign-in services: the application state, the passkeys and the unlock gate.
    pub auth: Arc<AuthServices>,
    /// The accounts service, which redeems the tokens.
    pub accounts: Arc<Accounts>,
    /// The door to Argon2.
    pub argon: ArgonGate,
    /// Seals the authenticator secret.
    pub sealer: Arc<Sealer>,
}

/// The enrolment routes and their half-finished states.
pub struct Enrol;

impl Enrol {
    /// The enrolment over `services`.
    pub fn new(_services: EnrolServices) -> Arc<Enrol> {
        Arc::new(Enrol)
    }

    /// How many half-finished enrolments exist now.
    pub fn pending_count(&self) -> usize {
        0
    }
}

/// The routes, as parts for [`crate::http::router::build`].
pub fn parts(_enrol: &Arc<Enrol>) -> Vec<Part> {
    Vec::new()
}

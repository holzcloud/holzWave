//! The routes of the administrator: people, invitations, roles and the audit log.

use std::sync::Arc;

use crate::accounts::Accounts;
use crate::auth::signin::SignIn;
use crate::clock::Clock;
use crate::http::app::PublicOrigin;
use crate::http::router::Part;

/// The services the administration routes call.
pub struct AdminServices {
    _accounts: Arc<Accounts>,
}

impl AdminServices {
    /// The services over the accounts, the sign-in (for its locks), the public origin links are built on and the clock.
    pub fn new(accounts: Arc<Accounts>, _signin: Arc<SignIn>, _origin: PublicOrigin, _clock: Arc<dyn Clock>) -> AdminServices {
        AdminServices { _accounts: accounts }
    }
}

/// The routes under `/api/admin`, as a part for [`crate::http::router::build`].
pub fn parts(_services: Arc<AdminServices>) -> Vec<Part> {
    Vec::new()
}

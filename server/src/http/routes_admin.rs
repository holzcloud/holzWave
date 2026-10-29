//! The routes of the administrator under `/api/admin`: the list of people, invitations and, further down, roles, the state of
//! an account, the reset of a person's secrets and the lifting of a sign-in lock.
//!
//! - **One mount, three gates.** Every route is in one [`Access::SessionFresh`] part, so the router has already answered 401
//!   for no session, 403 with `step_up` for a change after an old sign-in, and checked the anti-forgery token before a handler
//!   runs. Every handler then takes the [`Admin`] extractor, so a member is 403 `forbidden` without `step_up` (a recent sign-in
//!   would not help) before the body is read. The accounts service checks the role again inside its own transaction, so an
//!   administrator who was demoted a moment ago changes nothing.
//! - **The actor is the session.** No body and no path names the person who acts; the actor of every call is the account of
//!   the session.
//! - **Nothing of a person's data.** An answer holds names, roles, states and times, never a secret, a scrollback or a command.
//! - **Strict bodies.** A body is a JSON object with the members of its route and no others; a member that is wrong names
//!   itself in the answer, and nothing is changed.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::accounts::{AccountError, Accounts, INVITE_DAYS, MAX_INVITE_DAYS};
use crate::auth::signin::SignIn;
use crate::clock::{Clock, from_unix, rfc3339};
use crate::http::app::PublicOrigin;
use crate::http::router::{Access, Admin, ApiError, ApiJson, Part};
use crate::store::{AccountState, MAX_DISPLAY_NAME, Role, valid_username};

use super::routes_auth::{Object, Refusal, answer, blocking};

/// The services the administration routes call.
pub struct AdminServices {
    accounts: Arc<Accounts>,
    signin: Arc<SignIn>,
    origin: PublicOrigin,
    clock: Arc<dyn Clock>,
}

impl AdminServices {
    /// The services over the accounts, the sign-in (for the locks of the password path), the public origin that links are
    /// built on and the clock.
    pub fn new(accounts: Arc<Accounts>, signin: Arc<SignIn>, origin: PublicOrigin, clock: Arc<dyn Clock>) -> AdminServices {
        AdminServices { accounts, signin, origin, clock }
    }
}

type Shared = Arc<AdminServices>;

/// The answer to a refusal of the accounts service. A change that would leave no administrator is a conflict on `role`; the
/// other refusals are named by what the person can act on, and nothing from the service's own sentence is passed on.
fn refusal(error: &AccountError) -> Refusal {
    Refusal(match error {
        AccountError::NotAllowed => ApiError::Forbidden { step_up: false },
        AccountError::Conflict => ApiError::Conflict { field: Some("username") },
        AccountError::TooManyInvitations => ApiError::Conflict { field: Some("invitations") },
        AccountError::LastAdmin => ApiError::Conflict { field: Some("role") },
        AccountError::NotFound => ApiError::NotFound,
        AccountError::UidRangeFull => ApiError::Conflict { field: None },
        AccountError::Invalid(_) => ApiError::BadRequest { field: None },
        _ => ApiError::Busy,
    })
}

fn bad(field: &'static str) -> Refusal {
    Refusal(ApiError::BadRequest { field: Some(field) })
}

fn time_text(unix: i64) -> String {
    rfc3339(from_unix(unix))
}

// ---------------------------------------------------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------------------------------------------------

/// `GET /api/admin/accounts`: the accounts that are not deleted, in the order they were made, with the number of active
/// administrators. An account that is active but locked out of the password path is `locked_out`.
async fn list_route(State(services): State<Shared>, Admin(who): Admin) -> Result<Response, Refusal> {
    let actor = who.account.id;
    let worker = services.clone();
    let listed = blocking(move || {
        let listing = worker.accounts.list(actor).map_err(|e| refusal(&e))?;
        let now = worker.clock.unix();
        let mut rows = Vec::with_capacity(listing.accounts.len());
        for listed in &listing.accounts {
            let account = &listed.account;
            let state = match account.state {
                AccountState::Active => {
                    if worker.signin.is_locked(&account.username, now).map_err(|_| Refusal(ApiError::Busy))? {
                        "locked_out"
                    } else {
                        "active"
                    }
                }
                AccountState::Disabled => "disabled",
                AccountState::Deleted => continue,
            };
            rows.push(json!({
                "id": account.id.get(),
                "username": account.username,
                "display_name": account.display_name,
                "role": account.role.as_str(),
                "state": state,
                "last_signin": listed.last_signin.map(time_text),
                "created_at": time_text(account.created_at),
            }));
        }
        Ok::<_, Refusal>((listing.admins, rows))
    })
    .await??;
    Ok(answer(StatusCode::OK, &json!({ "admins": listed.0, "accounts": listed.1 }), Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------------------------------------------------

/// The members of an invitation body. Each is any JSON value here, so that a member of the wrong type names itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitationBody {
    #[serde(default)]
    role: Option<Value>,
    #[serde(default)]
    days: Option<Value>,
    #[serde(default)]
    username: Option<Value>,
    #[serde(default)]
    display_name: Option<Value>,
}

/// An invitation as the routes understand it.
struct Invitation {
    role: Role,
    days: i64,
    /// The username and the display name of an account that is made at once.
    account: Option<(String, String)>,
}

/// The role of a body: `admin` or `member`, nothing else.
fn role_of(value: Option<Value>, field: &'static str) -> Result<Role, Refusal> {
    match value {
        Some(Value::String(text)) => Role::parse(&text).ok_or_else(|| bad(field)),
        _ => Err(bad(field)),
    }
}

/// Whether `name` can be shown: 1 to [`MAX_DISPLAY_NAME`] characters, none a control character.
fn valid_shown_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_DISPLAY_NAME).contains(&count) && !name.chars().any(char::is_control)
}

fn invitation_of(body: InvitationBody) -> Result<Invitation, Refusal> {
    let role = role_of(body.role, "role")?;
    let days = match body.days {
        None => INVITE_DAYS,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(days) if (1..=MAX_INVITE_DAYS).contains(&days) => days,
            _ => return Err(bad("days")),
        },
        Some(_) => return Err(bad("days")),
    };
    let account = match (body.username, body.display_name) {
        (None, None) => None,
        (None, Some(_)) => return Err(bad("display_name")),
        (Some(Value::String(username)), shown) => {
            if !valid_username(&username) {
                return Err(bad("username"));
            }
            let shown = match shown {
                None => username.clone(),
                Some(Value::String(text)) if valid_shown_name(&text) => text,
                Some(_) => return Err(bad("display_name")),
            };
            Some((username, shown))
        }
        (Some(_), _) => return Err(bad("username")),
    };
    Ok(Invitation { role, days, account })
}

/// `POST /api/admin/invitations`: an invitation for a new person, or, with `username`, an account that has no sign-in method
/// yet and the invitation that enrols it. The link is shown in this answer and nowhere else; the service keeps only a hash.
async fn invitations_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    ApiJson(Object(body)): ApiJson<Object<InvitationBody>>,
) -> Result<Response, Refusal> {
    let invitation = invitation_of(body)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    let worker = services.clone();
    let issued = blocking(move || match invitation.account {
        Some((username, shown)) => worker
            .accounts
            .create_account_days(actor, addr, &username, &shown, invitation.role, invitation.days)
            .map(|(_, issued)| issued),
        None => worker.accounts.invite_create_days(actor, addr, None, invitation.role, invitation.days),
    })
    .await?
    .map_err(|e| refusal(&e))?;
    let link = format!("{}/#invite={}", services.origin.text(), issued.token.expose());
    Ok(answer(StatusCode::OK, &json!({ "link": link, "expires_at": time_text(issued.expires_at) }), Vec::new()))
}

// ---------------------------------------------------------------------------------------------------------------------
// The part
// ---------------------------------------------------------------------------------------------------------------------

/// The routes under `/api/admin`, as one part for [`crate::http::router::build`].
pub fn parts(services: Arc<AdminServices>) -> Vec<Part> {
    let router = Router::new()
        .route("/api/admin/accounts", get(list_route))
        .route("/api/admin/invitations", post(invitations_route))
        .with_state(services);
    vec![Part { access: Access::SessionFresh, router }]
}

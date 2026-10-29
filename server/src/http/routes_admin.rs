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
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::accounts::{AccountError, Accounts, INVITE_DAYS, MAX_INVITE_DAYS};
use crate::auth::signin::{SignIn, SignInError};
use crate::clock::{Clock, from_unix, rfc3339};
use crate::http::app::PublicOrigin;
use crate::http::router::{Access, Admin, ApiError, ApiJson, Part};
use crate::store::{AccountId, AccountState, MAX_DISPLAY_NAME, Role, valid_username};

use super::routes_auth::{Object, Refusal, answer, blocking};
use super::routes_me::{SecretsAdmin, present, seam, seam_refusal};

/// The services the administration routes call.
pub struct AdminServices {
    accounts: Arc<Accounts>,
    signin: Arc<SignIn>,
    origin: PublicOrigin,
    clock: Arc<dyn Clock>,
    secrets: Arc<dyn SecretsAdmin>,
}

impl AdminServices {
    /// The services over the accounts, the sign-in (for the locks of the password path), the public origin that links are
    /// built on, the clock and the seam to the secret store (for the reset of a person's secrets).
    pub fn new(
        accounts: Arc<Accounts>,
        signin: Arc<SignIn>,
        origin: PublicOrigin,
        clock: Arc<dyn Clock>,
        secrets: Arc<dyn SecretsAdmin>,
    ) -> AdminServices {
        AdminServices { accounts, signin, origin, clock, secrets }
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
    #[serde(default, deserialize_with = "present")]
    role: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    days: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    username: Option<Value>,
    #[serde(default, deserialize_with = "present")]
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
// Changes to one account
// ---------------------------------------------------------------------------------------------------------------------

/// The most characters of an account id in a path. A number of that many digits that does not fit is no account either.
const MAX_ID_CHARS: usize = 20;

/// The account a path names: plain decimal digits, at most [`MAX_ID_CHARS`] of them, and a number that fits. Anything else
/// (a sign, a point, an exponent, a space, an empty text, a long one) names no account, and the answer is the same as for an
/// account that is not there.
fn target_of(path: Result<Path<String>, PathRejection>) -> Result<AccountId, Refusal> {
    let not_found = || Refusal(ApiError::NotFound);
    let Path(text) = path.map_err(|_| not_found())?;
    if text.is_empty() || text.len() > MAX_ID_CHARS || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(not_found());
    }
    text.parse::<i64>().map(AccountId::from_raw).map_err(|_| not_found())
}

/// A body with no member: `{}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

/// The one member of a role change.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleBody {
    #[serde(default, deserialize_with = "present")]
    role: Option<Value>,
}

/// Looks the target up before anything is asked of it: an account that is not there, or is deleted, is 404 and writes nothing.
async fn require_target(services: &Shared, actor: AccountId, target: AccountId) -> Result<(), Refusal> {
    let worker = services.clone();
    blocking(move || worker.accounts.check_target(actor, target)).await?.map_err(|e| refusal(&e))
}

fn no_content() -> Response {
    super::routes_me::no_content(Vec::new())
}

/// `POST /api/admin/accounts/{id}/role`: makes the account an administrator or a member. The last active administrator cannot
/// be made a member (409 on `role`).
async fn role_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
    ApiJson(Object(body)): ApiJson<Object<RoleBody>>,
) -> Result<Response, Refusal> {
    let role = role_of(body.role, "role")?;
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    let worker = services.clone();
    blocking(move || worker.accounts.set_role(actor, addr, target, role)).await?.map_err(|e| refusal(&e))?;
    Ok(no_content())
}

/// `POST /api/admin/accounts/{id}/disable`: the account cannot sign in and every session of it ends.
async fn disable_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    let worker = services.clone();
    blocking(move || worker.accounts.disable(actor, addr, target)).await?.map_err(|e| refusal(&e))?;
    Ok(no_content())
}

/// `POST /api/admin/accounts/{id}/enable`: a disabled account can sign in again.
async fn enable_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    let worker = services.clone();
    blocking(move || worker.accounts.enable(actor, addr, target)).await?.map_err(|e| refusal(&e))?;
    Ok(no_content())
}

/// `DELETE /api/admin/accounts/{id}`: the credentials, identities and sessions of the account go; its row, user id and name
/// stay reserved.
async fn delete_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, Refusal> {
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    let worker = services.clone();
    blocking(move || worker.accounts.delete(actor, addr, target)).await?.map_err(|e| refusal(&e))?;
    Ok(no_content())
}

/// `POST /api/admin/accounts/{id}/reset-secrets`: the person's secret store is removed (the seam first), then the unlock rows
/// that hang on it, and only then is the line written. A seam that cannot do it leaves everything as it was; so does a
/// database that cannot, and the call may be made again.
async fn reset_secrets_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    seam(services.secrets.reset(target)).await.map_err(seam_refusal)?;
    let worker = services.clone();
    blocking(move || worker.accounts.secrets_reset_done(actor, addr, target)).await?.map_err(|e| refusal(&e))?;
    Ok(no_content())
}

/// `POST /api/admin/accounts/{id}/unlock-signin`: lifts the lock of the password path of the account. An account that is not
/// locked is answered the same way.
async fn unlock_signin_route(
    State(services): State<Shared>,
    Admin(who): Admin,
    path: Result<Path<String>, PathRejection>,
    ApiJson(Object(_)): ApiJson<Object<Empty>>,
) -> Result<Response, Refusal> {
    let target = target_of(path)?;
    let (actor, addr) = (who.account.id, Some(who.client));
    require_target(&services, actor, target).await?;
    let worker = services.clone();
    blocking(move || worker.signin.unlock_signin(actor, addr, target)).await?.map_err(|e| {
        Refusal(match e {
            SignInError::Invalid => ApiError::NotFound,
            _ => ApiError::Busy,
        })
    })?;
    Ok(no_content())
}

// ---------------------------------------------------------------------------------------------------------------------
// The part
// ---------------------------------------------------------------------------------------------------------------------

/// The routes under `/api/admin`, as one part for [`crate::http::router::build`].
pub fn parts(services: Arc<AdminServices>) -> Vec<Part> {
    let router = Router::new()
        .route("/api/admin/accounts", get(list_route))
        .route("/api/admin/invitations", post(invitations_route))
        .route("/api/admin/accounts/{id}", delete(delete_route))
        .route("/api/admin/accounts/{id}/role", post(role_route))
        .route("/api/admin/accounts/{id}/disable", post(disable_route))
        .route("/api/admin/accounts/{id}/enable", post(enable_route))
        .route("/api/admin/accounts/{id}/reset-secrets", post(reset_secrets_route))
        .route("/api/admin/accounts/{id}/unlock-signin", post(unlock_signin_route))
        .with_state(services);
    vec![Part { access: Access::SessionFresh, router }]
}

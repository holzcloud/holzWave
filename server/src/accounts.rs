//! Accounts, roles, invitations and the rules that keep at least one administrator.
//!
//! Everything here is a step over the store's atomic primitives: one [`Store::write`] per operation, so a token is spent
//! only together with the account it creates, and a failing enrolment gives the token back. The module never holds a
//! secret. It takes token strings, returns a new token once, stores only the SHA-256 of a token, and none of its types or
//! errors prints one. Every operation writes one audit line after the transaction ended, `ok` when it committed and `fail`
//! when it was refused, using only the closed vocabulary of the audit module.
//!
//! Tokens are 32 random bytes written as 43 base64url characters behind a prefix: `hwb_` for the bootstrap token that
//! creates the first administrator, `hwi_` for an invitation. The hash is taken over the 32 bytes, as for a session.

use std::fmt;
use std::net::IpAddr;
use std::ops::RangeInclusive;
use std::sync::Arc;

use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::session::{generate_token, parse_token};
use crate::clock::Clock;
use crate::log::{self, Level};
use crate::store::{Account, AccountId, HANDLE_BYTES, Role, Store, StoreError, TokenHash, Tx, hash_token};

/// What a bootstrap token starts with.
pub const BOOTSTRAP_PREFIX: &str = "hwb_";

/// What an invitation token starts with.
pub const INVITE_PREFIX: &str = "hwi_";

/// The characters of a token after its prefix.
pub const TOKEN_BODY_CHARS: usize = 43;

/// How long a bootstrap token works, in seconds: one hour.
pub const BOOTSTRAP_TTL_SECS: i64 = 3_600;

/// How long an invitation works, in days.
pub const INVITE_DAYS: i64 = 7;

/// How long an invitation works, in seconds.
pub const INVITE_TTL_SECS: i64 = INVITE_DAYS * 24 * 3_600;

/// Why an operation was refused or failed. The texts are sentences; none holds a token, a name or a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountError {
    /// A token is unknown, expired or spent, or no longer allowed to create anything. One error for all of them.
    NotRedeemable,
    /// The username (or a credential the enrolment stored) is already in use.
    Conflict,
    /// The caller is not an active administrator.
    NotAllowed,
    /// A single sign-on subject may not sign in or be made an account. One value for every reason.
    Denied,
    /// The change would leave no active administrator.
    LastAdmin,
    /// Every user id of the configured range is taken.
    UidRangeFull,
    /// There is no such live account.
    NotFound,
    /// An account exists already, so there is no first administrator to make.
    AlreadySetUp,
    /// An input broke a rule; the text is a fixed sentence.
    Invalid(&'static str),
    /// The caller's enrolment failed; the text is a fixed sentence of the caller.
    Enrolment(&'static str),
    /// The operating system gave no random bytes.
    Random,
    /// The store failed.
    Store(StoreError),
}

impl AccountError {
    /// A short word for the audit line.
    fn reason(&self) -> &'static str {
        match self {
            AccountError::NotRedeemable => "token",
            AccountError::Conflict => "conflict",
            AccountError::NotAllowed => "not_allowed",
            AccountError::Denied => "denied",
            AccountError::LastAdmin => "last_admin",
            AccountError::UidRangeFull => "uid_range",
            AccountError::NotFound => "not_found",
            AccountError::AlreadySetUp => "set_up",
            AccountError::Invalid(_) => "invalid",
            AccountError::Enrolment(_) => "enrolment",
            AccountError::Random => "random",
            AccountError::Store(_) => "storage",
        }
    }
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountError::NotRedeemable => f.write_str("the token is not valid"),
            AccountError::Conflict => f.write_str("that name or credential is already in use"),
            AccountError::NotAllowed => f.write_str("only an active administrator can do that"),
            AccountError::Denied => f.write_str("the sign-in is not allowed"),
            AccountError::LastAdmin => {
                f.write_str("the last active administrator cannot be demoted, disabled or deleted")
            }
            AccountError::UidRangeFull => f.write_str("no free user ids in the configured range"),
            AccountError::NotFound => f.write_str("no such account"),
            AccountError::AlreadySetUp => {
                f.write_str("an account exists already; the bootstrap token only creates the first administrator")
            }
            AccountError::Invalid(reason) | AccountError::Enrolment(reason) => f.write_str(reason),
            AccountError::Random => f.write_str("the operating system gave no random bytes"),
            AccountError::Store(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for AccountError {}

impl From<StoreError> for AccountError {
    fn from(e: StoreError) -> AccountError {
        match e {
            StoreError::NotRedeemable => AccountError::NotRedeemable,
            StoreError::Conflict => AccountError::Conflict,
            StoreError::LastAdmin => AccountError::LastAdmin,
            StoreError::UidRangeFull => AccountError::UidRangeFull,
            StoreError::NotFound => AccountError::NotFound,
            StoreError::Invalid(reason) => AccountError::Invalid(reason),
            other => AccountError::Store(other),
        }
    }
}

/// A one-time token in its written form. It is shown once, wiped when dropped, and prints as a marker.
pub struct Token(zeroize::Zeroizing<String>);

impl Token {
    /// The token as the person types or follows it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

/// A token that was just made, and when it stops working.
#[derive(Debug)]
pub struct Issued {
    /// The token. It is not stored anywhere, only its hash is.
    pub token: Token,
    /// Unix seconds after which the token no longer works.
    pub expires_at: i64,
}

/// A new token and the hash that is stored for it.
fn new_token(prefix: &str) -> Result<(Token, TokenHash), AccountError> {
    let (body, bytes) = generate_token().map_err(|_| AccountError::Random)?;
    let mut text = zeroize::Zeroizing::new(String::with_capacity(prefix.len() + TOKEN_BODY_CHARS));
    text.push_str(prefix);
    text.push_str(body.expose());
    Ok((Token(text), hash_token(&bytes[..])))
}

/// The hash of a token a person presented, or `None` for a text that cannot be a token of this kind. It looks at nothing but
/// the text.
fn presented_hash(prefix: &str, text: &str) -> Option<TokenHash> {
    parse_token(text.strip_prefix(prefix)?).ok()
}

/// The line `serve` and `bootstrap` print for a bootstrap token: one line that holds the token, the address to open and how
/// long the token works.
pub fn announcement(public_url: &str, issued: &Issued) -> String {
    format!(
        "To create the first administrator open {public_url} and use the one-time token {} within {} minutes.",
        issued.token.expose(),
        BOOTSTRAP_TTL_SECS / 60
    )
}

/// What a new person chooses for their account. The role comes from the token that is redeemed.
#[derive(Debug, Clone)]
pub struct NewAccount<'a> {
    /// The sign-in name.
    pub username: &'a str,
    /// The name shown to people.
    pub display_name: &'a str,
    /// The WebAuthn handle a passkey enrolment chose before the account existed, or `None` for a random one.
    pub handle: Option<[u8; HANDLE_BYTES]>,
}

impl NewAccount<'_> {
    fn for_store(&self, role: Role) -> crate::store::NewAccount<'_> {
        crate::store::NewAccount {
            username: self.username,
            display_name: self.display_name,
            role,
            handle: self.handle,
        }
    }
}

/// The accounts of the server.
pub struct Accounts {
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    audit: Arc<Audit>,
    uid_range: RangeInclusive<u32>,
}

impl fmt::Debug for Accounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Accounts(..)")
    }
}

impl Accounts {
    /// The service over `store`; `uid_range` is the range of operating-system user ids new accounts are given.
    pub fn new(
        store: Arc<Store>,
        clock: Arc<dyn Clock>,
        audit: Arc<Audit>,
        uid_range: RangeInclusive<u32>,
    ) -> Accounts {
        Accounts { store, clock, audit, uid_range }
    }

    /// Runs `f` in one write transaction. An error of `f` rolls the transaction back and comes out unchanged.
    fn run<T>(&self, f: impl FnOnce(&Tx<'_>) -> Result<T, AccountError>) -> Result<T, AccountError> {
        let mut failure: Option<AccountError> = None;
        let written = self.store.write(|tx| match f(tx) {
            Ok(value) => Ok(value),
            Err(e) => {
                failure = Some(e);
                Err(StoreError::Invalid("the change was refused"))
            }
        });
        match written {
            Ok(value) => Ok(value),
            Err(e) => Err(failure.take().unwrap_or_else(|| e.into())),
        }
    }

    /// Writes the audit line of a finished call. A line that cannot be written is logged by name and does not undo a change
    /// that is committed.
    fn finish<T>(
        &self,
        action: Action,
        actor: Actor,
        addr: Option<IpAddr>,
        target: Option<AccountId>,
        result: &Result<T, AccountError>,
        details: &[(DetailKey, DetailValue)],
    ) {
        let mut event = match result {
            Ok(_) => Event::new(action, Outcome::Ok, actor, addr),
            Err(e) => Event::new(action, Outcome::Fail, actor, addr).detail(DetailKey::Reason, DetailValue::Text(e.reason())),
        };
        if let Some(target) = target {
            event = event.target(target);
        }
        if result.is_ok() {
            for (key, value) in details {
                event = event.detail(*key, *value);
            }
        }
        if self.audit.record(&event).is_err() {
            log::event(Level::Error, "accounts.audit_failed", &[("action", action.as_str())]);
        }
    }

    // ---- the first administrator ----

    /// Makes the one-time token the first administrator redeems and stores its hash, replacing an earlier unredeemed token.
    /// Refused with [`AccountError::AlreadySetUp`] as soon as any account exists. The token works for an hour; it cannot be
    /// shown again, so a caller prints it once.
    pub fn bootstrap_issue(&self) -> Result<Issued, AccountError> {
        let (token, hash) = new_token(BOOTSTRAP_PREFIX)?;
        let expires_at = self.clock.unix().saturating_add(BOOTSTRAP_TTL_SECS);
        match self.store.bootstrap_set(&hash, expires_at) {
            Ok(()) => Ok(Issued { token, expires_at }),
            Err(StoreError::Conflict) => Err(AccountError::AlreadySetUp),
            Err(e) => Err(e.into()),
        }
    }

    /// Redeems the bootstrap token: in one transaction the token is spent, no account is found to exist, the first
    /// administrator is made with the first user id of the range, and `enrol` runs with the open transaction and the new
    /// account (it stores the password and the authenticator, or the passkey). When `enrol` fails, or anything else does,
    /// nothing is kept and the token stays valid. An unknown, expired and spent token are the same error.
    pub fn redeem_bootstrap<E: Into<AccountError>>(
        &self,
        token: &str,
        new: &NewAccount<'_>,
        addr: Option<IpAddr>,
        enrol: impl FnOnce(&Tx<'_>, &Account) -> Result<(), E>,
    ) -> Result<Account, AccountError> {
        let result = (|| {
            let hash = presented_hash(BOOTSTRAP_PREFIX, token).ok_or(AccountError::NotRedeemable)?;
            let now = self.clock.unix();
            self.run(|tx| {
                tx.bootstrap_redeem(&hash, now)?;
                let account = tx.account_create(&new.for_store(Role::Admin), self.uid_range.clone(), now)?;
                enrol(tx, &account).map_err(Into::into)?;
                Ok(account)
            })
        })();
        match &result {
            Ok(account) => self.finish(
                Action::BootstrapRedeem,
                Actor::Account(account.id),
                addr,
                Some(account.id),
                &result,
                &[(DetailKey::Role, DetailValue::Text(account.role.as_str()))],
            ),
            Err(_) => self.finish(Action::BootstrapRedeem, Actor::Anonymous, addr, None, &result, &[]),
        }
        result
    }
}

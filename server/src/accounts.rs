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
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::session::{generate_token, parse_token};
use crate::clock::Clock;
use crate::log::{self, Level};
use crate::store::{
    Account, AccountId, AccountState, HANDLE_BYTES, NewInvite, Role, Store, StoreError, TokenHash, Tx, hash_token,
};

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

/// Why an account's worker has to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// An administrator disabled the account; it can be enabled again.
    Disabled,
    /// An administrator deleted the account. Its row, user id and name stay reserved.
    Deleted,
}

impl Ending {
    fn word(self) -> &'static str {
        match self {
            Ending::Disabled => "disabled",
            Ending::Deleted => "deleted",
        }
    }
}

/// Who is told when an account is disabled or deleted. This module ends the sessions in the store and touches no home and no
/// store file of the person; the observer ends the worker and its processes and leaves the home where it is.
pub trait AccountObserver: Send + Sync {
    /// Called once per change, after it committed, with the account and its operating-system user id. A panic in here is
    /// caught and logged; it does not undo the change.
    fn account_ended(&self, _account: AccountId, _uid: u32, _ending: Ending) {}
}

/// The observer that does nothing.
struct NoObserver;

impl AccountObserver for NoObserver {}

/// The accounts of the server.
pub struct Accounts {
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    audit: Arc<Audit>,
    uid_range: RangeInclusive<u32>,
    observer: Arc<dyn AccountObserver>,
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
        Accounts { store, clock, audit, uid_range, observer: Arc::new(NoObserver) }
    }

    /// The same service, telling `observer` when an account is disabled or deleted.
    pub fn with_observer(mut self, observer: Arc<dyn AccountObserver>) -> Accounts {
        self.observer = observer;
        self
    }

    /// Tells the observer, and logs by name a panic of it.
    fn notify(&self, account: &Account, ending: Ending) {
        let observer = &self.observer;
        if catch_unwind(AssertUnwindSafe(|| observer.account_ended(account.id, account.uid, ending))).is_err() {
            log::event(Level::Error, "accounts.observer_panicked", &[("ending", ending.word())]);
        }
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
    // ---- invitations ----

    /// Makes an invitation. `for_account` is `None` for a new person, who chooses their username when they redeem it and
    /// gets `role`; it is an active account for a person who needs another credential (the invitation then keeps that
    /// account's role and username). The token is returned once and works for seven days. The actor must be an active
    /// administrator, which is checked inside the transaction.
    pub fn invite_create(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        for_account: Option<AccountId>,
        role: Role,
    ) -> Result<Issued, AccountError> {
        let result = (|| {
            let (token, hash) = new_token(INVITE_PREFIX)?;
            let expires_at = self.clock.unix().saturating_add(INVITE_TTL_SECS);
            let role = self.run(|tx| {
                require_admin(tx, actor)?;
                let role = match for_account {
                    Some(id) => match tx.account_get(id)? {
                        Some(account) if account.state == AccountState::Active => account.role,
                        _ => return Err(AccountError::NotFound),
                    },
                    None => role,
                };
                tx.invite_create(&NewInvite { token_hash: hash, account_id: for_account, role, created_by: actor, expires_at })?;
                Ok(role)
            })?;
            Ok((Issued { token, expires_at }, role))
        })();
        let details = [
            (DetailKey::Role, DetailValue::Text(result.as_ref().map_or("", |(_, role)| role.as_str()))),
            (DetailKey::Days, DetailValue::Number(INVITE_DAYS)),
        ];
        self.finish(Action::AccountInvite, Actor::Account(actor), addr, for_account, &result, &details);
        result.map(|(issued, _)| issued)
    }

    /// Redeems an invitation. For a new person the account is made with `new`'s username and display name and the
    /// invitation's role; for an invitation to an existing account `new` is ignored and that account, unchanged, goes to
    /// `enrol`. Either way the token is spent in the same transaction, and when `enrol` fails or the username is taken
    /// ([`AccountError::Conflict`]) nothing is kept and the token still works. An unknown, expired and spent token, and one
    /// whose administrator is no longer active, are the same error.
    pub fn redeem_invite<E: Into<AccountError>>(
        &self,
        token: &str,
        new: &NewAccount<'_>,
        addr: Option<IpAddr>,
        enrol: impl FnOnce(&Tx<'_>, &Account) -> Result<(), E>,
    ) -> Result<Account, AccountError> {
        let result = (|| {
            let hash = presented_hash(INVITE_PREFIX, token).ok_or(AccountError::NotRedeemable)?;
            let now = self.clock.unix();
            self.run(|tx| {
                let invite = tx.invite_redeem(&hash, now)?;
                // An invitation outlives its maker's authority no longer than the maker's role does.
                require_admin(tx, invite.created_by).map_err(|_| AccountError::NotRedeemable)?;
                let account = match invite.account_id {
                    Some(id) => match tx.account_get(id)? {
                        Some(account) if account.state == AccountState::Active => account,
                        _ => return Err(AccountError::NotRedeemable),
                    },
                    None => tx.account_create(&new.for_store(invite.role), self.uid_range.clone(), now)?,
                };
                enrol(tx, &account).map_err(Into::into)?;
                Ok(account)
            })
        })();
        match &result {
            Ok(account) => self.finish(
                Action::AccountRedeem,
                Actor::Account(account.id),
                addr,
                Some(account.id),
                &result,
                &[(DetailKey::Role, DetailValue::Text(account.role.as_str()))],
            ),
            Err(_) => self.finish(Action::AccountRedeem, Actor::Anonymous, addr, None, &result, &[]),
        }
        result
    }

    // ---- what an administrator does to an account ----

    /// Makes an account that has no credential, and one invitation for it that enrols the first one. The invitation token
    /// is returned once. Only an active administrator can do this.
    pub fn create_account(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        username: &str,
        display_name: &str,
        role: Role,
    ) -> Result<(Account, Issued), AccountError> {
        let result = (|| {
            let (token, hash) = new_token(INVITE_PREFIX)?;
            let now = self.clock.unix();
            let expires_at = now.saturating_add(INVITE_TTL_SECS);
            let account = self.run(|tx| {
                require_admin(tx, actor)?;
                let new = crate::store::NewAccount { username, display_name, role, handle: None };
                let account = tx.account_create(&new, self.uid_range.clone(), now)?;
                tx.invite_create(&NewInvite {
                    token_hash: hash,
                    account_id: Some(account.id),
                    role,
                    created_by: actor,
                    expires_at,
                })?;
                Ok(account)
            })?;
            Ok((account, Issued { token, expires_at }))
        })();
        let target = result.as_ref().ok().map(|(account, _)| account.id);
        self.finish(
            Action::AccountCreate,
            Actor::Account(actor),
            addr,
            target,
            &result,
            &[(DetailKey::Role, DetailValue::Text(role.as_str()))],
        );
        result
    }

    /// Changes the role of an account. Demoting the last active administrator is [`AccountError::LastAdmin`].
    pub fn set_role(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        target: AccountId,
        role: Role,
    ) -> Result<(), AccountError> {
        let result = self.run(|tx| {
            require_admin(tx, actor)?;
            tx.account_set_role(target, role)?;
            Ok(())
        });
        self.finish(
            Action::AccountRole,
            Actor::Account(actor),
            addr,
            Some(target),
            &result,
            &[(DetailKey::Role, DetailValue::Text(role.as_str()))],
        );
        result
    }

    /// Disables an account and ends all its sessions. The observer is told after the commit. Disabling the last active
    /// administrator is [`AccountError::LastAdmin`].
    pub fn disable(&self, actor: AccountId, addr: Option<IpAddr>, target: AccountId) -> Result<(), AccountError> {
        let result = self.run(|tx| {
            require_admin(tx, actor)?;
            let account = tx.account_get(target)?.ok_or(AccountError::NotFound)?;
            tx.account_set_state(target, AccountState::Disabled)?;
            tx.session_delete_all(target)?;
            Ok(account)
        });
        self.finish(
            Action::AccountDisable,
            Actor::Account(actor),
            addr,
            Some(target),
            &result,
            &[(DetailKey::State, DetailValue::Text(AccountState::Disabled.as_str()))],
        );
        match result {
            Ok(account) => {
                self.notify(&account, Ending::Disabled);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Enables a disabled account.
    pub fn enable(&self, actor: AccountId, addr: Option<IpAddr>, target: AccountId) -> Result<(), AccountError> {
        let result = self.run(|tx| {
            require_admin(tx, actor)?;
            tx.account_set_state(target, AccountState::Active)?;
            Ok(())
        });
        self.finish(
            Action::AccountEnable,
            Actor::Account(actor),
            addr,
            Some(target),
            &result,
            &[(DetailKey::State, DetailValue::Text(AccountState::Active.as_str()))],
        );
        result
    }

    /// Deletes an account: its credentials, identities, sessions and invitations go, and its row stays with the state
    /// `deleted`, so its user id and username are never given to anyone else. This touches no home and no store file; the
    /// observer, told after the commit, ends the worker. Deleting the last active administrator is
    /// [`AccountError::LastAdmin`].
    pub fn delete(&self, actor: AccountId, addr: Option<IpAddr>, target: AccountId) -> Result<(), AccountError> {
        let result = self.run(|tx| {
            require_admin(tx, actor)?;
            let account = tx.account_get(target)?.ok_or(AccountError::NotFound)?;
            tx.account_delete(target)?;
            Ok(account)
        });
        self.finish(
            Action::AccountDelete,
            Actor::Account(actor),
            addr,
            Some(target),
            &result,
            &[(DetailKey::State, DetailValue::Text(AccountState::Deleted.as_str()))],
        );
        match result {
            Ok(account) => {
                self.notify(&account, Ending::Deleted);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

/// Fails with [`AccountError::NotAllowed`] unless `actor` is an active administrator. It reads inside the transaction of the
/// change it guards, so a demotion that commits first is seen.
fn require_admin(tx: &Tx<'_>, actor: AccountId) -> Result<(), AccountError> {
    match tx.account_get(actor)? {
        Some(account) if account.state == AccountState::Active && account.role == Role::Admin => Ok(()),
        _ => Err(AccountError::NotAllowed),
    }
}

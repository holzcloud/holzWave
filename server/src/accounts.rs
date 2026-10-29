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
use crate::config;
use crate::log::{self, Level};
use crate::store::{
    Account, AccountId, AccountState, HANDLE_BYTES, NewInvite, Role, Store, StoreError, TokenHash, Tx, UnlockKind, hash_token,
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

/// The most days an invitation may be made to work.
pub const MAX_INVITE_DAYS: i64 = 30;

/// The most invitations that may be open (not redeemed, not expired) at once.
pub const MAX_OPEN_INVITATIONS: i64 = 200;

/// The most accounts the list of an administrator holds.
pub const MAX_LISTED_ACCOUNTS: usize = 1_000;

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
    /// There are as many open invitations as there may be.
    TooManyInvitations,
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
            AccountError::TooManyInvitations => "too_many",
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
            AccountError::TooManyInvitations => f.write_str("there are too many open invitations"),
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

/// The account an invitation is for, as a peek found it. The `Debug` text prints a marker: the facts are about a person.
#[derive(Clone)]
pub struct InviteTarget {
    /// The account.
    pub id: AccountId,
    /// Its sign-in name.
    pub username: String,
    /// The name shown to people.
    pub display_name: String,
    /// Its WebAuthn user handle, which a passkey enrolment must use.
    pub handle: [u8; HANDLE_BYTES],
    /// Whether it has any way to sign in already: a password, an authenticator, a passkey or a single sign-on identity.
    pub has_method: bool,
}

impl fmt::Debug for InviteTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteTarget(..)")
    }
}

/// What a peek at an invitation found. The `Debug` text prints a marker.
#[derive(Clone)]
pub struct InvitePeek {
    /// The role the person gets: the invitation's for a new person, the account's own for an existing one.
    pub role: Role,
    /// The account the invitation is for, or `None` for a new person.
    pub account: Option<InviteTarget>,
}

impl fmt::Debug for InvitePeek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InvitePeek(..)")
    }
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

/// One account of the administrator's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The account.
    pub account: Account,
    /// Unix seconds of the last sign-in, or `None` when the person never signed in.
    pub last_signin: Option<i64>,
}

/// What an administrator sees of the accounts: the number of active administrators and the accounts that are not deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The active administrators.
    pub admins: i64,
    /// The accounts, in the order they were made.
    pub accounts: Vec<Listed>,
}

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

    /// Runs `f` in one write transaction that is always rolled back, and answers what `f` answered. A peek asks the question of
    /// a redemption with the very query a redemption uses (an immediate transaction, the conditional update of the token), and
    /// then gives the token back, so nothing is spent, created or audited. The write lock is held for that one query only.
    fn peek<T>(&self, f: impl FnOnce(&Tx<'_>) -> Result<T, AccountError>) -> Result<T, AccountError> {
        let mut answer: Option<Result<T, AccountError>> = None;
        let written = self.store.write(|tx| {
            answer = Some(f(tx));
            Err::<(), StoreError>(StoreError::Invalid("a peek is always rolled back"))
        });
        match (answer, written) {
            (Some(answer), _) => answer,
            (None, Err(e)) => Err(e.into()),
            (None, Ok(())) => Err(AccountError::NotRedeemable),
        }
    }

    /// Looks at a bootstrap token without spending it: `Ok` when [`Accounts::redeem_bootstrap`] would accept it now, otherwise
    /// the one [`AccountError::NotRedeemable`] for an unknown, expired, spent and no-longer-allowed token. No audit line.
    pub fn peek_bootstrap(&self, token: &str) -> Result<(), AccountError> {
        let hash = presented_hash(BOOTSTRAP_PREFIX, token).ok_or(AccountError::NotRedeemable)?;
        let now = self.clock.unix();
        self.peek(|tx| tx.bootstrap_redeem(&hash, now).map_err(AccountError::from))
    }

    // ---- invitations ----

    /// Makes an invitation that works for seven days. See [`Accounts::invite_create_days`].
    pub fn invite_create(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        for_account: Option<AccountId>,
        role: Role,
    ) -> Result<Issued, AccountError> {
        self.invite_create_days(actor, addr, for_account, role, INVITE_DAYS)
    }

    /// Makes an invitation that works for `days` (1 to [`MAX_INVITE_DAYS`]). `for_account` is `None` for a new person, who
    /// chooses their username when they redeem it and gets `role`; it is an active account for a person who needs another
    /// credential (the invitation then keeps that account's role and username). The token is returned once. The actor must be
    /// an active administrator, and there must be fewer than [`MAX_OPEN_INVITATIONS`] open invitations; both are checked inside
    /// the transaction.
    pub fn invite_create_days(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        for_account: Option<AccountId>,
        role: Role,
        days: i64,
    ) -> Result<Issued, AccountError> {
        let result = (|| {
            let ttl = ttl_of(days)?;
            let (token, hash) = new_token(INVITE_PREFIX)?;
            let now = self.clock.unix();
            let expires_at = now.saturating_add(ttl);
            let role = self.run(|tx| {
                require_admin(tx, actor)?;
                let role = match for_account {
                    Some(id) => match tx.account_get(id)? {
                        Some(account) if account.state == AccountState::Active => account.role,
                        _ => return Err(AccountError::NotFound),
                    },
                    None => role,
                };
                require_room_for_invitation(tx, now)?;
                tx.invite_create(&NewInvite { token_hash: hash, account_id: for_account, role, created_by: actor, expires_at })?;
                Ok(role)
            })?;
            Ok((Issued { token, expires_at }, role))
        })();
        let details = [
            (DetailKey::Role, DetailValue::Text(result.as_ref().map_or("", |(_, role)| role.as_str()))),
            (DetailKey::Days, DetailValue::Number(days)),
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

    /// Looks at an invitation without spending it: the role, and for an invitation to an existing account that account's facts.
    /// It asks the question of [`Accounts::redeem_invite`] with the very same query and the same checks, in a transaction that
    /// is rolled back, so nothing is spent, created or audited. An unknown, expired and spent token, and one whose
    /// administrator is no longer active, are the one [`AccountError::NotRedeemable`].
    pub fn peek_invite(&self, token: &str) -> Result<InvitePeek, AccountError> {
        let hash = presented_hash(INVITE_PREFIX, token).ok_or(AccountError::NotRedeemable)?;
        let now = self.clock.unix();
        self.peek(|tx| {
            let invite = tx.invite_redeem(&hash, now)?;
            require_admin(tx, invite.created_by).map_err(|_| AccountError::NotRedeemable)?;
            let Some(id) = invite.account_id else { return Ok(InvitePeek { role: invite.role, account: None }) };
            let account = match tx.account_get(id)? {
                Some(account) if account.state == AccountState::Active => account,
                _ => return Err(AccountError::NotRedeemable),
            };
            let has_method = Accounts::has_method(tx, account.id)?;
            Ok(InvitePeek {
                role: account.role,
                account: Some(InviteTarget {
                    id: account.id,
                    username: account.username,
                    display_name: account.display_name,
                    handle: account.handle,
                    has_method,
                }),
            })
        })
    }

    /// Whether an account can sign in already: it has a password, an authenticator, a passkey or a single sign-on identity.
    /// An invitation to such an account is not accepted, so that an invitation cannot be used to add a second way in.
    pub fn has_method(tx: &Tx<'_>, id: AccountId) -> Result<bool, AccountError> {
        Ok(tx.password_get(id)?.is_some()
            || tx.totp_get(id)?.is_some()
            || !tx.passkey_list(id)?.is_empty()
            || !tx.oidc_list(id)?.is_empty())
    }

    // ---- single sign-on ----

    /// The account of a person who signed in at an OpenID Connect provider, whose tokens the caller already verified.
    ///
    /// An identity that is linked is matched by issuer and subject only, never by e-mail or name, and gives its account when
    /// that is active. An identity that is not linked becomes a new member account, with the identity linked in the same
    /// transaction, only when the claims satisfy `rules`: a verified e-mail whose domain equals (ignoring case) one of
    /// `allowed_domains`, or a group equal (exactly) to one of `allowed_groups`. With both lists empty no account is ever
    /// made. Every refusal is the one value [`AccountError::Denied`]; nothing is made or kept then. The username is derived
    /// with [`derive_username`].
    pub fn oidc_sign_in(
        &self,
        claims: &OidcClaims,
        rules: &ProviderRules,
        addr: Option<IpAddr>,
    ) -> Result<Account, AccountError> {
        let now = self.clock.unix();
        let mut linked = false;
        let mut created = false;
        let result = self.run(|tx| {
            if !claims.well_formed() {
                return Err(AccountError::Denied);
            }
            if let Some(id) = tx.oidc_find(&claims.issuer, &claims.subject)? {
                linked = true;
                return match tx.account_get(id)? {
                    Some(account) if account.state == AccountState::Active => Ok(account),
                    _ => Err(AccountError::Denied),
                };
            }
            if !rules.allows(claims) {
                return Err(AccountError::Denied);
            }
            let username = free_username(tx, &derive_username(claims.preferred_username.as_deref(), claims.email.as_deref()))?
                .ok_or(AccountError::Denied)?;
            let display_name = shown_name(claims.display_name.as_deref(), &username);
            let new = crate::store::NewAccount { username: &username, display_name: &display_name, role: Role::Member, handle: None };
            let account = tx.account_create(&new, self.uid_range.clone(), now)?;
            tx.oidc_link(account.id, &claims.issuer, &claims.subject)?;
            created = true;
            Ok(account)
        });
        match &result {
            Ok(account) if created => self.finish(
                Action::OidcCreate,
                Actor::Account(account.id),
                addr,
                Some(account.id),
                &result,
                &[(DetailKey::Role, DetailValue::Text(account.role.as_str()))],
            ),
            // A sign-in of a linked person is recorded by the sign-in flow, and so is a refusal of a linked identity.
            Ok(_) => {}
            Err(_) if linked => {}
            Err(_) => self.finish(Action::OidcCreate, Actor::Anonymous, addr, None, &result, &[]),
        }
        result
    }

    // ---- what an administrator does to an account ----

    /// The accounts that are not deleted, with the time each last signed in, and the number of active administrators. Only an
    /// active administrator may ask, which is checked in the read transaction. At most [`MAX_LISTED_ACCOUNTS`] accounts are
    /// listed. A read: it writes no audit line.
    pub fn list(&self, actor: AccountId) -> Result<Listing, AccountError> {
        self.store
            .read(|tx| {
                let allowed = tx
                    .account_get(actor)?
                    .is_some_and(|a| a.state == AccountState::Active && a.role == Role::Admin);
                if !allowed {
                    return Ok(Err(AccountError::NotAllowed));
                }
                let admins = tx.admin_count_active()?;
                let accounts = tx
                    .account_list_signins(MAX_LISTED_ACCOUNTS)?
                    .into_iter()
                    .map(|(account, last_signin)| Listed { account, last_signin })
                    .collect();
                Ok(Ok(Listing { admins, accounts }))
            })?
    }

    /// Checks, in one read, that `actor` is an active administrator and that `target` is an account that is not deleted. It
    /// writes no audit line: a call that names nothing is not a change that failed.
    pub fn check_target(&self, actor: AccountId, target: AccountId) -> Result<(), AccountError> {
        self.store.read(|tx| {
            let allowed =
                tx.account_get(actor)?.is_some_and(|a| a.state == AccountState::Active && a.role == Role::Admin);
            if !allowed {
                return Ok(Err(AccountError::NotAllowed));
            }
            let found = tx.account_get(target)?.is_some_and(|a| a.state != AccountState::Deleted);
            Ok(if found { Ok(()) } else { Err(AccountError::NotFound) })
        })?
    }

    /// Removes the unlock methods that hang on the secret store of `target` (passphrase, PRF keys and recovery code) once the
    /// store itself has been reset, in one transaction, and writes the `secrets.reset` line. The password row stays: it is how
    /// the person gets back in and starts a new store. A failure writes no line, so that the line means the reset is done.
    pub fn secrets_reset_done(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        target: AccountId,
    ) -> Result<(), AccountError> {
        let result = self.run(|tx| {
            require_admin(tx, actor)?;
            if tx.account_get(target)?.is_none_or(|a| a.state == AccountState::Deleted) {
                return Err(AccountError::NotFound);
            }
            tx.unlock_remove_kinds(target, &[UnlockKind::Passphrase, UnlockKind::Prf, UnlockKind::Recovery])?;
            Ok(())
        });
        if result.is_ok() {
            self.finish(Action::SecretsReset, Actor::Account(actor), addr, Some(target), &result, &[]);
        }
        result
    }

    /// Makes an account that has no credential, and one invitation for it that enrols the first one and works for seven days.
    /// See [`Accounts::create_account_days`].
    pub fn create_account(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        username: &str,
        display_name: &str,
        role: Role,
    ) -> Result<(Account, Issued), AccountError> {
        self.create_account_days(actor, addr, username, display_name, role, INVITE_DAYS)
    }

    /// Makes an account that has no credential, and one invitation for it that enrols the first one and works for `days`
    /// (1 to [`MAX_INVITE_DAYS`]). The invitation token is returned once. Only an active administrator can do this, and only
    /// while there are fewer than [`MAX_OPEN_INVITATIONS`] open invitations.
    pub fn create_account_days(
        &self,
        actor: AccountId,
        addr: Option<IpAddr>,
        username: &str,
        display_name: &str,
        role: Role,
        days: i64,
    ) -> Result<(Account, Issued), AccountError> {
        let result = (|| {
            let ttl = ttl_of(days)?;
            let (token, hash) = new_token(INVITE_PREFIX)?;
            let now = self.clock.unix();
            let expires_at = now.saturating_add(ttl);
            let account = self.run(|tx| {
                require_admin(tx, actor)?;
                require_room_for_invitation(tx, now)?;
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

/// The seconds an invitation of `days` days works, or an error for a number of days outside 1 to [`MAX_INVITE_DAYS`].
fn ttl_of(days: i64) -> Result<i64, AccountError> {
    if (1..=MAX_INVITE_DAYS).contains(&days) {
        Ok(days * 24 * 3_600)
    } else {
        Err(AccountError::Invalid("an invitation works for 1 to 30 days"))
    }
}

/// Fails with [`AccountError::TooManyInvitations`] when [`MAX_OPEN_INVITATIONS`] invitations are open at `now`. It runs in the
/// transaction of the invitation it guards, so two callers cannot both take the last place.
fn require_room_for_invitation(tx: &Tx<'_>, now: i64) -> Result<(), AccountError> {
    if tx.invite_open_count(now)? >= MAX_OPEN_INVITATIONS { Err(AccountError::TooManyInvitations) } else { Ok(()) }
}

/// Fails with [`AccountError::NotAllowed`] unless `actor` is an active administrator. It reads inside the transaction of the
/// change it guards, so a demotion that commits first is seen.
fn require_admin(tx: &Tx<'_>, actor: AccountId) -> Result<(), AccountError> {
    match tx.account_get(actor)? {
        Some(account) if account.state == AccountState::Active && account.role == Role::Admin => Ok(()),
        _ => Err(AccountError::NotAllowed),
    }
}

/// The most groups of one person that are looked at.
const MAX_GROUPS: usize = 1_024;

/// The longest issuer and subject that are accepted, as the store keeps them.
const MAX_ISSUER: usize = 512;
const MAX_SUBJECT: usize = 255;

/// The longest e-mail address that is looked at, in bytes.
const MAX_EMAIL: usize = 320;

/// The longest username that is derived from a claim, in characters.
const DERIVED_USERNAME_CHARS: usize = 32;

/// The most numbered names tried when a derived username is taken: `name`, `name-2`, ... `name-99`.
const MAX_USERNAME_TRIES: u32 = 99;

/// What a provider said about a person. The caller verified the tokens; this module evaluates the statements against the
/// operator's lists. The `Debug` text prints a marker, because every field is about a person.
#[derive(Clone, Default)]
pub struct OidcClaims {
    /// The issuer URL of the provider.
    pub issuer: String,
    /// The subject at that issuer: the stable identity.
    pub subject: String,
    /// The e-mail address.
    pub email: Option<String>,
    /// Whether the provider says it verified the address.
    pub email_verified: bool,
    /// The `preferred_username` claim.
    pub preferred_username: Option<String>,
    /// The name to show.
    pub display_name: Option<String>,
    /// The groups, from the claim the provider's configuration names.
    pub groups: Vec<String>,
}

impl fmt::Debug for OidcClaims {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OidcClaims(..)")
    }
}

impl OidcClaims {
    /// Whether the identity can be stored: an issuer and a subject of a plausible size without control characters.
    fn well_formed(&self) -> bool {
        let plain = |s: &str, max: usize| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control);
        plain(&self.issuer, MAX_ISSUER) && plain(&self.subject, MAX_SUBJECT)
    }
}

/// The lists of a provider that decide who becomes an account.
#[derive(Debug, Clone, Default)]
pub struct ProviderRules {
    /// E-mail domains, compared ignoring case and exactly (no sub-domains).
    pub allowed_domains: Vec<String>,
    /// Groups, compared exactly.
    pub allowed_groups: Vec<String>,
}

impl From<&config::Oidc> for ProviderRules {
    fn from(provider: &config::Oidc) -> ProviderRules {
        ProviderRules {
            allowed_domains: provider.allowed_domains.clone(),
            allowed_groups: provider.allowed_groups.clone(),
        }
    }
}

impl ProviderRules {
    /// Whether the claims satisfy a list. Both lists empty allows nobody.
    fn allows(&self, claims: &OidcClaims) -> bool {
        let by_domain = claims.email_verified
            && claims
                .email
                .as_deref()
                .and_then(email_domain)
                .is_some_and(|domain| self.allowed_domains.iter().any(|allowed| allowed.eq_ignore_ascii_case(domain)));
        let by_group = claims
            .groups
            .iter()
            .take(MAX_GROUPS)
            .any(|group| !group.is_empty() && self.allowed_groups.iter().any(|allowed| allowed == group));
        by_domain || by_group
    }
}

/// The part of an address after its last `@`, when there is a local part before it.
fn email_domain(email: &str) -> Option<&str> {
    if email.len() > MAX_EMAIL {
        return None;
    }
    let (local, domain) = email.rsplit_once('@')?;
    (!local.is_empty() && !domain.is_empty()).then_some(domain)
}

/// A claim made into a username candidate: lower case, every character outside `a-z 0-9 . _ -` replaced by `-`, no dot at
/// the start or the end, at most 32 characters. A claim with no letter or digit in it gives `None`.
fn normalise(claim: &str) -> Option<String> {
    let mapped: String = claim
        .to_lowercase()
        .chars()
        .map(|c| if matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-') { c } else { '-' })
        .collect();
    let cut: String = mapped.trim_matches('.').chars().take(DERIVED_USERNAME_CHARS).collect();
    let name = cut.trim_matches('.');
    name.bytes().any(|b| b.is_ascii_alphanumeric()).then(|| name.to_owned())
}

/// The username a just-in-time account starts from: the `preferred_username` claim, else the part of the e-mail address before
/// its last `@`, else `user`, normalised so that a claim can never put anything but `a-z 0-9 . _ -` into a name (see
/// [`normalise`]). A claim with no letter or digit counts as missing. A collision is handled by the caller with a suffix.
pub fn derive_username(preferred: Option<&str>, email: Option<&str>) -> String {
    preferred
        .and_then(normalise)
        .or_else(|| email.filter(|e| e.len() <= MAX_EMAIL).and_then(|e| e.rsplit_once('@')).and_then(|(local, _)| normalise(local)))
        .unwrap_or_else(|| "user".to_owned())
}

/// `base`, or the first of `base-2` to `base-99` that no account (deleted ones included) has, or `None` when all are taken.
fn free_username(tx: &Tx<'_>, base: &str) -> Result<Option<String>, AccountError> {
    for n in 1..=MAX_USERNAME_TRIES {
        let candidate = if n == 1 { base.to_owned() } else { format!("{base}-{n}") };
        if tx.account_by_username(&candidate)?.is_none() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// The name to show: the claim without control characters, cut to 128 characters, or the username when nothing is left.
fn shown_name(claim: Option<&str>, username: &str) -> String {
    let name: String = claim.unwrap_or("").chars().filter(|c| !c.is_control()).take(128).collect();
    let name = name.trim();
    if name.is_empty() { username.to_owned() } else { name.to_owned() }
}

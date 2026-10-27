//! Rate limits and lock-outs for sign-in attempts, per account and per address.
//!
//! A failed password or code is counted under two keys: the account key, the SHA-256 of the lower-cased name that was typed,
//! and the address key, the client address as the connection admission counts it (an IPv6 address by its /64). Reaching the
//! limit of a key locks it for a while; a locked key refuses every attempt, with the right password too, before anything that
//! costs is done. The account key is made from what was typed, not from what exists, so an unknown name is counted and locked
//! exactly like a known one and the lock tells nothing about which accounts exist.
//!
//! The counts live in the store's `throttle` table and are changed by its atomic upsert, so racing failures are counted
//! exactly. Only a method that can be guessed is throttled; a passkey or a single sign-on cannot be, and does not come here.
//! The sign-in flow turns a lock into its `Throttled` answer (see the sign-in module).

use std::net::IpAddr;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::limits::Limits;
use crate::proxy::key_of;
use crate::store::{Store, StoreError, ThrottleRule, ThrottleState};

/// The longest user name that is hashed as it is. A longer text cannot be a name, and all of them share one key.
const MAX_NAME_BYTES: usize = 128;

/// What the key of an over-long name is made from.
const OVERSIZE: &[u8] = b"\0oversize";

/// The numbers of the throttle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Failures per account key within the window before it is locked.
    pub account_limit: i64,
    /// Failures per address key within the window before it is locked.
    pub address_limit: i64,
    /// How long failures are counted, in seconds.
    pub window_secs: i64,
    /// How long a lock lasts, in seconds.
    pub lock_secs: i64,
}

fn secs(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

impl Policy {
    /// The policy of the limits in force.
    pub fn from_limits(limits: &Limits) -> Policy {
        Policy {
            account_limit: i64::from(limits.password_attempts_per_account),
            address_limit: i64::from(limits.password_attempts_per_address),
            window_secs: secs(limits.attempt_window),
            lock_secs: secs(limits.lock_duration),
        }
    }

    fn account_rule(&self) -> ThrottleRule {
        ThrottleRule { window_secs: self.window_secs, limit: self.account_limit, lock_secs: self.lock_secs }
    }

    fn address_rule(&self) -> ThrottleRule {
        ThrottleRule { window_secs: self.window_secs, limit: self.address_limit, lock_secs: self.lock_secs }
    }
}

/// The key a user name is counted under: the SHA-256 of the name in lower case, as lower-case hex behind a letter. No trimming:
/// a name with a space around it is another name. A text longer than a name can be shares one key.
pub fn account_key(username: &str) -> String {
    let mut hash = Sha256::new();
    if username.len() > MAX_NAME_BYTES {
        hash.update(OVERSIZE);
    } else {
        hash.update(username.to_lowercase().as_bytes());
    }
    let mut key = String::with_capacity(2 + 64);
    key.push_str("a:");
    for byte in hash.finalize() {
        key.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        key.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    key
}

/// The key a client address is counted under: the one the connection admission uses (IPv6 grouped by its /64).
pub fn address_key(addr: IpAddr) -> String {
    format!("i:{}", key_of(addr))
}

/// What one counted failure did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// Whether this failure was the one that reached a limit and started a lock.
    pub started_lock: bool,
}

/// The throttle over the store.
#[derive(Debug, Clone)]
pub struct Throttle {
    store: Arc<Store>,
    policy: Policy,
}

impl Throttle {
    /// A throttle with `policy`.
    pub fn new(store: Arc<Store>, policy: Policy) -> Throttle {
        Throttle { store, policy }
    }

    /// The policy in force.
    pub fn policy(&self) -> Policy {
        self.policy
    }

    /// The seconds left of the longest lock among the keys at `now` (at least one), or `None` when none is locked. Only
    /// reads; nothing is counted.
    pub fn check(&self, account: Option<&str>, address: &str, now: i64) -> Result<Option<u64>, StoreError> {
        let window = self.policy.window_secs;
        let (a, b) = self.store.read(|tx| {
            let a = match account {
                Some(key) => Some(tx.throttle_get(key, now, window)?),
                None => None,
            };
            Ok((a, tx.throttle_get(address, now, window)?))
        })?;
        let until = [a, Some(b)].into_iter().flatten().filter(|s| s.is_locked(now)).map(|s| s.locked_until).max();
        Ok(until.map(|u| u64::try_from(u.saturating_sub(now)).unwrap_or(1).max(1)))
    }

    /// Counts one failure under the address key and, when there is one, the account key, in one transaction.
    pub fn record_failure(&self, account: Option<&str>, address: &str, now: i64) -> Result<Failure, StoreError> {
        let policy = self.policy;
        self.store.write(|tx| {
            let mut started = false;
            if let Some(key) = account {
                started |= tx.throttle_fail(key, now, &policy.account_rule())?.failures == policy.account_limit;
            }
            started |= tx.throttle_fail(address, now, &policy.address_rule())?.failures == policy.address_limit;
            Ok(Failure { started_lock: started })
        })
    }

    /// Forgets the failures of an account key (a finished sign-in, or an administrator's unlock); answers whether there were
    /// any.
    pub fn clear_account(&self, account: &str) -> Result<bool, StoreError> {
        self.store.write(|tx| tx.throttle_clear(account))
    }

    /// The state of the key of `username` at `now`.
    pub fn account_state(&self, username: &str, now: i64) -> Result<ThrottleState, StoreError> {
        let key = account_key(username);
        self.store.read(|tx| tx.throttle_get(&key, now, self.policy.window_secs))
    }

    /// Whether the name is locked at `now`.
    pub fn is_locked(&self, username: &str, now: i64) -> Result<bool, StoreError> {
        Ok(self.account_state(username, now)?.is_locked(now))
    }

    /// Unix seconds until which the name is locked, or `None` when it is not.
    pub fn locked_until(&self, username: &str, now: i64) -> Result<Option<i64>, StoreError> {
        let state = self.account_state(username, now)?;
        Ok(state.is_locked(now).then_some(state.locked_until))
    }

    /// Removes the state whose window ended and that is not locked, at most `max_rows` of it; answers how many. A caller that
    /// gets `max_rows` back calls again.
    pub fn purge(&self, now: i64, max_rows: usize) -> Result<usize, StoreError> {
        self.store.write(|tx| tx.throttle_purge(now, self.policy.window_secs, max_rows))
    }
}

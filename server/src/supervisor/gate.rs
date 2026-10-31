//! The supervisor side of the secret store: the gate the sign-in routes tell about a new session, the view of the application
//! on whether a store is open, and the seam of the account routes that changes its slots.
//!
//! Each operation is one control frame to the worker of the person (started when none runs) and the worker's answer mapped to
//! its outcome, bounded in time by [`limits::GATE_WAIT`]. A key is the wiping type of the wire, built from the key the caller
//! handed over, sent once and never kept: the gate holds no key and prints none.
//!
//! The policy lives here too. The key of a store is in memory for as long as a session of the person is live: when a session
//! ends (an announcement of the session manager), when the time of a session runs out (the sweep, every
//! [`LOCK_SWEEP_INTERVAL`]) or when the account is disabled or deleted, the gate counts the live sessions of the person again
//! inside the person's lock and, when there are none, tells the worker to lock the store. A sign-in that finds no store creates
//! one with the key it carries and never replaces one that exists. The lines `secrets.unlock`, `recovery.use` (an explicit
//! unlock) and `secrets.lock` (the key left memory) are written here, once each; a sign-in, an enrolment and a reset write none
//! (their own line is their record), and a failed write of a line never changes what happened.
//!
//! One lock per account serialises everything that touches its store, so two unlocks of the same person end in one unlocked
//! state and a control never overtakes the one before it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use super::registry::{AccountKey, Supervisor};
use crate::audit::{Action, Actor, Audit, DetailKey, DetailValue, Event, Outcome};
use crate::auth::password::{ArgonGate, PasswordError, parse_params};
use crate::auth::session::SessionManager;
use crate::clock::Clock;
use crate::http::app::{StoreState, StoreView};
use crate::http::routes_me::{SecretsAdmin, SecretsError};
use crate::http::routes_unlock::{GateError, GateFuture, Material, ProvisionKey, UnlockGate, UnlockSecret};
use crate::limits;
use crate::log::{self, Level};
use crate::store::{AccountId, AccountState, Store, UnlockKind};
use crate::unlock::{SALT_BYTES, kek_from_prf, kek_from_recovery, parse_recovery_code};
use crate::wire::{Ctl, CtlOutcome, Key, OpeningKey, SLOT_ID_BYTES, SlotKey, StoreState as WireState};
use crate::worker::secretstore::SlotKind;

/// Everything the gate is built from. One value, so that a later plan adds behaviour without changing the constructor or the
/// assembly.
#[derive(Clone)]
pub struct GateServices {
    /// The workers.
    pub supervisor: Supervisor,
    /// The accounts and their unlock methods.
    pub store: Arc<Store>,
    /// The sessions (held for the policy of the layer above).
    pub sessions: Arc<SessionManager>,
    /// The audit writer (held, not used here).
    pub audit: Arc<Audit>,
    /// The clock.
    pub clock: Arc<dyn Clock>,
    /// The bounded gate for key derivations.
    pub argon: ArgonGate,
}

impl fmt::Debug for GateServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GateServices(..)")
    }
}

/// How often the gate looks at the sessions of every person whose store is open, whatever happened to them. A session whose
/// idle or absolute time ran out is not announced by anything, so this is the longest a key outlives the last session that
/// could have used it (besides the sign-out, which is announced and handled at once).
pub const LOCK_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// The cause words of a `secrets.lock` line.
const CAUSE_SIGNOUT: &str = "signout";
const CAUSE_EXPIRED: &str = "expired";
const CAUSE_DISABLED: &str = "disabled";

/// What made the gate look at the sessions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// A session was ended on purpose.
    Ended,
    /// The periodic look.
    Sweep,
}

/// What the gate holds, behind one pointer, so that the task of the policy and the gate itself share it.
struct Core {
    services: GateServices,
    wait: Duration,
    locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    /// The accounts whose key the gate put into a worker's memory and has not seen leave. Only ever changed while the lock of
    /// the account is held, except for the forgetting of an account that is gone.
    open: Mutex<HashSet<i64>>,
}

/// The gate: the unlock gate of the sign-in routes, the store view of the application and the secrets seam of the account
/// routes, over the supervisor.
pub struct SupervisorGate {
    core: Arc<Core>,
}

impl fmt::Debug for SupervisorGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SupervisorGate(..)")
    }
}

/// The kind of the worker side for the kind of an unlock method.
pub fn slot_kind(kind: UnlockKind) -> SlotKind {
    match kind {
        UnlockKind::Password => SlotKind::Password,
        UnlockKind::Passphrase => SlotKind::Passphrase,
        UnlockKind::Prf => SlotKind::Prf,
        UnlockKind::Recovery => SlotKind::Recovery,
    }
}

/// The id of a slot: zeros for the kinds that have one slot, the first sixteen bytes of the SHA-256 of the credential id for
/// a passkey, so that every passkey of a person has a slot of its own.
pub fn slot_id(kind: UnlockKind, credential_id: Option<&[u8]>) -> [u8; SLOT_ID_BYTES] {
    let mut id = [0u8; SLOT_ID_BYTES];
    if kind == UnlockKind::Prf
        && let Some(credential) = credential_id
    {
        let digest = Sha256::digest(credential);
        id.copy_from_slice(&digest[..SLOT_ID_BYTES]);
    }
    id
}

fn slot_key(key: &ProvisionKey) -> SlotKey {
    SlotKey { kind: slot_kind(key.kind()), id: slot_id(key.kind(), key.credential_id()), key: Key::from_bytes(key.key()) }
}

impl SupervisorGate {
    /// The gate over `services`, waiting at most [`limits::GATE_WAIT`] for any answer. Inside a runtime it starts the task
    /// that locks stores when the last session of a person ends.
    pub fn new(services: GateServices) -> SupervisorGate {
        SupervisorGate::with_wait(services, limits::GATE_WAIT)
    }

    /// The same with another bound on the wait.
    pub fn with_wait(services: GateServices, wait: Duration) -> SupervisorGate {
        let core = Arc::new(Core { services, wait, locks: Mutex::new(HashMap::new()), open: Mutex::new(HashSet::new()) });
        Core::start_policy(&core);
        SupervisorGate { core }
    }

    /// What the worker last said about the store of `account`, as a watch channel: the value changes whenever the worker
    /// reports a store state. `None` when the account does not exist.
    pub fn subscribe(&self, account: AccountId) -> Option<watch::Receiver<Option<WireState>>> {
        let key = self.core.key_of(account)?;
        Some(self.core.services.supervisor.watch_store_state(key))
    }

    /// The services the gate was built from.
    pub fn services(&self) -> &GateServices {
        &self.core.services
    }

    /// One look at every person whose key the gate put into memory, at the time `now`: whoever has no live session any more
    /// loses the key and gets one `secrets.lock` line. This is the function the interval task calls; tests call it too.
    /// Answers how many keys it wiped.
    pub async fn sweep_locks(&self, now: i64) -> usize {
        self.core.clone().look(now, Trigger::Sweep).await
    }
}

impl Core {
    /// The key of the worker of `account`, from the account row; `None` when there is no such account.
    fn key_of(&self, account: AccountId) -> Option<AccountKey> {
        let found = self.services.store.account_get(account).ok().flatten()?;
        Some(AccountKey { id: found.id, uid: found.uid })
    }

    async fn key_of_async(&self, account: AccountId) -> Option<AccountKey> {
        let store = self.services.store.clone();
        let found = tokio::task::spawn_blocking(move || store.account_get(account)).await.ok()?.ok()??;
        Some(AccountKey { id: found.id, uid: found.uid })
    }

    /// The lock of one account. Locks nobody holds are dropped when another one is taken, so the table stays as small as the
    /// number of people who are busy.
    fn lock_of(&self, account: AccountId) -> Arc<tokio::sync::Mutex<()>> {
        let mut table = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        table.retain(|_, lock| Arc::strong_count(lock) > 1);
        table.entry(account.get()).or_default().clone()
    }

    /// One control to the worker of `account`, with the answer, bounded in time. The worker is started when none runs.
    async fn control(&self, account: AccountKey, control: Ctl) -> Option<CtlOutcome> {
        match tokio::time::timeout(self.wait, self.services.supervisor.ctl(account, control)).await {
            Ok(Ok(outcome)) => Some(outcome),
            Ok(Err(_)) | Err(_) => None,
        }
    }

    fn open_set(&self) -> std::sync::MutexGuard<'_, HashSet<i64>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The gate put a key into the memory of the worker of `account`.
    fn mark_open(&self, account: AccountId) {
        self.open_set().insert(account.get());
    }

    /// The key of `account` is gone; answers whether the gate had it on record, so that exactly one caller records its end.
    fn take_open(&self, account: AccountId) -> bool {
        self.open_set().remove(&account.get())
    }

    /// Notes that a control left the store of `account` open (a store that was made by a change of a slot is open).
    fn note_state(&self, who: AccountKey) {
        if self.services.supervisor.store_state(who) == Some(WireState::Unlocked) {
            self.mark_open(who.id);
        }
    }

    /// Writes one audit line. A line that cannot be written is logged by name and changes nothing else.
    async fn record(&self, event: Event) {
        let audit = self.services.audit.clone();
        let written = tokio::task::spawn_blocking(move || audit.record(&event)).await;
        if !matches!(written, Ok(Ok(()))) {
            log::event(Level::Error, "gate.audit_failed", &[]);
        }
    }

    async fn unlock(&self, account: AccountId, kind: SlotKind, key: Key) -> Result<CtlOutcome, GateError> {
        self.unlock_or_create(account, kind, key, None).await
    }

    /// Opens the store with `key`. With `creation` set, a store that does not exist is made with that one slot and left open
    /// (the sign-in of a person who has none); a store that exists is never replaced, whatever the answer.
    async fn unlock_or_create(
        &self,
        account: AccountId,
        kind: SlotKind,
        key: Key,
        creation: Option<SlotKey>,
    ) -> Result<CtlOutcome, GateError> {
        let Some(who) = self.key_of_async(account).await else { return Err(GateError::Unavailable) };
        let lock = self.lock_of(account);
        let _held = lock.lock().await;
        let answer = self.control(who, Ctl::Unlock { ctl: 0, kind, key }).await.ok_or(GateError::Unavailable)?;
        match (answer, creation) {
            (CtlOutcome::Absent, Some(slot)) => {
                // `Provision` refuses when a store exists, so even a store that appeared meanwhile stays what it is.
                let made = self.control(who, Ctl::Provision { ctl: 0, slots: vec![slot] }).await.ok_or(GateError::Unavailable)?;
                if made == CtlOutcome::Done {
                    self.mark_open(account);
                }
                Ok(made)
            }
            (CtlOutcome::Done, _) => {
                self.mark_open(account);
                Ok(CtlOutcome::Done)
            }
            (other, _) => Ok(other),
        }
    }

    async fn explicit(&self, account: AccountId, secret: UnlockSecret<'_>) -> Result<(), GateError> {
        let (action, word) = match secret {
            UnlockSecret::Passphrase(_) => (Action::SecretsUnlock, "passphrase"),
            UnlockSecret::RecoveryCode(_) => (Action::RecoveryUse, "recovery_code"),
        };
        let verdict = self.explicit_unaudited(account, secret).await;
        let result = if verdict.is_ok() { Outcome::Ok } else { Outcome::Fail };
        self.record(
            Event::new(action, result, Actor::Account(account), None).target(account).detail(DetailKey::Method, DetailValue::Text(word)),
        )
        .await;
        verdict
    }

    async fn explicit_unaudited(&self, account: AccountId, secret: UnlockSecret<'_>) -> Result<(), GateError> {
        let method_kind = match secret {
            UnlockSecret::Passphrase(_) => UnlockKind::Passphrase,
            UnlockSecret::RecoveryCode(_) => UnlockKind::Recovery,
        };
        let store = self.services.store.clone();
        let row = tokio::task::spawn_blocking(move || store.read(|tx| tx.unlock_list(account)))
            .await
            .map_err(|_| GateError::Unavailable)?
            .map_err(|_| GateError::Unavailable)?
            .into_iter()
            .find(|row| row.kind == method_kind)
            .ok_or(GateError::Refused)?;
        let key = match secret {
            UnlockSecret::Passphrase(text) => {
                let kdf = parse_params(&row.params).map_err(|_| GateError::Refused)?;
                let salt: [u8; SALT_BYTES] = row.salt.as_slice().try_into().map_err(|_| GateError::Refused)?;
                let kek = self.services.argon.derive_passphrase(text, salt, kdf).await.map_err(|error| match error {
                    PasswordError::Busy | PasswordError::Derive | PasswordError::Store(_) => GateError::Unavailable,
                    _ => GateError::Refused,
                })?;
                Key::from_bytes(kek.expose())
            }
            UnlockSecret::RecoveryCode(text) => {
                let code = parse_recovery_code(text).map_err(|_| GateError::Refused)?;
                let kek = kek_from_recovery(&code, &row.salt).map_err(|_| GateError::Refused)?;
                Key::from_bytes(kek.expose())
            }
        };
        match self.unlock(account, slot_kind(method_kind), key).await? {
            CtlOutcome::Done => Ok(()),
            CtlOutcome::Locked | CtlOutcome::Refused | CtlOutcome::Absent => Err(GateError::Refused),
            CtlOutcome::Failed => Err(GateError::Unavailable),
        }
    }

    // ---- the policy: when the key leaves memory ----

    /// Starts the task that looks at the sessions whenever one is ended and every [`LOCK_SWEEP_INTERVAL`]. It holds the gate
    /// weakly, so it ends with it. Outside a runtime nothing is started (`sweep_locks` still works).
    fn start_policy(core: &Arc<Core>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let weak = Arc::downgrade(core);
        let mut ended = core.services.sessions.subscribe_ended();
        let clock = core.services.clock.clone();
        runtime.spawn(async move {
            let mut tick = tokio::time::interval(LOCK_SWEEP_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick is immediate and means nothing.
            tick.tick().await;
            loop {
                let trigger = tokio::select! {
                    got = ended.recv() => match got {
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                            // Several announcements at once are one look.
                            while !matches!(ended.try_recv(), Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)) {}
                            Trigger::Ended
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = tick.tick() => Trigger::Sweep,
                };
                let Some(core) = weak.upgrade() else { break };
                core.look(clock.unix(), trigger).await;
            }
        });
    }

    /// Looks at every account whose key the gate holds on record. Answers how many keys it wiped.
    async fn look(self: Arc<Core>, now: i64, trigger: Trigger) -> usize {
        let ids: Vec<i64> = self.open_set().iter().copied().collect();
        let mut jobs = JoinSet::new();
        for id in ids {
            let core = self.clone();
            jobs.spawn(async move { core.lock_if_last(AccountId::from_raw(id), trigger, now).await });
        }
        let mut wiped = 0;
        while let Some(done) = jobs.join_next().await {
            if done.unwrap_or(false) {
                wiped += 1;
            }
        }
        wiped
    }

    /// The cause of a wipe, from what the store of accounts and sessions says now.
    async fn cause_of(&self, account: AccountId, trigger: Trigger) -> (&'static str, bool) {
        let store = self.services.store.clone();
        let read = tokio::task::spawn_blocking(move || {
            store.read(|tx| Ok((tx.account_get(account)?, tx.session_list(account)?.len())))
        })
        .await;
        match read {
            Ok(Ok((found, rows))) => {
                let active = matches!(found, Some(ref a) if a.state == AccountState::Active);
                if !active {
                    (CAUSE_DISABLED, false)
                } else if rows > 0 || trigger == Trigger::Sweep {
                    // Rows are left, so the sessions ran out by time and nobody ended them.
                    (CAUSE_EXPIRED, true)
                } else {
                    (CAUSE_SIGNOUT, true)
                }
            }
            // A store that cannot be read does not decide anything: the account is taken as active and the sessions as ended.
            _ => (CAUSE_SIGNOUT, true),
        }
    }

    /// The decision to lock `account`, taken inside its lock after counting its live sessions again at `now`. Answers whether
    /// this call wiped the key.
    async fn lock_if_last(&self, account: AccountId, trigger: Trigger, now: i64) -> bool {
        let Some(who) = self.key_of_async(account).await else {
            // No such account: nothing can be locked and nothing is on record.
            self.take_open(account);
            return false;
        };
        let lock = self.lock_of(account);
        let _held = lock.lock().await;
        if self.services.supervisor.store_state(who) != Some(WireState::Unlocked) {
            // The key is not in memory (the worker ended or the store was locked another way). An account that ended
            // takes its key with the worker, and that is recorded once; for any other account there is nothing to say.
            if self.open_set().contains(&account.get()) {
                let (cause, active) = self.cause_of(account, trigger).await;
                let on_record = self.take_open(account);
                if on_record && !active {
                    self.record_lock(account, cause).await;
                }
            }
            return false;
        }
        let sessions = self.services.sessions.clone();
        let live = tokio::task::spawn_blocking(move || sessions.list_live(account, now)).await;
        match live {
            Ok(Ok(live)) if live.is_empty() => {}
            // A live session, or a store that cannot say: the key stays.
            _ => return false,
        }
        let (cause, _) = self.cause_of(account, trigger).await;
        match self.control(who, Ctl::Lock { ctl: 0 }).await {
            Some(CtlOutcome::Done) => {}
            Some(CtlOutcome::Absent) => {
                self.take_open(account);
                return false;
            }
            // A lock the worker did not take (it failed or timed out) is tried again at the next look.
            Some(_) | None => return false,
        }
        // A worker that ended while the frame was on its way took the key with it and has nothing to record.
        if !matches!(self.services.supervisor.store_state(who), Some(WireState::Locked | WireState::Absent)) {
            self.take_open(account);
            return false;
        }
        if self.take_open(account) {
            self.record_lock(account, cause).await;
            return true;
        }
        false
    }

    async fn record_lock(&self, account: AccountId, cause: &'static str) {
        self.record(
            Event::new(Action::SecretsLock, Outcome::Ok, Actor::Account(account), None)
                .target(account)
                .detail(DetailKey::Reason, DetailValue::Text(cause)),
        )
        .await;
    }
}

impl StoreView for SupervisorGate {
    fn state(&self, account: AccountId) -> StoreState {
        let Some(key) = self.core.key_of(account) else { return StoreState::Locked };
        match self.core.services.supervisor.store_state(key) {
            Some(WireState::Unlocked) => StoreState::Open,
            Some(WireState::Absent | WireState::Locked) | None => StoreState::Locked,
        }
    }
}

impl UnlockGate for SupervisorGate {
    fn provision(&self, account: AccountId, keys: Vec<ProvisionKey>) -> GateFuture<'_, Result<(), GateError>> {
        Box::pin(async move {
            if keys.is_empty() {
                return Ok(());
            }
            let slots: Vec<SlotKey> = keys.iter().map(slot_key).collect();
            drop(keys);
            let core = &self.core;
            let who = core.key_of_async(account).await.ok_or(GateError::Unavailable)?;
            let lock = core.lock_of(account);
            let _held = lock.lock().await;
            match core.control(who, Ctl::Provision { ctl: 0, slots }).await {
                Some(CtlOutcome::Done) => {
                    core.mark_open(account);
                    Ok(())
                }
                Some(CtlOutcome::Refused | CtlOutcome::Locked) => Err(GateError::Refused),
                Some(CtlOutcome::Failed | CtlOutcome::Absent) | None => Err(GateError::Unavailable),
            }
        })
    }

    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>> {
        Box::pin(async move {
            let (kind, key, creation) = match &material {
                Material::Password(m) => {
                    let slot = SlotKey {
                        kind: SlotKind::Password,
                        id: slot_id(UnlockKind::Password, None),
                        key: Key::from_bytes(m.kek().expose()),
                    };
                    (SlotKind::Password, Key::from_bytes(m.kek().expose()), slot)
                }
                Material::Passkey(m) => match kek_from_prf(m.output().expose(), m.salt()) {
                    Ok(kek) => {
                        let slot = SlotKey {
                            kind: SlotKind::Prf,
                            id: slot_id(UnlockKind::Prf, Some(m.credential_id())),
                            key: Key::from_bytes(kek.expose()),
                        };
                        (SlotKind::Prf, Key::from_bytes(kek.expose()), slot)
                    }
                    // A salt the derivation cannot use: nothing opens the store, and the sign-in goes on.
                    Err(_) => return Ok(()),
                },
                // Single sign-on carries no key: nothing opens the store and nothing is made.
                Material::None => return Ok(()),
            };
            drop(material);
            // Whatever the worker answers, the sign-in goes on; the store stays locked and the page shows the unlock dialog.
            // A store that does not exist is made with this key; one that exists is only ever opened.
            self.core.unlock_or_create(account, kind, key, Some(creation)).await.map(|_| ())
        })
    }

    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>> {
        Box::pin(async move { self.core.explicit(account, secret).await })
    }
}

fn secrets_error(outcome: Option<CtlOutcome>) -> Result<(), SecretsError> {
    match outcome {
        Some(CtlOutcome::Done) => Ok(()),
        Some(CtlOutcome::Locked) => Err(SecretsError::Locked),
        Some(CtlOutcome::Refused | CtlOutcome::Absent) => Err(SecretsError::Refused),
        Some(CtlOutcome::Failed) | None => Err(SecretsError::Unavailable),
    }
}

impl SecretsAdmin for SupervisorGate {
    fn set_slot(
        &self,
        account: AccountId,
        key: ProvisionKey,
        via: Option<ProvisionKey>,
    ) -> GateFuture<'_, Result<(), SecretsError>> {
        Box::pin(async move {
            let slot = slot_key(&key);
            let opening = via.as_ref().map(|via| OpeningKey { kind: slot_kind(via.kind()), key: Key::from_bytes(via.key()) });
            drop((key, via));
            let core = &self.core;
            let who = core.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = core.lock_of(account);
            let _held = lock.lock().await;
            let answer = core.control(who, Ctl::SetSlot { ctl: 0, slot, opening }).await;
            if answer == Some(CtlOutcome::Done) {
                // A store made by this change is open.
                core.note_state(who);
            }
            secrets_error(answer)
        })
    }

    fn remove_slot(
        &self,
        account: AccountId,
        kind: UnlockKind,
        credential_id: Option<Vec<u8>>,
    ) -> GateFuture<'_, Result<(), SecretsError>> {
        Box::pin(async move {
            let core = &self.core;
            let who = core.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = core.lock_of(account);
            let _held = lock.lock().await;
            let control = Ctl::RemoveSlot { ctl: 0, kind: slot_kind(kind), id: slot_id(kind, credential_id.as_deref()) };
            secrets_error(core.control(who, control).await)
        })
    }

    fn reset(&self, account: AccountId) -> GateFuture<'_, Result<(), SecretsError>> {
        Box::pin(async move {
            let core = &self.core;
            let who = core.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = core.lock_of(account);
            let _held = lock.lock().await;
            match tokio::time::timeout(core.wait, core.services.supervisor.wipe_secrets(who)).await {
                Ok(Ok(())) => {
                    // The key left memory with the store; the route writes the one line of a reset.
                    core.take_open(account);
                    Ok(())
                }
                Ok(Err(_)) | Err(_) => Err(SecretsError::Unavailable),
            }
        })
    }
}

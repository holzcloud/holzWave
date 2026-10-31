//! The supervisor side of the secret store: the gate the sign-in routes tell about a new session, the view of the application
//! on whether a store is open, and the seam of the account routes that changes its slots.
//!
//! Each operation is one control frame to the worker of the person (started when none runs) and the worker's answer mapped to
//! its outcome, bounded in time by [`limits::GATE_WAIT`]. A key is the wiping type of the wire, built from the key the caller
//! handed over, sent once and never kept: the gate holds no key, prints none, logs none and writes no audit line. The policy
//! that decides when to lock, what to do with a missing store at sign-in and every `secrets.*` audit line belong to the layer
//! above this one; the audit writer is held here so that layer can use it without changing the constructor.
//!
//! One lock per account serialises everything that touches its store, so two unlocks of the same person end in one unlocked
//! state and a control never overtakes the one before it.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::registry::{AccountKey, Supervisor};
use crate::audit::Audit;
use crate::auth::password::{ArgonGate, PasswordError, parse_params};
use crate::auth::session::SessionManager;
use crate::clock::Clock;
use crate::http::app::{StoreState, StoreView};
use crate::http::routes_me::{SecretsAdmin, SecretsError};
use crate::http::routes_unlock::{GateError, GateFuture, Material, ProvisionKey, UnlockGate, UnlockSecret};
use crate::limits;
use crate::store::{AccountId, Store, UnlockKind};
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

/// The gate: the unlock gate of the sign-in routes, the store view of the application and the secrets seam of the account
/// routes, over the supervisor.
pub struct SupervisorGate {
    services: GateServices,
    wait: Duration,
    locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
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
    /// The gate over `services`, waiting at most [`limits::GATE_WAIT`] for any answer.
    pub fn new(services: GateServices) -> SupervisorGate {
        SupervisorGate::with_wait(services, limits::GATE_WAIT)
    }

    /// The same with another bound on the wait.
    pub fn with_wait(services: GateServices, wait: Duration) -> SupervisorGate {
        SupervisorGate { services, wait, locks: Mutex::new(HashMap::new()) }
    }

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

    /// What the worker last said about the store of `account`, as a watch channel: the value changes whenever the worker
    /// reports a store state. `None` when the account does not exist.
    pub fn subscribe(&self, account: AccountId) -> Option<watch::Receiver<Option<WireState>>> {
        let key = self.key_of(account)?;
        Some(self.services.supervisor.watch_store_state(key))
    }

    /// The services the gate was built from.
    pub fn services(&self) -> &GateServices {
        &self.services
    }

    async fn unlock(&self, account: AccountId, kind: SlotKind, key: Key) -> Result<CtlOutcome, GateError> {
        let Some(who) = self.key_of_async(account).await else { return Err(GateError::Unavailable) };
        let lock = self.lock_of(account);
        let _held = lock.lock().await;
        self.control(who, Ctl::Unlock { ctl: 0, kind, key }).await.ok_or(GateError::Unavailable)
    }

    async fn explicit(&self, account: AccountId, secret: UnlockSecret<'_>) -> Result<(), GateError> {
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
}

impl StoreView for SupervisorGate {
    fn state(&self, account: AccountId) -> StoreState {
        let Some(key) = self.key_of(account) else { return StoreState::Locked };
        match self.services.supervisor.store_state(key) {
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
            let who = self.key_of_async(account).await.ok_or(GateError::Unavailable)?;
            let lock = self.lock_of(account);
            let _held = lock.lock().await;
            match self.control(who, Ctl::Provision { ctl: 0, slots }).await {
                Some(CtlOutcome::Done) => Ok(()),
                Some(CtlOutcome::Refused | CtlOutcome::Locked) => Err(GateError::Refused),
                Some(CtlOutcome::Failed | CtlOutcome::Absent) | None => Err(GateError::Unavailable),
            }
        })
    }

    fn session_started(&self, account: AccountId, material: Material) -> GateFuture<'_, Result<(), GateError>> {
        Box::pin(async move {
            let (kind, key) = match &material {
                Material::Password(m) => (SlotKind::Password, Key::from_bytes(m.kek().expose())),
                Material::Passkey(m) => match kek_from_prf(m.output().expose(), m.salt()) {
                    Ok(kek) => (SlotKind::Prf, Key::from_bytes(kek.expose())),
                    // A salt the derivation cannot use: nothing opens the store, and the sign-in goes on.
                    Err(_) => return Ok(()),
                },
                Material::None => return Ok(()),
            };
            drop(material);
            // Whatever the worker answers, the sign-in goes on; the store stays locked and the page shows the unlock dialog.
            self.unlock(account, kind, key).await.map(|_| ())
        })
    }

    fn unlock_with<'a>(&'a self, account: AccountId, secret: UnlockSecret<'a>) -> GateFuture<'a, Result<(), GateError>> {
        Box::pin(async move { self.explicit(account, secret).await })
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
            let who = self.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = self.lock_of(account);
            let _held = lock.lock().await;
            secrets_error(self.control(who, Ctl::SetSlot { ctl: 0, slot, opening }).await)
        })
    }

    fn remove_slot(
        &self,
        account: AccountId,
        kind: UnlockKind,
        credential_id: Option<Vec<u8>>,
    ) -> GateFuture<'_, Result<(), SecretsError>> {
        Box::pin(async move {
            let who = self.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = self.lock_of(account);
            let _held = lock.lock().await;
            let control = Ctl::RemoveSlot { ctl: 0, kind: slot_kind(kind), id: slot_id(kind, credential_id.as_deref()) };
            secrets_error(self.control(who, control).await)
        })
    }

    fn reset(&self, account: AccountId) -> GateFuture<'_, Result<(), SecretsError>> {
        Box::pin(async move {
            let who = self.key_of_async(account).await.ok_or(SecretsError::Unavailable)?;
            let lock = self.lock_of(account);
            let _held = lock.lock().await;
            match tokio::time::timeout(self.wait, self.services.supervisor.wipe_secrets(who)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) | Err(_) => Err(SecretsError::Unavailable),
            }
        })
    }
}

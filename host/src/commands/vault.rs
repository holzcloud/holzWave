//! The plain functions of the six commands that drive the vault: its status, the unlock, the lock, the switch of its
//! protection, the reset of a lost one, and the page's visibility.
//!
//! A host with no vault (the desktop, the server, a test) answers the status as `none` and refuses the other five with one
//! sentence, [`NO_VAULT`], and changes nothing. A host with a vault passes the vault's own status to the page. A failure
//! of the keystore never reaches the page as text: it becomes one of four fixed words (`cancelled`, `lost`,
//! `no_screen_lock`, `failed`), because a platform message may be about the device and the page matches the words. The
//! answers hold no key and no secret.
//!
//! `vault_unlock` and `vault_protect` may wait for the person's finger or face, so their rows run off the main thread; the
//! vault holds no lock of its own state while it waits, so `vault_status` answers at once meanwhile.

use holzwave_core::vault::{Protection, VaultError, VaultStatus};
use serde::Serialize;

use crate::state::AppCore;

/// What every vault command except the status answers on a host that has no vault.
pub const NO_VAULT: &str = "no secret vault on this device";

/// The vault's status as the page reads it: words, no numbers, no secret. The names are the ones of the core's status plus
/// the state `none` for a host that has no vault.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VaultStatusMsg {
    /// `none`, `new`, `locked`, `unlocked` or `lost`.
    pub state: &'static str,
    /// `plain` or `auth`.
    pub protection: &'static str,
    /// The device has a screen lock.
    pub has_screen_lock: bool,
    pub strongbox: bool,
    /// A plain vault can be switched to `auth` now.
    pub can_protect: bool,
    /// `none` or `no_lock` (the device has no screen lock, so the vault is protected by the device key alone).
    pub notice: &'static str,
    /// Why the vault is lost: `key_gone` or `corrupt`.
    pub reason: Option<&'static str>,
}

impl VaultStatusMsg {
    /// The status of a host that has no vault.
    pub fn none() -> VaultStatusMsg {
        VaultStatusMsg {
            state: "none",
            protection: Protection::Plain.as_str(),
            has_screen_lock: false,
            strongbox: false,
            can_protect: false,
            notice: "none",
            reason: None,
        }
    }
}

impl From<VaultStatus> for VaultStatusMsg {
    fn from(status: VaultStatus) -> VaultStatusMsg {
        VaultStatusMsg {
            state: status.state.as_str(),
            protection: status.protection.as_str(),
            has_screen_lock: status.has_screen_lock,
            strongbox: status.strongbox,
            can_protect: status.can_protect,
            notice: status.notice.as_str(),
            reason: status.reason.map(|reason| reason.as_str()),
        }
    }
}

/// The fixed word of a failed vault operation. The text of the failure is dropped on purpose.
fn word(error: &VaultError) -> String {
    match error {
        VaultError::Cancelled => "cancelled",
        VaultError::Lost(_) => "lost",
        VaultError::NoScreenLock => "no_screen_lock",
        VaultError::Failed(_) => "failed",
    }
    .to_owned()
}

fn vault_of(core: &AppCore) -> Result<std::sync::Arc<holzwave_core::vault::VaultStore>, String> {
    core.host().vault().ok_or_else(|| NO_VAULT.to_owned())
}

/// The state of the vault. Never prompts and never waits for a prompt.
pub fn vault_status(core: &AppCore) -> VaultStatusMsg {
    core.host().vault().map_or_else(VaultStatusMsg::none, |vault| vault.status().into())
}

/// Unlock the vault (create it when it is new). Waits for the system prompt; a paused sync starts one run when it worked.
/// Fails with `cancelled`, `lost`, `no_screen_lock` or `failed`.
pub fn vault_unlock(core: &AppCore) -> Result<VaultStatusMsg, String> {
    let vault = vault_of(core)?;
    vault.unlock().map_err(|error| word(&error))?;
    core.vault_unlocked();
    Ok(vault.status().into())
}

/// Lock the vault at once. Open sessions are not touched; what needs a secret asks for the unlock again.
pub fn vault_lock(core: &AppCore) -> Result<VaultStatusMsg, String> {
    let vault = vault_of(core)?;
    vault.lock();
    Ok(vault.status().into())
}

/// Switch the protection of the vault: `true` asks for the screen lock at every unlock, `false` keeps the vault under the
/// device key alone. Fails with `no_screen_lock` when `true` is asked on a device that has none.
pub fn vault_protect(core: &AppCore, on: bool) -> Result<VaultStatusMsg, String> {
    let vault = vault_of(core)?;
    vault.set_protection(if on { Protection::Auth } else { Protection::Plain }).map_err(|error| word(&error))?;
    Ok(vault.status().into())
}

/// Throw away a lost vault and start a new one. It only deletes: it never reveals anything.
pub fn vault_reset(core: &AppCore) -> Result<VaultStatusMsg, String> {
    let vault = vault_of(core)?;
    vault.reset().map_err(|error| word(&error))?;
    Ok(vault.status().into())
}

/// The page was hidden (`true`) or shown again (`false`): an unlocked vault with protection `auth` locks after five
/// minutes hidden.
pub fn vault_hidden(core: &AppCore, hidden: bool) -> Result<(), String> {
    vault_of(core)?.hidden(hidden);
    Ok(())
}

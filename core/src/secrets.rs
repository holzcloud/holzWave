//! Where passwords and key passphrases live: the operating system's keychain, never a file of ours.
//!
//! [`SecretStore`] is the seam. [`MemoryStore`] is for tests and for a session that must not touch the keychain.
//! [`KeychainStore`] (feature `keyring`) is the macOS Keychain, the Windows Credential Manager or the Secret
//! Service on Linux. A secret is addressed by an account string such as `ssh:<profile id>`; the service name is
//! fixed, so holzWave never reads another application's entries.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use crate::{Error, Result};

/// The service name every entry of holzWave is filed under.
pub const SERVICE: &str = "ch.holzcloud.holzwave";

/// The longest account and secret that are accepted; far more than anyone types.
pub const MAX_LEN: usize = 4096;

pub trait SecretStore: Send + Sync {
    /// The secret for `account`, `None` if there is none.
    fn get(&self, account: &str) -> Result<Option<String>>;
    /// Store (or replace) the secret for `account`.
    fn set(&self, account: &str, secret: &str) -> Result<()>;
    /// Remove the secret for `account`. Removing one that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<()>;
}

fn check(account: &str, secret: Option<&str>) -> Result<()> {
    if account.is_empty() || account.len() > MAX_LEN || account.chars().any(char::is_control) {
        return Err(Error::Unsupported("the account of a secret is empty, too long or has a control character".into()));
    }
    if secret.is_some_and(|s| s.len() > MAX_LEN) {
        return Err(Error::Unsupported(format!("a secret is longer than {MAX_LEN} bytes")));
    }
    Ok(())
}

/// Secrets in memory only, gone when the process ends.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        check(account, None)?;
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        check(account, Some(secret))?;
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        check(account, None)?;
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(account);
        Ok(())
    }
}

/// The operating system's keychain.
#[cfg(feature = "keyring")]
pub struct KeychainStore;

#[cfg(feature = "keyring")]
impl KeychainStore {
    fn entry(account: &str) -> Result<keyring::Entry> {
        check(account, None)?;
        keyring::Entry::new(SERVICE, account).map_err(|e| Error::Unsupported(format!("keychain: {e}")))
    }
}

#[cfg(feature = "keyring")]
impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Unsupported(format!("keychain: {e}"))),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<()> {
        check(account, Some(secret))?;
        Self::entry(account)?.set_password(secret).map_err(|e| Error::Unsupported(format!("keychain: {e}")))
    }

    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Unsupported(format!("keychain: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_stored_replaced_and_removed() {
        let store = MemoryStore::default();
        assert_eq!(store.get("ssh:a").unwrap(), None);
        store.set("ssh:a", "one").unwrap();
        store.set("ssh:a", "two").unwrap();
        assert_eq!(store.get("ssh:a").unwrap().as_deref(), Some("two"));
        store.delete("ssh:a").unwrap();
        store.delete("ssh:a").unwrap();
        assert_eq!(store.get("ssh:a").unwrap(), None);
    }

    #[test]
    fn a_bad_account_or_a_huge_secret_is_refused() {
        let store = MemoryStore::default();
        assert!(store.set("", "x").is_err());
        assert!(store.set("a\nb", "x").is_err());
        assert!(store.set("ssh:a", &"x".repeat(MAX_LEN + 1)).is_err());
    }
}

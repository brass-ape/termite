// SPDX-License-Identifier: MIT
//! `CredentialStore` implementations: `KeyringStore` (the real OS keychain)
//! and `MemoryStore` (for tests).

use std::collections::HashMap;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

use termite_core::{CredentialStore, TermiteError};

use crate::error::StorageError;

const SERVICE: &str = "termite";

fn password_key(host: &str, user: &str) -> String {
    format!("{host}:{user}")
}

fn passphrase_key(fingerprint: &str) -> String {
    format!("key:{fingerprint}")
}

/// A [`CredentialStore`] backed by the OS keychain via the `keyring` crate
/// (Keychain Services on macOS, Credential Manager on Windows, Secret
/// Service on Linux). Passwords and passphrases never touch disk.
#[derive(Default)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>, TermiteError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(StorageError::Keyring)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(SecretString::from(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(StorageError::Keyring(err).into()),
        }
    }

    fn set(&self, key: &str, value: &SecretString) -> Result<(), TermiteError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(StorageError::Keyring)?;
        entry
            .set_password(value.expose_secret())
            .map_err(|err| StorageError::Keyring(err).into())
    }

    fn delete(&self, key: &str) -> Result<(), TermiteError> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(StorageError::Keyring)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(StorageError::Keyring(err).into()),
        }
    }
}

impl CredentialStore for KeyringStore {
    fn get_password(&self, host: &str, user: &str) -> Result<Option<SecretString>, TermiteError> {
        self.get(&password_key(host, user))
    }

    fn set_password(
        &self,
        host: &str,
        user: &str,
        password: &SecretString,
    ) -> Result<(), TermiteError> {
        self.set(&password_key(host, user), password)
    }

    fn delete_password(&self, host: &str, user: &str) -> Result<(), TermiteError> {
        self.delete(&password_key(host, user))
    }

    fn get_passphrase(&self, fingerprint: &str) -> Result<Option<SecretString>, TermiteError> {
        self.get(&passphrase_key(fingerprint))
    }

    fn set_passphrase(
        &self,
        fingerprint: &str,
        passphrase: &SecretString,
    ) -> Result<(), TermiteError> {
        self.set(&passphrase_key(fingerprint), passphrase)
    }
}

/// An in-memory [`CredentialStore`] for tests — never touches the OS
/// keychain. Entries are plain `String`s rather than `SecretString`
/// (`SecretString` isn't `Clone`, and this is a test double, not a path
/// secrets flow through in production).
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, key: &str) -> Option<SecretString> {
        self.entries
            .lock()
            .expect("MemoryStore mutex poisoned")
            .get(key)
            .cloned()
            .map(SecretString::from)
    }

    fn set(&self, key: &str, value: &SecretString) {
        self.entries
            .lock()
            .expect("MemoryStore mutex poisoned")
            .insert(key.to_string(), value.expose_secret().to_string());
    }

    fn delete(&self, key: &str) {
        self.entries
            .lock()
            .expect("MemoryStore mutex poisoned")
            .remove(key);
    }
}

impl CredentialStore for MemoryStore {
    fn get_password(&self, host: &str, user: &str) -> Result<Option<SecretString>, TermiteError> {
        Ok(self.get(&password_key(host, user)))
    }

    fn set_password(
        &self,
        host: &str,
        user: &str,
        password: &SecretString,
    ) -> Result<(), TermiteError> {
        self.set(&password_key(host, user), password);
        Ok(())
    }

    fn delete_password(&self, host: &str, user: &str) -> Result<(), TermiteError> {
        self.delete(&password_key(host, user));
        Ok(())
    }

    fn get_passphrase(&self, fingerprint: &str) -> Result<Option<SecretString>, TermiteError> {
        Ok(self.get(&passphrase_key(fingerprint)))
    }

    fn set_passphrase(
        &self,
        fingerprint: &str,
        passphrase: &SecretString,
    ) -> Result<(), TermiteError> {
        self.set(&passphrase_key(fingerprint), passphrase);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        let password = SecretString::from("hunter2".to_string());

        assert!(store
            .get_password("example.com", "alice")
            .unwrap()
            .is_none());

        store
            .set_password("example.com", "alice", &password)
            .unwrap();
        let retrieved = store.get_password("example.com", "alice").unwrap().unwrap();
        assert_eq!(retrieved.expose_secret(), "hunter2");

        store.delete_password("example.com", "alice").unwrap();
        assert!(store
            .get_password("example.com", "alice")
            .unwrap()
            .is_none());
    }

    #[test]
    fn memory_store_passphrase_round_trip() {
        let store = MemoryStore::new();
        let passphrase = SecretString::from("correct horse battery staple".to_string());

        store.set_passphrase("SHA256:abc123", &passphrase).unwrap();
        let retrieved = store.get_passphrase("SHA256:abc123").unwrap().unwrap();
        assert_eq!(retrieved.expose_secret(), "correct horse battery staple");
    }
}

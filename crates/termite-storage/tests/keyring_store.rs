// SPDX-License-Identifier: MIT
//! Integration test against the *real* OS keychain (not a mock) — this
//! environment has `gnome-keyring-daemon` running with `org.freedesktop.secrets`
//! on the session D-Bus, so `KeyringStore` can be verified for real instead
//! of only through the `MemoryStore` double. Uses randomized keys so it
//! can't collide with anything real, and cleans up after itself.

use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::{ExposeSecret, SecretString};

use termite_core::CredentialStore;
use termite_storage::KeyringStore;

fn unique(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    format!("termite-test-{label}-{nanos}")
}

#[test]
fn keyring_store_password_round_trip() {
    let store = KeyringStore::new();
    let host = unique("host");
    let user = "test-user";
    let password = SecretString::from("hunter2-real-keychain-test".to_string());

    assert!(store.get_password(&host, user).unwrap().is_none());

    store.set_password(&host, user, &password).unwrap();
    let retrieved = store.get_password(&host, user).unwrap().unwrap();
    assert_eq!(retrieved.expose_secret(), "hunter2-real-keychain-test");

    store.delete_password(&host, user).unwrap();
    assert!(store.get_password(&host, user).unwrap().is_none());
}

#[test]
fn keyring_store_passphrase_round_trip() {
    let store = KeyringStore::new();
    let fingerprint = unique("fingerprint");
    let passphrase = SecretString::from("correct horse battery staple".to_string());

    assert!(store.get_passphrase(&fingerprint).unwrap().is_none());

    store.set_passphrase(&fingerprint, &passphrase).unwrap();
    let retrieved = store.get_passphrase(&fingerprint).unwrap().unwrap();
    assert_eq!(retrieved.expose_secret(), "correct horse battery staple");

    // `CredentialStore` has no `delete_passphrase` (matches ARCHITECTURE.md's
    // trait sketch), so clean up directly through `keyring` the same way
    // `KeyringStore` composes its entry key internally.
    let entry = keyring::Entry::new("termite", &format!("key:{fingerprint}")).unwrap();
    entry.delete_credential().unwrap();
}

#[test]
fn missing_entry_is_none_not_error() {
    let store = KeyringStore::new();
    let host = unique("never-set");

    assert!(store.get_password(&host, "nobody").unwrap().is_none());
}

// SPDX-License-Identifier: MIT
//! Configuration and credential storage for Termite.
//!
//! Manages host profiles, app settings, known hosts, recent connections,
//! and the OS keychain integration for passwords and key passphrases.
//! M3 implements the `CredentialStore` side (`KeyringStore`/`MemoryStore`);
//! host profile/settings TOML persistence is still a later milestone.
//!
//! Config directory:
//! - Linux:   `~/.config/termite/`
//! - macOS:   `~/Library/Application Support/termite/`
//! - Windows: `%APPDATA%\termite\`

pub mod credential_store;
pub mod error;

pub use credential_store::{KeyringStore, MemoryStore};
pub use error::StorageError;

// SPDX-License-Identifier: MIT
//! Configuration and credential storage for Termite.
//!
//! Manages host profiles, app settings, known hosts, recent connections,
//! and the OS keychain integration for passwords and key passphrases.
//! M3 implements the `CredentialStore` side (`KeyringStore`/`MemoryStore`).
//! M4 adds the `HostStore` side (`TomlHostStore`/`MemoryHostStore`) for
//! host profile persistence. Settings TOML persistence is still pending.
//!
//! Config directory:
//! - Linux:   `~/.config/termite/`
//! - macOS:   `~/Library/Application Support/termite/`
//! - Windows: `%APPDATA%\termite\`

pub mod credential_store;
pub mod error;
pub mod host_store;

pub use credential_store::{KeyringStore, MemoryStore};
pub use error::StorageError;
pub use host_store::{HostStore, MemoryHostStore, TomlHostStore};

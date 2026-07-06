// SPDX-License-Identifier: MIT
//! Cryptographic key management for Termite.
//!
//! Handles loading, decrypting, generating, and saving SSH key pairs, and
//! provides `LocalKeyProvider`, a `termite_core::KeyProvider` backed by an
//! in-memory decrypted key.
//!
//! # Security invariants
//!
//! - Passphrases are always `secrecy::SecretString`, never plain `String`.
//! - Private key material is held in `ssh_key::PrivateKey`, which already
//!   zeroes its own memory on drop (verified against its source — see
//!   `HANDOFF.md`) — not re-wrapped in our own `SecretVec` on top of that.
//! - Passphrases are NEVER logged. The `secrecy` crate enforces this via
//!   its `Debug` implementation (prints `[REDACTED]`).

pub mod error;
pub mod key;
pub mod provider;

pub use error::CryptoError;
pub use provider::LocalKeyProvider;

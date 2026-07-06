// SPDX-License-Identifier: MIT
//! Cryptographic key management for Termite.
//!
//! Handles loading, decrypting, and generating SSH key pairs.
//! All key material is wrapped in `secrecy::Secret<T>` and zeroed on drop.
//! Implemented in M3.
//!
//! Key crates added in M3:
//! - `ssh-key` — SSH key format parsing/generation (RustCrypto, Apache-2.0/MIT)
//! - `rand`    — cryptographically secure randomness via OsRng
//!
//! # Security invariants
//!
//! - Private key bytes are NEVER stored in `Vec<u8>` or `String` directly.
//!   Always use `secrecy::SecretVec<u8>`.
//! - Key material is zeroed on drop via `zeroize`.
//! - Passphrases are NEVER logged. The `secrecy` crate enforces this via
//!   its `Debug` implementation (prints `[REDACTED]`).

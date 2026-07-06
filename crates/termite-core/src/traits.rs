// SPDX-License-Identifier: MIT

use secrecy::SecretString;

use crate::error::TermiteError;

/// Signs data on behalf of an SSH identity without exposing the private key
/// material to the caller — `termite-ssh` calls `.sign(data)` and never
/// touches raw key bytes directly (see `CLAUDE.md`'s architecture patterns).
///
/// Implemented by `termite-crypto` for keys loaded from disk. A future
/// SSH-agent-backed implementation would satisfy the same trait.
pub trait KeyProvider: Send + Sync {
    /// The SSH wire-format public key blob (algorithm name + key data,
    /// as encoded in the protocol) identifying this key.
    fn public_key_blob(&self) -> Vec<u8>;

    /// Signs `data`, returning an SSH wire-format signature blob
    /// (algorithm name + signature data).
    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, TermiteError>;
}

/// Stores and retrieves secrets (host passwords, key passphrases) in the
/// OS keychain. Implemented by `termite-storage`'s `KeyringStore`; a
/// `MemoryStore` implementation exists for tests.
///
/// Methods are synchronous — the underlying keychain APIs (and `termite-crypto`'s
/// signing) are synchronous/CPU-bound themselves, so an async trait would only
/// move where a `spawn_blocking` needs to happen, not remove the need for one.
/// Async callers should wrap calls at the call site if blocking the executor
/// would matter there.
pub trait CredentialStore: Send + Sync {
    fn get_password(&self, host: &str, user: &str) -> Result<Option<SecretString>, TermiteError>;
    fn set_password(
        &self,
        host: &str,
        user: &str,
        password: &SecretString,
    ) -> Result<(), TermiteError>;
    fn delete_password(&self, host: &str, user: &str) -> Result<(), TermiteError>;

    fn get_passphrase(&self, fingerprint: &str) -> Result<Option<SecretString>, TermiteError>;
    fn set_passphrase(
        &self,
        fingerprint: &str,
        passphrase: &SecretString,
    ) -> Result<(), TermiteError>;
}

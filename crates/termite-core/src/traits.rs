// SPDX-License-Identifier: MIT

use secrecy::SecretString;

use crate::error::TermiteError;

/// Hash algorithm for RSA signatures (`rsa-sha2-256` / `rsa-sha2-512`).
///
/// SSH servers advertise which RSA signature algorithms they accept via the
/// `server-sig-algs` extension, and the signature blob's algorithm name must
/// match what was negotiated — so the SSH layer has to be able to tell a
/// [`KeyProvider`] which hash to sign with. Irrelevant for non-RSA keys
/// (ed25519/ECDSA have exactly one signature scheme each). Deliberately our
/// own type: `termite-core` depends on no SSH crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsaHashAlg {
    /// `rsa-sha2-256`.
    Sha256,
    /// `rsa-sha2-512`.
    Sha512,
}

/// Signs data on behalf of an SSH identity without exposing the private key
/// material to the caller — `termite-ssh` calls `.sign(data, hash)` and never
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
    ///
    /// `hash` selects the RSA signature algorithm when the underlying key is
    /// RSA (`None` lets the implementation pick its default, SHA-512);
    /// implementations must ignore it for non-RSA keys.
    fn sign(&self, data: &[u8], hash: Option<RsaHashAlg>) -> Result<Vec<u8>, TermiteError>;
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
    fn delete_passphrase(&self, fingerprint: &str) -> Result<(), TermiteError>;
}

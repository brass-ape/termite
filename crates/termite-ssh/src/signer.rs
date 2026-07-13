// SPDX-License-Identifier: MIT
//! Adapts a `termite_core::KeyProvider` to `russh::auth::Signer`, so
//! `authenticate_publickey_with` can call it lazily — the key material
//! itself never crosses into `termite-ssh`, only signature bytes do.

use russh::keys::{agent::AgentIdentity, HashAlg};
use russh::Signer;

use termite_core::{KeyProvider, RsaHashAlg};

use crate::error::SshError;

pub struct KeyProviderSigner {
    provider: Box<dyn KeyProvider>,
}

impl KeyProviderSigner {
    pub fn new(provider: Box<dyn KeyProvider>) -> Self {
        Self { provider }
    }
}

impl Signer for KeyProviderSigner {
    type Error = SshError;

    async fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        // Only RSA keys carry a hash choice; translate to the SSH-crate-free
        // enum `termite-core` exposes. `HashAlg` is non-exhaustive, so fail
        // loudly on variants we don't know rather than mis-signing.
        let hash = match hash_alg {
            None => None,
            Some(HashAlg::Sha256) => Some(RsaHashAlg::Sha256),
            Some(HashAlg::Sha512) => Some(RsaHashAlg::Sha512),
            Some(other) => {
                return Err(SshError::KeyProvider(format!(
                    "unsupported RSA hash algorithm requested: {other:?}"
                )))
            }
        };
        // russh's `Signer` contract (per `AgentClient::sign_request`, the
        // reference implementation): return the *entire* `to_sign` buffer
        // with the signature appended as an SSH string —
        // `u32-length ++ string(algorithm) ++ string(signature-bytes)` —
        // not the bare signature. russh then slices the userauth packet
        // out of the returned buffer; returning only signature bytes makes
        // it emit a malformed packet and hang waiting for an auth reply.
        let sig_blob = self
            .provider
            .sign(&to_sign, hash)
            .map_err(|err| SshError::KeyProvider(err.to_string()))?;
        let sig_len = u32::try_from(sig_blob.len())
            .map_err(|_| SshError::KeyProvider("signature blob exceeds u32 length".to_string()))?;

        let mut signed = to_sign;
        signed.extend_from_slice(&sig_len.to_be_bytes());
        signed.extend_from_slice(&sig_blob);
        Ok(signed)
    }
}

// SPDX-License-Identifier: MIT
//! Adapts a `termite_core::KeyProvider` to `russh::auth::Signer`, so
//! `authenticate_publickey_with` can call it lazily — the key material
//! itself never crosses into `termite-ssh`, only signature bytes do.

use russh::keys::{agent::AgentIdentity, HashAlg};
use russh::Signer;

use termite_core::KeyProvider;

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
        _hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.provider
            .sign(&to_sign)
            .map_err(|err| SshError::KeyProvider(err.to_string()))
    }
}

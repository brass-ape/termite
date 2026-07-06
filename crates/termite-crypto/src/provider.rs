// SPDX-License-Identifier: MIT

use ssh_key::encoding::Encode;
use ssh_key::PrivateKey;

use termite_core::{KeyProvider, TermiteError};

/// A [`KeyProvider`] backed by an in-memory, already-decrypted private key.
/// The key material never leaves this type — callers only ever get back
/// signature bytes from [`sign`](KeyProvider::sign).
pub struct LocalKeyProvider(PrivateKey);

impl LocalKeyProvider {
    pub fn new(key: PrivateKey) -> Self {
        Self(key)
    }
}

impl KeyProvider for LocalKeyProvider {
    fn public_key_blob(&self) -> Vec<u8> {
        self.0
            .public_key()
            .key_data()
            .encode_vec()
            .expect("encoding a public key blob is infallible for in-memory keys")
    }

    fn sign(&self, data: &[u8]) -> Result<Vec<u8>, TermiteError> {
        use signature::Signer;

        let signature = self
            .0
            .try_sign(data)
            .map_err(|err| crate::error::CryptoError::Key(err.into()))?;
        let bytes = signature
            .encode_vec()
            .map_err(crate::error::CryptoError::Encoding)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use ssh_key::encoding::Decode;
    use ssh_key::public::KeyData;
    use ssh_key::Signature;

    use super::*;

    #[test]
    fn sign_produces_a_verifiable_signature() {
        use signature::Verifier;

        let key = crate::key::generate_ed25519().unwrap();
        let public_key = key.public_key().clone();
        let provider = LocalKeyProvider::new(key);

        let data = b"ssh auth session data to sign";
        let sig_bytes = provider.sign(data).unwrap();
        let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

        // `PublicKey` also has an inherent `verify(namespace, msg, &SshSig)`
        // method for the unrelated SSHSIG scheme, which method-call syntax
        // would resolve to instead of the `signature::Verifier` trait impl
        // we want here — call the trait method explicitly via UFCS.
        Verifier::verify(&public_key, data, &signature).unwrap();
    }

    #[test]
    fn tampered_signature_fails_verification() {
        use signature::Verifier;

        let key = crate::key::generate_ed25519().unwrap();
        let public_key = key.public_key().clone();
        let provider = LocalKeyProvider::new(key);

        let sig_bytes = provider.sign(b"original data").unwrap();
        let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

        assert!(Verifier::verify(&public_key, b"different data", &signature).is_err());
    }

    #[test]
    fn public_key_blob_round_trips() {
        let key = crate::key::generate_ed25519().unwrap();
        let expected = key.public_key().key_data().clone();
        let provider = LocalKeyProvider::new(key);

        let blob = provider.public_key_blob();
        let decoded = KeyData::decode(&mut &blob[..]).unwrap();

        assert_eq!(decoded, expected);
    }
}

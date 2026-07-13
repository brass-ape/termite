// SPDX-License-Identifier: MIT

use ssh_key::encoding::Encode;
use ssh_key::private::KeypairData;
use ssh_key::{HashAlg, PrivateKey};

use termite_core::{KeyProvider, RsaHashAlg, TermiteError};

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

    fn sign(&self, data: &[u8], hash: Option<RsaHashAlg>) -> Result<Vec<u8>, TermiteError> {
        use signature::Signer;

        // For RSA the signature algorithm name must match what the server
        // negotiated (rsa-sha2-256/512), so honor the requested hash;
        // ssh-key's plain `try_sign` would always pick SHA-512. All other
        // key types have a single signature scheme and ignore `hash`.
        let signature = match (self.0.key_data(), hash) {
            (KeypairData::Rsa(keypair), Some(hash)) => {
                let hash = match hash {
                    RsaHashAlg::Sha256 => HashAlg::Sha256,
                    RsaHashAlg::Sha512 => HashAlg::Sha512,
                };
                (keypair, Some(hash)).try_sign(data)
            }
            _ => self.0.try_sign(data),
        }
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
        let sig_bytes = provider.sign(data, None).unwrap();
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

        let sig_bytes = provider.sign(b"original data", None).unwrap();
        let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

        assert!(Verifier::verify(&public_key, b"different data", &signature).is_err());
    }

    /// Throwaway 2048-bit RSA key generated solely as a test fixture —
    /// embedded because RSA keygen is far too slow in debug builds.
    const RSA_TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAtDHYPb/aC1L9HLmF3VkFhvvJVw4IjXZGSg11XFbvBNuqxuFZtJWX
xlH5KOSwyYviSvVIHQzYA0qFrY+eeyQ/s90AmxxRj13JBIDGuq0fKJM9FVj7oT+MLUo847
k+KI5cZEa3tWCGn+O5xxoISeIcivo0M9TRVKyACHP5wFRfGqwRJJuPs2YC9uRxGMmttS+l
pK/zxeD8X4rQpUi6yEotN1IHL+xoLaaWSR0kbCjX0BiHcGBdtWMLs5AhEGB0/PuhSIwTxk
ev0wlhElHd7DF6zIE0IC9f7EcApm+XJcPMhAQd+fazIGQOjJwFKneNRihe3UeROY+0opeo
7NqW23cPlQAAA9BHrXcmR613JgAAAAdzc2gtcnNhAAABAQC0Mdg9v9oLUv0cuYXdWQWG+8
lXDgiNdkZKDXVcVu8E26rG4Vm0lZfGUfko5LDJi+JK9UgdDNgDSoWtj557JD+z3QCbHFGP
XckEgMa6rR8okz0VWPuhP4wtSjzjuT4ojlxkRre1YIaf47nHGghJ4hyK+jQz1NFUrIAIc/
nAVF8arBEkm4+zZgL25HEYya21L6Wkr/PF4PxfitClSLrISi03Ugcv7GgtppZJHSRsKNfQ
GIdwYF21YwuzkCEQYHT8+6FIjBPGR6/TCWESUd3sMXrMgTQgL1/sRwCmb5clw8yEBB359r
MgZA6MnAUqd41GKF7dR5E5j7Sil6js2pbbdw+VAAAAAwEAAQAAAQAAmmgb23iS8CE1XMW+
5yklecElrIzZ6W80VHOJNxcC/g0bdFA/p7pDsejhoP5WGICatUtGIicKBAw1gEhEJ1ITIg
RYIrQY6y/QCDZzJS4R0d2utbXW+m8b6sRDnBKO9TGytbIFhkz0hqeDLBCRgSRq2XjzU5je
2GL4ZEXr1slCTwYkzuzxpdiBsMQzUnpSyUViqCQc1BcRJdX8k15MrOExzrmCYTayS5IE2z
SriPv90Tld1KyWZXcOwSlhSkunY1345raiO2xRXmeEKCFPVzKilz2Olsbvt2uJB7ohuJqW
cQqrv7vpP9LxRdqTPE/Aet0vVU9RKbgQYoGqR6cwfLEjAAAAgCqJIHBPRVv/L1qNl7d/Ji
gnQIB3UsKWIn9xkYJc3Hx/h83XyTZD9QPZRDeXi6Zwmu6LMfUj8tK8/niuQjvKuPE4GYLG
rU3mDqYvvszZP+aS0DAVfu3m6rV8cEl/QpHNL2BhvRlJMs9pKOoo/O4eBq8I1Lt/OVxc3P
sfh4anBeBLAAAAgQD+dhlICb3OtPZdTgoZ75AqBZW82yTMtcEkyG743UzSBMiXGL2cASkO
54MO6SQbE201xDlUnEdRDlM/fo0EY/aiTsEYtityoSdW8f/WnauzrWRb2QXPgTyiK9Pjvm
m14El7Wo+5XY0MSReGRtheD0/nmhcGlE4Vm47exg/n1B7pJwAAAIEAtUjIWvz0JqOjmDhu
sK15AVOlC4BXPw/vexGcyp6TY6dUq4ZBB8wEumz4gI4yc9Y1UNSZiRL6hf22ZyFRMMg4Mg
yjG0H9zMiiByMvDrta+jXb04lob4QuhkD2sXD/F2dIhwPCFjdhvD9mBRslwPFt/8PqWLDi
XOSuZ+gGZFU+XuMAAAAUdGVybWl0ZS10ZXN0LWZpeHR1cmUBAgMEBQYH
-----END OPENSSH PRIVATE KEY-----
";

    fn rsa_provider() -> (ssh_key::PublicKey, LocalKeyProvider) {
        let key = PrivateKey::from_openssh(RSA_TEST_KEY).unwrap();
        let public_key = key.public_key().clone();
        (public_key, LocalKeyProvider::new(key))
    }

    #[test]
    fn rsa_signature_algorithm_matches_requested_hash() {
        use signature::Verifier;
        use ssh_key::Algorithm;

        for (hash, expected) in [
            (RsaHashAlg::Sha256, HashAlg::Sha256),
            (RsaHashAlg::Sha512, HashAlg::Sha512),
        ] {
            let (public_key, provider) = rsa_provider();
            let data = b"ssh auth session data to sign";

            let sig_bytes = provider.sign(data, Some(hash)).unwrap();
            let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

            // The wire algorithm name is what the server checks against the
            // negotiated rsa-sha2-* — the whole point of threading `hash`.
            assert_eq!(
                signature.algorithm(),
                Algorithm::Rsa {
                    hash: Some(expected)
                }
            );
            Verifier::verify(&public_key, data, &signature).unwrap();
        }
    }

    #[test]
    fn rsa_without_requested_hash_defaults_to_sha512() {
        use ssh_key::Algorithm;

        let (_, provider) = rsa_provider();
        let sig_bytes = provider.sign(b"data", None).unwrap();
        let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

        assert_eq!(
            signature.algorithm(),
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            }
        );
    }

    #[test]
    fn ed25519_ignores_requested_hash() {
        use signature::Verifier;

        let key = crate::key::generate_ed25519().unwrap();
        let public_key = key.public_key().clone();
        let provider = LocalKeyProvider::new(key);
        let data = b"ssh auth session data to sign";

        let sig_bytes = provider.sign(data, Some(RsaHashAlg::Sha256)).unwrap();
        let signature = Signature::decode(&mut &sig_bytes[..]).unwrap();

        assert_eq!(signature.algorithm(), ssh_key::Algorithm::Ed25519);
        Verifier::verify(&public_key, data, &signature).unwrap();
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

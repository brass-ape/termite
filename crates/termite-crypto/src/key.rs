// SPDX-License-Identifier: MIT
//! Loading, decrypting, generating, and saving SSH private keys.

use std::path::Path;

use secrecy::{ExposeSecret, SecretString};
use ssh_key::{Algorithm, LineEnding, PrivateKey};

use crate::error::CryptoError;

/// Reads a private key from an OpenSSH-formatted file. If the key is
/// encrypted, the returned `PrivateKey` is still encrypted — check
/// `.is_encrypted()` and call [`decrypt`] before using it.
pub fn load(path: &Path) -> Result<PrivateKey, CryptoError> {
    PrivateKey::read_openssh_file(path).map_err(CryptoError::Key)
}

/// Decrypts an encrypted private key using a passphrase. The passphrase is
/// only exposed for the duration of this call.
pub fn decrypt(key: &PrivateKey, passphrase: &SecretString) -> Result<PrivateKey, CryptoError> {
    key.decrypt(passphrase.expose_secret().as_bytes())
        .map_err(CryptoError::Key)
}

/// Generates a new ed25519 key pair. Other algorithms aren't supported yet
/// — ed25519 is the modern default and this is the only one needed until a
/// UI surfaces key generation with algorithm choice.
pub fn generate_ed25519() -> Result<PrivateKey, CryptoError> {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).map_err(CryptoError::Key)
}

/// Writes a private key to disk in OpenSSH format with restrictive
/// permissions (0600 on Unix — `write_openssh_file` sets this itself).
/// If `passphrase` is given, the key is encrypted before writing.
pub fn save_to_disk(
    key: &PrivateKey,
    path: &Path,
    passphrase: Option<&SecretString>,
) -> Result<(), CryptoError> {
    let encrypted;
    let to_write = match passphrase {
        Some(passphrase) => {
            encrypted = key.encrypt(&mut rand::rng(), passphrase.expose_secret().as_bytes())?;
            &encrypted
        }
        None => key,
    };
    to_write
        .write_openssh_file(path, LineEnding::LF)
        .map_err(CryptoError::Key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let key = generate_ed25519().unwrap();
        let passphrase = SecretString::from("correct horse battery staple".to_string());

        save_to_disk(&key, &path, Some(&passphrase)).unwrap();

        let loaded = load(&path).unwrap();
        assert!(loaded.is_encrypted());

        let decrypted = decrypt(&loaded, &passphrase).unwrap();
        assert_eq!(decrypted.public_key(), key.public_key());
    }

    #[test]
    fn decrypt_with_wrong_passphrase_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let key = generate_ed25519().unwrap();
        let passphrase = SecretString::from("correct horse battery staple".to_string());
        let wrong_passphrase = SecretString::from("not the right one".to_string());

        save_to_disk(&key, &path, Some(&passphrase)).unwrap();
        let loaded = load(&path).unwrap();

        assert!(decrypt(&loaded, &wrong_passphrase).is_err());
    }

    #[test]
    fn unencrypted_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let key = generate_ed25519().unwrap();

        save_to_disk(&key, &path, None).unwrap();

        let loaded = load(&path).unwrap();
        assert!(!loaded.is_encrypted());
        assert_eq!(loaded.public_key(), key.public_key());
    }
}

// SPDX-License-Identifier: MIT

use thiserror::Error;

/// Errors from loading, decrypting, generating, or saving SSH keys.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("key error: {0}")]
    Key(#[from] ssh_key::Error),

    #[error("key encoding error: {0}")]
    Encoding(#[from] ssh_key::encoding::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("key is encrypted and requires a passphrase")]
    PassphraseRequired,

    #[error("signature verification failed")]
    VerificationFailed,
}

impl From<CryptoError> for termite_core::TermiteError {
    fn from(err: CryptoError) -> Self {
        termite_core::TermiteError::Crypto(err.to_string())
    }
}

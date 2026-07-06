// SPDX-License-Identifier: MIT

use thiserror::Error;

/// Errors from persisted configuration and credential storage.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    TomlDe(#[from] toml::de::Error),

    #[error("TOML serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
}

impl From<StorageError> for termite_core::TermiteError {
    fn from(err: StorageError) -> Self {
        termite_core::TermiteError::Storage(err.to_string())
    }
}

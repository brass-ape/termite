// SPDX-License-Identifier: MIT

use thiserror::Error;

/// Top-level error type shared across Termite crates.
///
/// Each variant wraps errors from the relevant subsystem. Higher-level crates
/// convert their own error types into `TermiteError` at crate boundaries.
#[derive(Debug, Error)]
pub enum TermiteError {
    #[error("SSH error: {0}")]
    Ssh(String),

    #[error("Terminal error: {0}")]
    Terminal(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Crypto error: {0}")]
    Crypto(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

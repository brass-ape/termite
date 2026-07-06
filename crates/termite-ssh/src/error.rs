// SPDX-License-Identifier: MIT

use thiserror::Error;

/// Errors that can occur in the SSH protocol layer.
#[derive(Debug, Error)]
pub enum SshError {
    /// Low-level protocol/transport failure from `russh` (handshake, kex,
    /// channel, or I/O errors).
    #[error("SSH protocol error: {0}")]
    Protocol(#[from] russh::Error),

    /// Failure reading, writing, or parsing `known_hosts` or a public key.
    #[error("host key error: {0}")]
    HostKey(#[from] russh::keys::Error),

    /// The user (or the mandatory verification step) rejected the server's
    /// host key. This is a deliberate refusal, never a silent fallback.
    #[error("host key rejected by user")]
    HostKeyRejected,

    /// Authentication failed after exhausting the offered method.
    #[error("authentication failed for user {user}")]
    AuthenticationFailed { user: String },

    /// The profile requests an authentication method not yet implemented.
    #[error("authentication method not yet supported: {0}")]
    UnsupportedAuthMethod(&'static str),

    /// The session task's command or event channel was dropped by its peer.
    #[error("session channel closed unexpectedly")]
    ChannelClosed,

    /// Underlying filesystem I/O (known_hosts path resolution/read/write).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A `KeyProvider` (key loading/decryption/signing, from `termite-crypto`)
    /// failed.
    #[error("key provider error: {0}")]
    KeyProvider(String),
}

impl From<SshError> for termite_core::TermiteError {
    fn from(err: SshError) -> Self {
        termite_core::TermiteError::Ssh(err.to_string())
    }
}

impl From<russh::SendError> for SshError {
    fn from(_: russh::SendError) -> Self {
        SshError::ChannelClosed
    }
}

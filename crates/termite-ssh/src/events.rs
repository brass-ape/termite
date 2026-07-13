// SPDX-License-Identifier: MIT
//! Messages exchanged between an [`crate::session::SshSession`] task and its
//! owner, per `ARCHITECTURE.md` §6-8.

use russh::keys::HashAlg;
use secrecy::SecretString;

use crate::error::SshError;

/// A human-displayable summary of a server's host key, used to show the
/// user a fingerprint to approve — never the raw key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub algorithm: String,
    pub fingerprint: String,
}

impl HostKey {
    pub fn from_public_key(key: &russh::keys::PublicKey) -> Self {
        Self {
            algorithm: key.algorithm().to_string(),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
        }
    }
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisconnectReason {
    /// The remote end closed the connection or the shell exited.
    Remote,
    /// The app asked the session to disconnect.
    Requested,
    /// The connection failed before or during setup.
    Error(String),
}

/// A pending authentication step the app must resolve.
///
/// Password and public-key (passphrase-prompt) auth are implemented as of
/// M3. SSH-agent auth is deferred (see `HANDOFF.md`) — `AuthMethod::Agent`
/// still fails explicitly rather than adding a challenge variant for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthChallenge {
    Password,
    /// The key at the configured path is encrypted; `fingerprint` identifies
    /// it for display (never the key material itself).
    Passphrase {
        fingerprint: String,
    },
}

/// The app's answer to an [`AuthChallenge`].
#[derive(Debug, Clone)]
pub enum AuthResponse {
    Password(SecretString),
    Passphrase(SecretString),
}

/// Events emitted by an SSH session task to its owner.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// Authenticated and the shell is ready for I/O.
    Connected,
    /// The app must supply credentials to continue.
    AuthRequired(AuthChallenge),
    /// Raw bytes read from the remote shell, to feed to a terminal grid.
    Output(Vec<u8>),
    /// The session ended.
    Disconnected { reason: DisconnectReason },
    /// A non-fatal-to-report error occurred (also fired as a terminal
    /// event when a connection attempt fails outright).
    Error(String),
    /// The server's host key has no entry in `known_hosts`. The user must
    /// explicitly approve before the connection proceeds — there is no
    /// silent-accept path.
    HostKeyUnknown(HostKey),
    /// The server's host key differs from the one recorded in
    /// `known_hosts`. This is a security warning, never silently accepted.
    HostKeyMismatch(HostKey),
}

impl From<SshError> for SessionEvent {
    fn from(err: SshError) -> Self {
        SessionEvent::Error(err.to_string())
    }
}

/// Commands sent to an SSH session task by its owner.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    /// Bytes to write to the remote shell's stdin.
    Write(Vec<u8>),
    /// The terminal was resized; forward to the remote PTY.
    Resize { rows: u16, cols: u16 },
    /// Answers a pending [`AuthChallenge`].
    AuthResponse(AuthResponse),
    /// The user's decision on a pending host-key prompt
    /// (`HostKeyUnknown`/`HostKeyMismatch`). `true` trusts and records the
    /// key; `false` aborts the connection.
    ApproveHostKey(bool),
    /// Tear down the session.
    Disconnect,
}

// SPDX-License-Identifier: MIT

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};
use uuid::Uuid;

// ── Session identity ──────────────────────────────────────────────────────────

/// Unique identifier for an active SSH session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        SessionId(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Host identity ─────────────────────────────────────────────────────────────
/// Unique identifier for a saved host profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostId(Uuid);

impl HostId {
    pub fn new() -> Self {
        HostId(Uuid::new_v4())
    }
}

impl Default for HostId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Host profile ──────────────────────────────────────────────────────────────

/// A saved SSH connection profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProfile {
    pub id:       HostId,
    pub name:     String,
    pub host:     String,
    pub port:     u16,
    pub username: String,
    pub auth:     AuthMethod,
    pub tags:     Vec<String>,
}

impl HostProfile {
    /// Construct a minimal profile with sensible defaults.
    pub fn new(
        name: impl Into<String>,
        host: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            id:       HostId::new(),
            name:     name.into(),
            host:     host.into(),
            port:     22,
            username: username.into(),
            auth:     AuthMethod::Agent,
            tags:     Vec::new(),
        }
    }
}

// ── Auth method ───────────────────────────────────────────────────────────────

/// The method used to authenticate with an SSH server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthMethod {
    /// Interactive password authentication.
    /// Passwords are never stored in config files — only in the OS keychain.
    Password,

    /// Public key authentication. The key file path is stored; the passphrase
    /// (if any) is stored in the OS keychain, never on disk in plaintext.
    PublicKey { key_path: PathBuf },

    /// Authentication delegated to a running SSH agent.
    /// Preferred: no key material handled by Termite at all.
    Agent,
}

// ── Connection status ─────────────────────────────────────────────────────────

/// The current state of an SSH connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Reconnecting { attempt: u32 },
    Disconnected,
    Failed { reason: String },
}

impl fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting            => write!(f, "Connecting"),
            Self::Connected             => write!(f, "Connected"),
            Self::Reconnecting { attempt } => write!(f, "Reconnecting (attempt {attempt})"),
            Self::Disconnected          => write!(f, "Disconnected"),
            Self::Failed { reason }     => write!(f, "Failed: {reason}"),
        }
    }
}

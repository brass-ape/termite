// SPDX-License-Identifier: MIT
//! SSH protocol layer for Termite.
//!
//! Provides connection management, authentication, channel handling,
//! SFTP, and port forwarding. M2 implemented connection lifecycle, mandatory
//! host-key verification against `known_hosts`, and password authentication.
//! M3 adds public-key authentication (via `termite-crypto`'s `KeyProvider`)
//! and SSH-agent authentication. SFTP and port forwarding land in later
//! milestones.
//!
//! Key crates:
//! - `russh` — pure-Rust SSH-2 implementation
//! - `tokio` — async runtime for session tasks
//! - `tracing` — structured logging

mod agent;
pub mod error;
pub mod events;
mod handler;
pub mod known_hosts;
mod session;
mod signer;

pub use error::SshError;
pub use events::{
    AuthChallenge, AuthResponse, DisconnectReason, HostKey, SessionCommand, SessionEvent,
};
pub use known_hosts::HostKeyDecision;
pub use session::SshSession;

// SPDX-License-Identifier: MIT
//! SSH protocol layer for Termite.
//!
//! Provides connection management, authentication, channel handling,
//! SFTP, and port forwarding. Implemented in M2+.
//!
//! Key crates added in M2:
//! - `russh` — pure-Rust SSH-2 implementation
//! - `tokio` — async runtime for session tasks
//! - `tracing` — structured logging

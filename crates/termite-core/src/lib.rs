// SPDX-License-Identifier: MIT
//! Shared types, traits, and errors used across all Termite crates.
//!
//! `termite-core` has no dependencies on other workspace crates.
//! All other crates may depend on this one.

pub mod error;
pub mod types;

pub use error::TermiteError;
pub use types::{AuthMethod, ConnectionStatus, HostId, HostProfile, SessionId};

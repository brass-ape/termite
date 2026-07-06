// SPDX-License-Identifier: MIT
//! Configuration and credential storage for Termite.
//!
//! Manages host profiles, app settings, known hosts, recent connections,
//! and the OS keychain integration for passwords and key passphrases.
//! Implemented in M3+.
//!
//! Config directory:
//! - Linux:   `~/.config/termite/`
//! - macOS:   `~/Library/Application Support/termite/`
//! - Windows: `%APPDATA%\termite\`

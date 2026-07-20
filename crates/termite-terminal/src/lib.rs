// SPDX-License-Identifier: MIT
//! Terminal emulation for Termite.
//!
//! Provides VT/ANSI sequence parsing, a terminal grid, PTY management,
//! scrollback, selection, and clipboard integration. Implemented in M1.
//!
//! Key crates added in M1:
//! - `vte` — VT sequence parser (Apache-2.0)
//! - `portable-pty` — cross-platform PTY creation (MIT)
//! - `tokio` — async I/O bridging

pub mod cell;
pub mod grid;
pub mod handler;
pub mod pty;

pub use cell::{Attrs, Cell, TermColor};
pub use grid::{EraseMode, MouseTracking, TerminalGrid};
pub use handler::GridHandler;
pub use pty::{Pty, PtyError};

// SPDX-License-Identifier: MIT
//! Local PTY spawning via `portable-pty`.

use std::io::{Read, Write};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;

/// Errors that can occur creating or driving a PTY-backed shell session.
#[derive(Debug, Error)]
pub enum PtyError {
    #[error("failed to open pty: {0}")]
    Open(String),
    #[error("failed to spawn shell: {0}")]
    Spawn(String),
    #[error("failed to clone pty reader: {0}")]
    Reader(String),
    #[error("failed to take pty writer: {0}")]
    Writer(String),
    #[error("failed to resize pty: {0}")]
    Resize(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// A spawned shell (or other command) attached to a pseudo-terminal.
///
/// Owns the master side of the pty and the child process handle. The read
/// and write ends are obtained on demand via [`Pty::try_clone_reader`] and
/// [`Pty::take_writer`], since the reader is typically moved onto its own
/// thread that blocks on it.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Pty {
    /// Spawns `shell` attached to a new pty of the given size.
    pub fn spawn(shell: &str, rows: u16, cols: u16) -> Result<Self, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Open(e.to_string()))?;

        let cmd = CommandBuilder::new(shell);
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Spawn(e.to_string()))?;

        // Drop the slave end in this process now that the child has it;
        // holding it open here can prevent the reader from seeing EOF.
        let master = pair.master;
        drop(pair.slave);

        Ok(Self { master, child })
    }

    /// Obtains a fresh readable handle for the shell's output.
    pub fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, PtyError> {
        self.master
            .try_clone_reader()
            .map_err(|e| PtyError::Reader(e.to_string()))
    }

    /// Obtains a writable handle for sending input to the shell.
    pub fn take_writer(&self) -> Result<Box<dyn Write + Send>, PtyError> {
        self.master
            .take_writer()
            .map_err(|e| PtyError::Writer(e.to_string()))
    }

    /// Informs the pty (and thus the shell) of a new terminal size.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Resize(e.to_string()))
    }

    /// Returns the child's exit status without blocking, if it has already
    /// exited.
    pub fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Io)
    }
}

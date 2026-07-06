// SPDX-License-Identifier: MIT
//! The `russh::client::Handler` implementation. Its only job in M2 is
//! mandatory host-key verification — see the security invariants in
//! `CLAUDE.md`: unknown keys prompt the user, changed keys warn, and
//! nothing is ever silently accepted.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use russh::client;
use russh::keys::PublicKey;
use tokio::sync::{mpsc, oneshot};

use termite_core::SessionId;

use crate::error::SshError;
use crate::events::{HostKey, SessionEvent};
use crate::known_hosts::{self, HostKeyDecision};

/// Shared slot used to hand a host-key approval decision from the session
/// task's command loop back into a `check_server_key` call that's parked
/// waiting on it.
pub type PendingApproval = Arc<Mutex<Option<oneshot::Sender<bool>>>>;

pub struct SessionHandler {
    pub session_id: SessionId,
    pub host: String,
    pub port: u16,
    pub known_hosts_path: PathBuf,
    pub event_tx: mpsc::Sender<(SessionId, SessionEvent)>,
    pub pending_approval: PendingApproval,
}

impl client::Handler for SessionHandler {
    type Error = SshError;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        let decision = known_hosts::classify(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts_path,
        )?;

        let stale_line = match decision {
            HostKeyDecision::Trusted => return Ok(true),
            HostKeyDecision::Unknown => None,
            HostKeyDecision::Changed { line } => Some(line),
        };

        let host_key = HostKey::from_public_key(server_public_key);
        let event = match stale_line {
            None => SessionEvent::HostKeyUnknown(host_key),
            Some(_) => SessionEvent::HostKeyMismatch(host_key),
        };

        let (tx, rx) = oneshot::channel();
        *self
            .pending_approval
            .lock()
            .expect("pending_approval mutex poisoned") = Some(tx);

        if self.event_tx.send((self.session_id, event)).await.is_err() {
            // Owner has gone away; never fall back to accepting.
            return Ok(false);
        }

        let approved = rx.await.unwrap_or(false);
        if !approved {
            return Ok(false);
        }

        match stale_line {
            None => known_hosts::record(
                &self.host,
                self.port,
                server_public_key,
                &self.known_hosts_path,
            )?,
            Some(line) => known_hosts::replace(
                &self.host,
                self.port,
                server_public_key,
                &self.known_hosts_path,
                line,
            )?,
        }

        Ok(true)
    }
}

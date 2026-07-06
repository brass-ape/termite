// SPDX-License-Identifier: MIT
//! Host key verification against Termite's `known_hosts` file.
//!
//! This wraps `russh::keys::known_hosts`, which already implements the
//! standard OpenSSH `known_hosts` line format (including hashed hostnames),
//! rather than reimplementing that parsing here.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};
use russh::keys::PublicKey;

use crate::error::SshError;

/// The outcome of checking a server's host key against `known_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// The key matches a previously recorded entry for this host.
    Trusted,
    /// No entry exists for this host yet. The user must be asked.
    Unknown,
    /// An entry exists but the key differs from what's recorded — a
    /// possible man-in-the-middle attack. `line` is the 1-indexed line
    /// in the `known_hosts` file holding the stale entry.
    Changed { line: usize },
}

/// Resolves Termite's `known_hosts` file path under the platform config dir
/// (`~/.config/termite/known_hosts` on Linux, etc. — see `ARCHITECTURE.md` §8).
pub fn known_hosts_path() -> Result<PathBuf, SshError> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        SshError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "could not determine platform config directory",
        ))
    })?;
    Ok(config_dir.join("termite").join("known_hosts"))
}

/// Classifies a server's host key against the `known_hosts` file at `path`.
pub fn classify(
    host: &str,
    port: u16,
    key: &PublicKey,
    path: &Path,
) -> Result<HostKeyDecision, SshError> {
    match check_known_hosts_path(host, port, key, path) {
        Ok(true) => Ok(HostKeyDecision::Trusted),
        Ok(false) => Ok(HostKeyDecision::Unknown),
        Err(russh::keys::Error::KeyChanged { line }) => Ok(HostKeyDecision::Changed { line }),
        Err(other) => Err(SshError::HostKey(other)),
    }
}

/// Records a host key as trusted, appending it to the `known_hosts` file.
/// Only call this after explicit user approval.
pub fn record(host: &str, port: u16, key: &PublicKey, path: &Path) -> Result<(), SshError> {
    learn_known_hosts_path(host, port, key, path).map_err(SshError::HostKey)
}

/// Removes the stale entry at `line` (1-indexed, as returned in
/// [`HostKeyDecision::Changed`]) and records the new key in its place.
/// Only call this after the user has explicitly approved overwriting a
/// changed host key.
pub fn replace(
    host: &str,
    port: u16,
    key: &PublicKey,
    path: &Path,
    stale_line: usize,
) -> Result<(), SshError> {
    let file = File::open(path)?;
    let remaining: Vec<String> = BufReader::new(file)
        .lines()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| (idx + 1 != stale_line).then_some(line))
        .collect();

    let mut file = File::create(path)?;
    for line in remaining {
        writeln!(file, "{line}")?;
    }
    drop(file);

    record(host, port, key, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed25519_key() -> PublicKey {
        use russh::keys::{Algorithm, PrivateKey};
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .expect("key generation")
            .public_key()
            .clone()
    }

    #[test]
    fn unknown_host_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = ed25519_key();

        assert_eq!(
            classify("example.com", 22, &key, &path).unwrap(),
            HostKeyDecision::Unknown
        );
    }

    #[test]
    fn recorded_host_is_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = ed25519_key();

        record("example.com", 22, &key, &path).unwrap();

        assert_eq!(
            classify("example.com", 22, &key, &path).unwrap(),
            HostKeyDecision::Trusted
        );
    }

    #[test]
    fn changed_key_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let original = ed25519_key();
        let attacker = ed25519_key();

        record("example.com", 22, &original, &path).unwrap();

        // Don't assert an exact line number: `learn_known_hosts_path` writes
        // a leading blank line when creating a fresh file (its "does the
        // file end in a newline" check fails open on an empty file), which
        // is an upstream implementation detail, not part of our contract.
        match classify("example.com", 22, &attacker, &path).unwrap() {
            HostKeyDecision::Changed { .. } => {}
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn replace_then_trusted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let original = ed25519_key();
        let replacement = ed25519_key();

        record("example.com", 22, &original, &path).unwrap();
        let line = match classify("example.com", 22, &replacement, &path).unwrap() {
            HostKeyDecision::Changed { line } => line,
            other => panic!("expected Changed, got {other:?}"),
        };

        replace("example.com", 22, &replacement, &path, line).unwrap();

        assert_eq!(
            classify("example.com", 22, &replacement, &path).unwrap(),
            HostKeyDecision::Trusted
        );
    }
}

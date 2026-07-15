// SPDX-License-Identifier: MIT
//! `HostStore` implementations: `TomlHostStore` (the real on-disk store)
//! and `MemoryHostStore` (for tests).

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use termite_core::{HostId, HostProfile, TermiteError};

use crate::error::StorageError;

/// Persists saved [`HostProfile`]s. Implementations must never store secrets
/// (passwords, key passphrases) — those live only in a `CredentialStore`;
/// `HostProfile::auth` only ever holds a key *path*, never key material.
pub trait HostStore: Send + Sync {
    /// All saved profiles, in no particular order.
    fn list(&self) -> Result<Vec<HostProfile>, TermiteError>;

    /// A single profile by id, or `None` if it doesn't exist.
    fn get(&self, id: HostId) -> Result<Option<HostProfile>, TermiteError>;

    /// Inserts a new profile or overwrites the existing one with the same id.
    fn save(&self, profile: HostProfile) -> Result<(), TermiteError>;

    /// Removes a profile by id. Not an error if it doesn't exist.
    fn delete(&self, id: HostId) -> Result<(), TermiteError>;
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default)]
    hosts: Vec<HostProfile>,
}

/// A [`HostStore`] backed by a TOML file on disk (`hosts.toml` under the
/// platform config dir). Re-reads the file on every call rather than caching
/// in memory — host profile CRUD is infrequent (ARCHITECTURE.md: profiles
/// load lazily, not all at startup) and this keeps a hand-edited file from
/// going stale under a cached copy.
pub struct TomlHostStore {
    path: PathBuf,
    // Serializes this process's read-modify-write cycles; does not protect
    // against concurrent external writers (e.g. the user editing the file).
    lock: Mutex<()>,
}

impl TomlHostStore {
    /// Store backed by the TOML file at `path`, created on first save.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// `~/.config/termite/hosts.toml` on Linux, and the platform equivalent
    /// elsewhere (see `CLAUDE.md`). `None` if the platform has no config dir.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("termite").join("hosts.toml"))
    }

    fn read(&self) -> Result<HostsFile, TermiteError> {
        match fs::read_to_string(&self.path) {
            Ok(contents) => Ok(toml::from_str(&contents).map_err(StorageError::TomlDe)?),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(HostsFile::default()),
            Err(err) => Err(StorageError::Io(err).into()),
        }
    }

    fn write(&self, file: &HostsFile) -> Result<(), TermiteError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(StorageError::Io)?;
        }
        let contents = toml::to_string_pretty(file).map_err(StorageError::TomlSer)?;
        fs::write(&self.path, contents).map_err(|err| StorageError::Io(err).into())
    }
}

impl HostStore for TomlHostStore {
    fn list(&self) -> Result<Vec<HostProfile>, TermiteError> {
        let _guard = self.lock.lock().expect("TomlHostStore mutex poisoned");
        Ok(self.read()?.hosts)
    }

    fn get(&self, id: HostId) -> Result<Option<HostProfile>, TermiteError> {
        let _guard = self.lock.lock().expect("TomlHostStore mutex poisoned");
        Ok(self.read()?.hosts.into_iter().find(|h| h.id == id))
    }

    fn save(&self, profile: HostProfile) -> Result<(), TermiteError> {
        let _guard = self.lock.lock().expect("TomlHostStore mutex poisoned");
        let mut file = self.read()?;
        match file.hosts.iter_mut().find(|h| h.id == profile.id) {
            Some(existing) => *existing = profile,
            None => file.hosts.push(profile),
        }
        self.write(&file)
    }

    fn delete(&self, id: HostId) -> Result<(), TermiteError> {
        let _guard = self.lock.lock().expect("TomlHostStore mutex poisoned");
        let mut file = self.read()?;
        file.hosts.retain(|h| h.id != id);
        self.write(&file)
    }
}

/// An in-memory [`HostStore`] for tests — never touches disk.
#[derive(Default)]
pub struct MemoryHostStore {
    hosts: Mutex<Vec<HostProfile>>,
}

impl MemoryHostStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl HostStore for MemoryHostStore {
    fn list(&self) -> Result<Vec<HostProfile>, TermiteError> {
        Ok(self
            .hosts
            .lock()
            .expect("MemoryHostStore mutex poisoned")
            .clone())
    }

    fn get(&self, id: HostId) -> Result<Option<HostProfile>, TermiteError> {
        Ok(self
            .hosts
            .lock()
            .expect("MemoryHostStore mutex poisoned")
            .iter()
            .find(|h| h.id == id)
            .cloned())
    }

    fn save(&self, profile: HostProfile) -> Result<(), TermiteError> {
        let mut hosts = self.hosts.lock().expect("MemoryHostStore mutex poisoned");
        match hosts.iter_mut().find(|h| h.id == profile.id) {
            Some(existing) => *existing = profile,
            None => hosts.push(profile),
        }
        Ok(())
    }

    fn delete(&self, id: HostId) -> Result<(), TermiteError> {
        self.hosts
            .lock()
            .expect("MemoryHostStore mutex poisoned")
            .retain(|h| h.id != id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str) -> HostProfile {
        HostProfile::new(name, "example.com", "alice")
    }

    #[test]
    fn memory_store_crud() {
        let store = MemoryHostStore::new();
        let host = profile("prod");
        let id = host.id;

        assert!(store.get(id).unwrap().is_none());

        store.save(host.clone()).unwrap();
        assert_eq!(store.get(id).unwrap(), Some(host.clone()));
        assert_eq!(store.list().unwrap(), vec![host.clone()]);

        let mut renamed = host.clone();
        renamed.name = "prod-renamed".to_string();
        store.save(renamed.clone()).unwrap();
        assert_eq!(store.list().unwrap(), vec![renamed]);

        store.delete(id).unwrap();
        assert!(store.get(id).unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn toml_store_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let store = TomlHostStore::new(path.clone());
        let host = profile("staging");
        store.save(host.clone()).unwrap();

        // A fresh store instance re-reading the same path sees the save.
        let reopened = TomlHostStore::new(path);
        assert_eq!(reopened.list().unwrap(), vec![host]);
    }

    #[test]
    fn toml_store_missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = TomlHostStore::new(dir.path().join("does-not-exist.toml"));
        assert_eq!(store.list().unwrap(), Vec::new());
    }

    #[test]
    fn toml_store_delete_removes_only_matching_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = TomlHostStore::new(dir.path().join("hosts.toml"));

        let a = profile("a");
        let b = profile("b");
        store.save(a.clone()).unwrap();
        store.save(b.clone()).unwrap();

        store.delete(a.id).unwrap();
        assert_eq!(store.list().unwrap(), vec![b]);
    }

    #[test]
    fn hosts_file_written_before_favourite_and_last_connected_existed_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        // Deliberately omits `favourite`/`last_connected` — a hand-written
        // stand-in for a hosts.toml saved by an older Termite build, before
        // those fields existed.
        std::fs::write(
            &path,
            r#"
            [[hosts]]
            id = "3fa85f64-5717-4562-b3fc-2c963f66afa6"
            name = "legacy"
            host = "example.com"
            port = 22
            username = "alice"
            tags = []

            [hosts.auth]
            type = "agent"
            "#,
        )
        .unwrap();

        let store = TomlHostStore::new(path);
        let hosts = store.list().unwrap();
        assert_eq!(hosts.len(), 1);
        assert!(!hosts[0].favourite);
        assert_eq!(hosts[0].last_connected, None);
    }
}

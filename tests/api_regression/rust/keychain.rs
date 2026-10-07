//! Persistent synthetic secrets. This module belongs only to the separate test crate.
use seasnail_crypto::{Error, KeychainLabel, KeychainStore};
use std::os::unix::fs::PermissionsExt;
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf, sync::Mutex};

pub struct FixtureKeychain {
    path: PathBuf,
    mode: String,
    lock: Mutex<()>,
}
impl FixtureKeychain {
    pub fn new(home: PathBuf, mode: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            ["persistent", "missing-master-dek", "read-error"].contains(&mode),
            "unknown keychain mode"
        );
        Ok(Self {
            path: home.join("fixture-keychain.json"),
            mode: mode.into(),
            lock: Mutex::new(()),
        })
    }
    fn key(label: KeychainLabel, account: &str) -> String {
        format!("{label:?}:{account}")
    }
    fn read(&self) -> Result<BTreeMap<String, Vec<u8>>, Error> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| Error::Keychain("invalid fixture store".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err(Error::Keychain("fixture read failed".into())),
        }
    }
    fn write(&self, values: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
        let persist = || -> anyhow::Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
            tmp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            serde_json::to_writer(&mut tmp, values)?;
            tmp.flush()?;
            tmp.as_file().sync_all()?;
            tmp.persist(&self.path)?;
            fs::File::open(self.path.parent().unwrap())?.sync_all()?;
            Ok(())
        };
        persist().map_err(|_| Error::Keychain("fixture atomic write failed".into()))
    }
}
impl KeychainStore for FixtureKeychain {
    fn set_secret(&self, l: KeychainLabel, a: &str, v: &[u8]) -> Result<(), Error> {
        let _guard = self.lock.lock().unwrap();
        let mut values = self.read()?;
        values.insert(Self::key(l, a), v.to_vec());
        self.write(&values)
    }
    fn get_secret(&self, l: KeychainLabel, a: &str) -> Result<Option<Vec<u8>>, Error> {
        if l == KeychainLabel::MasterDek {
            if self.mode == "missing-master-dek" {
                return Ok(None);
            }
            if self.mode == "read-error" {
                return Err(Error::Keychain("controlled fixture read failure".into()));
            }
        }
        let _guard = self.lock.lock().unwrap();
        Ok(self.read()?.get(&Self::key(l, a)).cloned())
    }
    fn delete_secret(&self, l: KeychainLabel, a: &str) -> Result<(), Error> {
        let _guard = self.lock.lock().unwrap();
        let mut values = self.read()?;
        values.remove(&Self::key(l, a));
        self.write(&values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_phase() {
        let Ok(home) = std::env::var("FIXTURE_TEST_HOME") else {
            return;
        };
        let kc = FixtureKeychain::new(PathBuf::from(home), "persistent").unwrap();
        for label in [
            KeychainLabel::MasterDek,
            KeychainLabel::RootTokenSecret,
            KeychainLabel::ProviderCredentials,
        ] {
            match std::env::var("FIXTURE_TEST_PHASE").unwrap().as_str() {
                "write" => kc
                    .set_secret(label, "synthetic-account", b"synthetic-secret")
                    .unwrap(),
                "read-delete" => {
                    assert_eq!(
                        kc.get_secret(label, "synthetic-account").unwrap(),
                        Some(b"synthetic-secret".to_vec())
                    );
                    kc.delete_secret(label, "synthetic-account").unwrap();
                }
                "absent" => assert_eq!(kc.get_secret(label, "synthetic-account").unwrap(), None),
                _ => panic!("invalid phase"),
            }
        }
    }
    #[test]
    fn persistence_across_independent_processes_and_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        for phase in ["write", "read-delete", "absent"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "keychain::tests::child_phase"])
                .env("FIXTURE_TEST_HOME", dir.path())
                .env("FIXTURE_TEST_PHASE", phase)
                .output()
                .unwrap();
            assert!(output.status.success(), "phase {phase} failed");
        }
        assert_eq!(
            fs::metadata(dir.path().join("fixture-keychain.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn controlled_master_dek_modes_leave_other_labels_readable() {
        let dir = tempfile::tempdir().unwrap();
        let base = FixtureKeychain::new(dir.path().into(), "persistent").unwrap();
        base.set_secret(KeychainLabel::MasterDek, "a", b"dek")
            .unwrap();
        base.set_secret(KeychainLabel::RootTokenSecret, "a", b"token")
            .unwrap();
        let missing = FixtureKeychain::new(dir.path().into(), "missing-master-dek").unwrap();
        assert_eq!(
            missing.get_secret(KeychainLabel::MasterDek, "a").unwrap(),
            None
        );
        assert_eq!(
            missing
                .get_secret(KeychainLabel::RootTokenSecret, "a")
                .unwrap(),
            Some(b"token".to_vec())
        );
        let failed = FixtureKeychain::new(dir.path().into(), "read-error").unwrap();
        assert!(failed.get_secret(KeychainLabel::MasterDek, "a").is_err());
    }
}

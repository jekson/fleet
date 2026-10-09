//! Software keys in plain files under the data dir (no TPM / Hello).
//! P-256 keys per role (`keys/<role>.key`, hex scalar), the Noise static
//! key and the cache integrity key (`keys/noise.key`, `keys/cache.key`).

use fleet_core_ffi::{DeviceSigner, KeyRole, KeyStore, SignerError};
use fleet_crypto::sig::{Signer as _, SoftwareP256Signer};
use std::fs;
use std::path::{Path, PathBuf};

fn read_hex32(path: &Path) -> Result<Option<[u8; 32]>, SignerError> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let v = hex::decode(s.trim()).map_err(|_| SignerError::Failed)?;
            let b: [u8; 32] = v.try_into().map_err(|_| SignerError::Failed)?;
            Ok(Some(b))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SignerError::Failed),
    }
}

fn write_hex(path: &Path, bytes: &[u8]) -> Result<(), SignerError> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|_| SignerError::Failed)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, hex::encode(bytes)).map_err(|_| SignerError::Failed)?;
    fs::rename(&tmp, path).map_err(|_| SignerError::Failed)
}

fn load_or_create(dir: &Path, name: &str) -> Result<SoftwareP256Signer, String> {
    let path = dir.join(format!("{name}.key"));
    if let Some(b) = read_hex32(&path).map_err(|e| format!("{name}.key: {e}"))? {
        return SoftwareP256Signer::from_bytes(&b).map_err(|e| format!("{name}.key: {e}"));
    }
    loop {
        let mut b = [0u8; 32];
        fleet_crypto::random_bytes(&mut b).map_err(|e| e.to_string())?;
        if let Ok(k) = SoftwareP256Signer::from_bytes(&b) {
            write_hex(&path, &b).map_err(|e| format!("{name}.key: {e}"))?;
            return Ok(k);
        }
    }
}

pub struct FileSigner {
    root: SoftwareP256Signer,
    device: SoftwareP256Signer,
    monitor: SoftwareP256Signer,
    ssh: SoftwareP256Signer,
    monitor_ssh: SoftwareP256Signer,
}

impl FileSigner {
    pub fn open(dir: &Path) -> Result<Self, String> {
        Ok(Self {
            root: load_or_create(dir, "root")?,
            device: load_or_create(dir, "device")?,
            monitor: load_or_create(dir, "monitor")?,
            ssh: load_or_create(dir, "ssh")?,
            monitor_ssh: load_or_create(dir, "monitor_ssh")?,
        })
    }

    fn key(&self, role: KeyRole) -> &SoftwareP256Signer {
        match role {
            KeyRole::Root => &self.root,
            KeyRole::Device => &self.device,
            KeyRole::Monitor => &self.monitor,
            KeyRole::Ssh => &self.ssh,
            KeyRole::MonitorSsh => &self.monitor_ssh,
        }
    }
}

impl DeviceSigner for FileSigner {
    fn public_key(&self, role: KeyRole) -> Result<Vec<u8>, SignerError> {
        Ok(self.key(role).public().0.to_vec())
    }

    fn sign(&self, role: KeyRole, msg: Vec<u8>, _reason: String) -> Result<Vec<u8>, SignerError> {
        self.key(role)
            .sign(&msg)
            .map(|s| s.0.to_vec())
            .map_err(|_| SignerError::Failed)
    }
}

pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }
}

impl KeyStore for FileKeyStore {
    fn load_noise_key(&self) -> Result<Option<Vec<u8>>, SignerError> {
        Ok(read_hex32(&self.dir.join("noise.key"))?.map(|b| b.to_vec()))
    }
    fn store_noise_key(&self, secret: Vec<u8>) -> Result<(), SignerError> {
        write_hex(&self.dir.join("noise.key"), &secret)
    }
    fn load_cache_key(&self) -> Result<Option<Vec<u8>>, SignerError> {
        Ok(read_hex32(&self.dir.join("cache.key"))?.map(|b| b.to_vec()))
    }
    fn store_cache_key(&self, secret: Vec<u8>) -> Result<(), SignerError> {
        write_hex(&self.dir.join("cache.key"), &secret)
    }
}

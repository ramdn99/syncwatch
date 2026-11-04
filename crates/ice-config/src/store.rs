//! Secure storage of the user's Metered credentials (spec section 5 "Storage").
//!
//! Installed mode: Windows Credential Manager via `keyring`. Payloads too large for a Credential
//! Manager blob (~2.5 KB) go to a DPAPI-encrypted file instead. Portable mode: a DPAPI-encrypted
//! file under `data\` (bound to the Windows user and PC). The plain settings file holds only
//! hostnames and status. `Debug` never shows secrets.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const KEYRING_SERVICE: &str = "com.syncwatch.app";
pub const KEYRING_ACCOUNT: &str = "metered-credentials";
/// Credential Manager blobs are limited to 5*512 bytes; stay well below.
const KEYRING_MAX: usize = 2000;

/// What the user saved.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum StoredCredentials {
    /// Tab A: app name + API key; fresh ICE servers are fetched on every create/join.
    ApiKey { app: String, key: String },
    /// Tab B: the validated ICE servers (normalized JSON).
    IceServers {
        servers: Vec<crate::model::IceServer>,
    },
}

impl std::fmt::Debug for StoredCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoredCredentials::ApiKey { .. } => {
                f.write_str("StoredCredentials::ApiKey(<redacted>)")
            }
            StoredCredentials::IceServers { servers } => write!(
                f,
                "StoredCredentials::IceServers({} servers, <redacted>)",
                servers.len()
            ),
        }
    }
}

impl StoredCredentials {
    /// Every secret string, for registration with the log scrubber.
    pub fn secret_values(&self) -> Vec<String> {
        match self {
            StoredCredentials::ApiKey { app, key } => vec![app.clone(), key.clone()],
            StoredCredentials::IceServers { servers } => servers
                .iter()
                .flat_map(|s| [s.username.clone(), s.credential.clone()])
                .flatten()
                .collect(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("the secure credential store is not available")]
    Unavailable,
    #[error("the saved credentials could not be read")]
    Corrupt,
    #[error("file error")]
    Io,
}

pub trait SecretStore: Send + Sync {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError>;
    fn save(&self, data: &[u8]) -> Result<(), StoreError>;
    fn delete(&self) -> Result<(), StoreError>;
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<Option<Vec<u8>>>);

impl SecretStore for MemoryStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.0.lock().map_err(|_| StoreError::Unavailable)?.clone())
    }
    fn save(&self, data: &[u8]) -> Result<(), StoreError> {
        *self.0.lock().map_err(|_| StoreError::Unavailable)? = Some(data.to_vec());
        Ok(())
    }
    fn delete(&self) -> Result<(), StoreError> {
        *self.0.lock().map_err(|_| StoreError::Unavailable)? = None;
        Ok(())
    }
}

/// DPAPI-encrypted file (current user scope).
pub struct DpapiFileStore {
    pub path: PathBuf,
}

impl SecretStore for DpapiFileStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(b) => dpapi::unprotect(&b).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(StoreError::Io),
        }
    }
    fn save(&self, data: &[u8]) -> Result<(), StoreError> {
        let enc = dpapi::protect(data)?;
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p).map_err(|_| StoreError::Io)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, enc).map_err(|_| StoreError::Io)?;
        std::fs::rename(&tmp, &self.path).map_err(|_| StoreError::Io)
    }
    fn delete(&self) -> Result<(), StoreError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(StoreError::Io),
        }
    }
}

/// Windows Credential Manager, with a DPAPI-file overflow for large payloads.
pub struct KeyringStore {
    pub account: String,
    pub overflow: DpapiFileStore,
}

impl KeyringStore {
    fn entry(&self) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(KEYRING_SERVICE, &self.account).map_err(|_| StoreError::Unavailable)
    }
}

impl SecretStore for KeyringStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match self.entry()?.get_secret() {
            Ok(b) if b == b"overflow" => self.overflow.load(),
            Ok(b) => Ok(Some(b)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(StoreError::Unavailable),
        }
    }
    fn save(&self, data: &[u8]) -> Result<(), StoreError> {
        if data.len() > KEYRING_MAX {
            self.overflow.save(data)?;
            self.entry()?
                .set_secret(b"overflow")
                .map_err(|_| StoreError::Unavailable)
        } else {
            let _ = self.overflow.delete();
            self.entry()?
                .set_secret(data)
                .map_err(|_| StoreError::Unavailable)
        }
    }
    fn delete(&self) -> Result<(), StoreError> {
        let _ = self.overflow.delete();
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(StoreError::Unavailable),
        }
    }
}

pub fn save_credentials(store: &dyn SecretStore, c: &StoredCredentials) -> Result<(), StoreError> {
    let json = serde_json::to_vec(c).map_err(|_| StoreError::Corrupt)?;
    store.save(&json)
}

pub fn load_credentials(store: &dyn SecretStore) -> Result<Option<StoredCredentials>, StoreError> {
    match store.load()? {
        None => Ok(None),
        Some(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|_| StoreError::Corrupt),
    }
}

/// The last ICE servers fetched successfully (encrypted), for when Metered is unreachable.
#[derive(Clone, Serialize, Deserialize)]
pub struct LastGood {
    pub fetched_at_unix: i64,
    pub servers: Vec<crate::model::IceServer>,
}

#[cfg(windows)]
mod dpapi {
    use super::StoreError;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    /// Extra entropy so other apps using DPAPI for the same user can't trivially decrypt.
    const ENTROPY: &[u8] = b"SyncWatch connection credentials v1";

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, StoreError> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        // SAFETY: input/entropy point to live buffers for the duration of the call; the output
        // buffer is allocated by Windows and released with LocalFree below.
        unsafe {
            let res = if protect {
                CryptProtectData(
                    &input,
                    None,
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            } else {
                CryptUnprotectData(
                    &input,
                    None,
                    Some(&entropy),
                    None,
                    None,
                    CRYPTPROTECT_UI_FORBIDDEN,
                    &mut out,
                )
            };
            if res.is_err() || out.pbData.is_null() {
                return Err(if protect {
                    StoreError::Unavailable
                } else {
                    StoreError::Corrupt
                });
            }
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
            Ok(v)
        }
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, StoreError> {
        run(data, true)
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, StoreError> {
        run(data, false)
    }
}

#[cfg(not(windows))]
mod dpapi {
    use super::StoreError;
    pub fn protect(_: &[u8]) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unavailable)
    }
    pub fn unprotect(_: &[u8]) -> Result<Vec<u8>, StoreError> {
        Err(StoreError::Unavailable)
    }
}

pub use dpapi::{protect as dpapi_protect, unprotect as dpapi_unprotect};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_round_trip_and_redacted_debug() {
        let s = MemoryStore::default();
        let c = StoredCredentials::ApiKey {
            app: "secretapp".into(),
            key: "secretkey-123".into(),
        };
        save_credentials(&s, &c).unwrap();
        assert_eq!(load_credentials(&s).unwrap(), Some(c.clone()));
        assert!(!format!("{c:?}").contains("secret"));
        assert_eq!(
            c.secret_values(),
            vec!["secretapp".to_owned(), "secretkey-123".to_owned()]
        );
        s.delete().unwrap();
        assert_eq!(load_credentials(&s).unwrap(), None);
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_file_is_encrypted_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("syncwatch-dpapi-test-{}", std::process::id()));
        let store = DpapiFileStore {
            path: dir.join("creds.bin"),
        };
        let c = StoredCredentials::ApiKey {
            app: "plainappname".into(),
            key: "plain-key-value".into(),
        };
        save_credentials(&store, &c).unwrap();
        let raw = std::fs::read(&store.path).unwrap();
        let hay = String::from_utf8_lossy(&raw);
        assert!(
            !hay.contains("plainappname") && !hay.contains("plain-key-value"),
            "file must not hold plaintext"
        );
        assert_eq!(load_credentials(&store).unwrap(), Some(c));
        // Tampering is detected.
        let mut bad = raw.clone();
        let n = bad.len();
        bad[n - 5] ^= 0xFF;
        std::fs::write(&store.path, bad).unwrap();
        assert!(load_credentials(&store).is_err());
        store.delete().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Opt-in: touches the real Windows Credential Manager (throwaway account, cleaned up).
    ///   cargo test -p syncwatch-ice-config -- --ignored keyring
    #[cfg(windows)]
    #[test]
    #[ignore = "writes to the real Windows Credential Manager"]
    fn keyring_round_trip_and_overflow() {
        let dir =
            std::env::temp_dir().join(format!("syncwatch-keyring-test-{}", std::process::id()));
        let store = KeyringStore {
            account: format!("syncwatch-test-{}", std::process::id()),
            overflow: DpapiFileStore {
                path: dir.join("overflow.bin"),
            },
        };
        let small = StoredCredentials::ApiKey {
            app: "testapp".into(),
            key: "placeholder-key".into(),
        };
        save_credentials(&store, &small).unwrap();
        assert_eq!(load_credentials(&store).unwrap(), Some(small));
        assert!(
            !store.overflow.path.exists(),
            "small payloads stay in Credential Manager"
        );
        // Large payload (> Credential Manager blob limit) goes to the DPAPI overflow file.
        let big = StoredCredentials::ApiKey {
            app: "testapp".into(),
            key: "k".repeat(240).repeat(12),
        };
        save_credentials(&store, &big).unwrap();
        assert!(store.overflow.path.exists());
        assert_eq!(load_credentials(&store).unwrap(), Some(big));
        store.delete().unwrap();
        assert_eq!(load_credentials(&store).unwrap(), None);
        assert!(!store.overflow.path.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}

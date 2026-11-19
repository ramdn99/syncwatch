//! "Remove all my SyncWatch data" (Settings, About) and `syncwatch.exe --cleanup-user-data`
//! (run by the MSI on uninstall when REMOVEUSERDATA=1, Milestone 9).
//!
//! Removes, for the current Windows user:
//! - the data folder (`%LOCALAPPDATA%\SyncWatch`, or `<exe>\data` in portable mode): settings,
//!   window position, logs, caches, the WebView2 profile, finished torrent files;
//! - the Credential Manager entries for the relay setup and the identity key.
//!
//! The running app cannot delete its own WebView2 profile (Edge holds it open), so the Settings
//! button starts `--cleanup-user-data --wait` and quits; the helper retries until the files are
//! free (up to 60 s). It never touches the install folder or anything else.

use crate::paths::AppPaths;
use std::time::{Duration, Instant};
use syncwatch_ice_config::store::{self, DpapiFileStore, KeyringStore, SecretStore};

pub const FLAG: &str = "--cleanup-user-data";

/// What was removed (for the exit code and a test).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub folder_removed: bool,
    pub credentials_removed: usize,
}

/// The Credential Manager accounts SyncWatch uses (DPAPI overflow files live in the folder).
fn keyring_accounts() -> [&'static str; 2] {
    [store::KEYRING_ACCOUNT, "identity-ed25519"]
}

/// Removes everything; `wait_for` gives files held by a closing app time to be released.
pub fn run(paths: &AppPaths, wait_for: Duration) -> Report {
    let mut r = Report::default();
    if !paths.portable {
        for account in keyring_accounts() {
            let s = KeyringStore {
                account: account.to_owned(),
                overflow: DpapiFileStore {
                    path: paths.config.join("__none__"),
                },
            };
            if s.delete().is_ok() {
                r.credentials_removed += 1;
            }
        }
    }
    let end = Instant::now() + wait_for;
    loop {
        match std::fs::remove_dir_all(&paths.root) {
            Ok(()) => {
                r.folder_removed = true;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                r.folder_removed = true;
                break;
            }
            Err(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(500)),
            Err(_) => break,
        }
    }
    r
}

/// Settings: start the cleanup helper (it waits for this process to end), then quit.
#[tauri::command]
pub fn remove_all_user_data(app: tauri::AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Could not find SyncWatch: {e}"))?;
    std::process::Command::new(exe)
        .args([FLAG, "--wait"])
        .spawn()
        .map_err(|e| format!("Could not start the cleanup: {e}"))?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_the_data_folder_and_only_it() {
        let base = std::env::temp_dir().join(format!("sw-cleanup-{}", std::process::id()));
        let exe_dir = base.join("install");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(exe_dir.join("syncwatch.exe"), b"x").unwrap();
        // Portable layout: no Credential Manager involved.
        let paths = AppPaths::from_parts(exe_dir.clone(), true, None);
        paths.ensure();
        std::fs::write(paths.config.join("prefs.json"), b"{}").unwrap();
        std::fs::create_dir_all(paths.root.join("webview2/Default")).unwrap();
        let r = run(&paths, Duration::from_secs(1));
        assert!(r.folder_removed);
        assert!(!paths.root.exists());
        assert!(
            exe_dir.join("syncwatch.exe").exists(),
            "the install folder is untouched"
        );
        // Running it again is fine (nothing left).
        assert!(run(&paths, Duration::from_secs(1)).folder_removed);
        let _ = std::fs::remove_dir_all(&base);
    }
}

//! The app's torrent service (spec section 10): ONE engine actor for the process (it has no
//! librqbit session until a torrent starts, ADR 0010), persisted torrent settings, and the
//! commands the Change-source dialog and the Settings screen use. Engine events go to the
//! active room's driver.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use syncwatch_torrent::{EngineConfig, EngineEvent, EngineHandle};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

use crate::paths::AppPaths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TorrentSettings {
    /// None = the default under the app's cache folder.
    pub cache_dir: Option<String>,
    pub cap_gb: u32,
    /// Keep a finished file when leaving the room (it is replayed without re-joining the
    /// swarm); off = delete it on leave.
    pub keep_finished: bool,
    /// Incoming peer connections; 0 = random.
    pub port: u16,
    /// Bind address ("" = all interfaces).
    pub bind: String,
    pub dht: bool,
    /// The user acknowledged the "you are responsible for what you stream" notice.
    pub notice_accepted: bool,
}

impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            cache_dir: None,
            cap_gb: 50,
            keep_finished: true,
            port: 0,
            bind: String::new(),
            dht: true,
            notice_accepted: false,
        }
    }
}

pub struct TorrentService {
    pub engine: EngineHandle,
    room_tx: Mutex<Option<mpsc::UnboundedSender<EngineEvent>>>,
    settings: Mutex<TorrentSettings>,
    path: PathBuf,
    default_cache: PathBuf,
}

#[cfg(windows)]
fn free_space(p: &Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows::core::HSTRING;
    let mut free = 0u64;
    let dir = HSTRING::from(p.as_os_str());
    // SAFETY: valid null-terminated wide string and a valid out pointer for the call.
    unsafe { GetDiskFreeSpaceExW(&dir, Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(not(windows))]
fn free_space(_p: &Path) -> Option<u64> {
    None
}

fn config_for(default_cache: &Path, s: &TorrentSettings) -> EngineConfig {
    let dir = s
        .cache_dir
        .as_deref()
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_cache.to_path_buf());
    let mut c = EngineConfig::new(dir);
    c.cap_bytes = u64::from(s.cap_gb.max(1)) * 1024 * 1024 * 1024;
    c.keep_finished = s.keep_finished;
    c.listen_port = s.port;
    c.bind_ip = s.bind.trim().parse().ok();
    c.dht = s.dht;
    c.metadata_timeout = Duration::from_secs(90);
    c.free_space = Some(Arc::new(free_space));
    c
}

impl TorrentService {
    /// Creates the service and its (idle) engine actor. Must run inside the tokio runtime.
    pub fn start(paths: &AppPaths) -> Arc<Self> {
        let path = paths.config.join("torrent.json");
        let mut settings: TorrentSettings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // Debug builds: automated window checks stay on the loopback (no firewall prompt, no
        // DHT, no public swarm).
        if cfg!(debug_assertions) {
            if let Ok(b) = std::env::var("SYNCWATCH_DEV_TORRENT_BIND") {
                settings.bind = b;
            }
            if std::env::var("SYNCWATCH_DEV_TORRENT_DHT").as_deref() == Ok("0") {
                settings.dht = false;
            }
        }
        let default_cache = paths.cache.join("torrents");
        let (engine, mut rx) = EngineHandle::spawn(config_for(&default_cache, &settings));
        let me = Arc::new(Self {
            engine,
            room_tx: Mutex::new(None),
            settings: Mutex::new(settings),
            path,
            default_cache,
        });
        let weak = Arc::downgrade(&me);
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let Some(s) = weak.upgrade() else { break };
                let tx = s.room_tx.lock().ok().and_then(|g| g.clone());
                if let Some(tx) = tx {
                    let _ = tx.send(ev);
                }
            }
        });
        me
    }

    fn config(&self, s: &TorrentSettings) -> EngineConfig {
        config_for(&self.default_cache, s)
    }

    pub fn settings(&self) -> TorrentSettings {
        self.settings.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn save(&self, s: TorrentSettings) {
        if let Ok(b) = serde_json::to_vec_pretty(&s) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
        self.engine.set_config(self.config(&s));
        if let Ok(mut g) = self.settings.lock() {
            *g = s;
        }
    }

    /// The active room receives the engine's events (one room at a time).
    pub fn attach_room(&self) -> mpsc::UnboundedReceiver<EngineEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut g) = self.room_tx.lock() {
            *g = Some(tx);
        }
        rx
    }

    pub fn detach_room(&self) {
        if let Ok(mut g) = self.room_tx.lock() {
            *g = None;
        }
    }
}

// ------------------------------------------------------------------------ commands -------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFileDto {
    index: u32,
    path: String,
    size: u64,
    video: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentInfoDto {
    infohash: String,
    name: String,
    files: Vec<TorrentFileDto>,
}

pub fn engine_error_text(e: &syncwatch_torrent::EngineError) -> &'static str {
    use syncwatch_torrent::EngineError as E;
    match e {
        E::BadMagnet => "This is not a valid magnet link.",
        E::MetadataTimeout => {
            "No peer sent the torrent's file list in time. The torrent may have no seeds right now."
        }
        E::Metadata => "The torrent's file list could not be fetched.",
        E::UnsafePath => "This torrent contains an unsafe file name or path and was refused.",
        E::NoSuchFile => "The selected file does not exist in this torrent.",
        E::SizeMismatch => "This torrent's file differs from the one the host selected.",
        E::DiskFull => "There is not enough free disk space for this file.",
        E::CacheFull => {
            "The torrent cache is full. Raise the cache size in Settings or clear finished files."
        }
        E::Engine => "The torrent engine could not start.",
    }
}

/// Host, Change source: fetch the file list of a magnet (metadata only; the trackers are told
/// "stopped" right after).
#[tauri::command]
pub async fn torrent_metadata(app: AppHandle, magnet: String) -> Result<TorrentInfoDto, String> {
    let svc = Arc::clone(&*app.state::<Arc<TorrentService>>());
    let info = svc
        .engine
        .metadata(&magnet)
        .await
        .map_err(|e| engine_error_text(&e).to_owned())?;
    let mut files: Vec<TorrentFileDto> = info
        .files
        .into_iter()
        .map(|f| TorrentFileDto {
            index: f.index,
            path: f.path,
            size: f.len,
            video: f.video,
        })
        .collect();
    // Videos first, largest first (the likely main file), then the rest.
    files.sort_by(|a, b| b.video.cmp(&a.video).then(b.size.cmp(&a.size)));
    Ok(TorrentInfoDto {
        infohash: info.infohash.to_hex(),
        name: info.name,
        files,
    })
}

#[tauri::command]
pub fn torrent_settings_get(app: AppHandle) -> TorrentSettings {
    app.state::<Arc<TorrentService>>().settings()
}

#[tauri::command]
pub fn torrent_settings_set(app: AppHandle, settings: TorrentSettings) -> Result<(), String> {
    if !settings.bind.trim().is_empty() && settings.bind.trim().parse::<std::net::IpAddr>().is_err()
    {
        return Err("The bind address must be an IP address (or empty for all).".into());
    }
    app.state::<Arc<TorrentService>>().save(settings);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults_and_partial_json() {
        let s: TorrentSettings = serde_json::from_str(r#"{"capGb": 10}"#).unwrap_or_default();
        assert_eq!(s.cap_gb, 10);
        assert!(s.dht && s.keep_finished && !s.notice_accepted);
        assert_eq!(s.port, 0);
    }

    #[test]
    fn every_engine_error_has_a_friendly_text() {
        use syncwatch_torrent::EngineError as E;
        for e in [
            E::BadMagnet,
            E::MetadataTimeout,
            E::Metadata,
            E::UnsafePath,
            E::NoSuchFile,
            E::SizeMismatch,
            E::DiskFull,
            E::CacheFull,
            E::Engine,
        ] {
            let t = engine_error_text(&e);
            assert!(t.ends_with('.') && t.len() > 20, "{e:?}");
        }
    }
}

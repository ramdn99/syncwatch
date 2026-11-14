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
    /// Largest torrent metadata accepted (a .torrent file or the host's offer), MB.
    pub meta_cap_mb: u32,
    /// Buffering window (ADR 0012): media time before and after the room position that must
    /// be on disk to be "ready", in seconds.
    pub window_before_secs: u32,
    pub window_after_secs: u32,
    /// Start-up range counted for a player that has not opened the file yet, KiB.
    pub startup_kb: u32,
}

/// Allowed ranges of the numeric settings (also shown in Settings). Inclusive.
pub const META_CAP_MB: (u32, u32) = (1, 16);
pub const WINDOW_BEFORE_SECS: (u32, u32) = (0, 10);
pub const WINDOW_AFTER_SECS: (u32, u32) = (2, 60);
pub const STARTUP_KB: (u32, u32) = (256, 8192);

impl TorrentSettings {
    /// Friendly text for the first value outside its range, if any.
    pub fn validate(&self) -> Result<(), String> {
        let check = |v: u32, (lo, hi): (u32, u32), what: &str, unit: &str| {
            if v < lo || v > hi {
                Err(format!("{what} must be between {lo} and {hi} {unit}."))
            } else {
                Ok(())
            }
        };
        check(
            self.meta_cap_mb,
            META_CAP_MB,
            "The metadata size limit",
            "MB",
        )?;
        check(
            self.window_before_secs,
            WINDOW_BEFORE_SECS,
            "The window before the position",
            "seconds",
        )?;
        check(
            self.window_after_secs,
            WINDOW_AFTER_SECS,
            "The window after the position",
            "seconds",
        )?;
        check(self.startup_kb, STARTUP_KB, "The start-up range", "KB")?;
        if !self.bind.trim().is_empty() && self.bind.trim().parse::<std::net::IpAddr>().is_err() {
            return Err("The bind address must be an IP address (or empty for all).".into());
        }
        Ok(())
    }
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
            meta_cap_mb: 5,
            window_before_secs: 1,
            window_after_secs: 5,
            startup_kb: 1024,
        }
    }
}

pub struct TorrentService {
    pub engine: EngineHandle,
    room_tx: Mutex<Option<mpsc::UnboundedSender<EngineEvent>>>,
    /// Recent torrent metadata the host has (infohash, sanitized .torrent bytes).
    metas: Mutex<Vec<(syncwatch_protocol::Infohash, Vec<u8>)>>,
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

fn meta_cap(s: &TorrentSettings) -> usize {
    (s.meta_cap_mb.clamp(1, 16) as usize * 1024 * 1024)
        .min(syncwatch_protocol::limits::MAX_TORRENT_META_BYTES)
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
    c.meta_cap = meta_cap(s);
    let (wb, wa, sk) = (
        s.window_before_secs
            .clamp(WINDOW_BEFORE_SECS.0, WINDOW_BEFORE_SECS.1),
        s.window_after_secs
            .clamp(WINDOW_AFTER_SECS.0, WINDOW_AFTER_SECS.1),
        s.startup_kb.clamp(STARTUP_KB.0, STARTUP_KB.1),
    );
    c.ready.back_ms = u64::from(wb) * 1000;
    c.ready.window_ms = u64::from(wa) * 1000;
    // The keep window (hysteresis) stays well inside the full window.
    c.ready.keep_ms = c.ready.window_ms.min(1500).max(c.ready.window_ms * 3 / 10);
    c.ready.startup_bytes = u64::from(sk) * 1024;
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
            metas: Mutex::new(Vec::new()),
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

    /// Metadata the host already has (a magnet's file list fetched in the Change source
    /// dialog, or a .torrent file): the room starts from it without asking the swarm again.
    pub fn cache_meta(&self, ih: syncwatch_protocol::Infohash, bytes: Vec<u8>) {
        if let Ok(mut g) = self.metas.lock() {
            g.retain(|(h, _)| *h != ih);
            g.push((ih, bytes));
            if g.len() > 8 {
                g.remove(0);
            }
        }
    }

    pub fn cached_meta(&self, ih: &syncwatch_protocol::Infohash) -> Option<Vec<u8>> {
        self.metas
            .lock()
            .ok()
            .and_then(|g| g.iter().find(|(h, _)| h == ih).map(|(_, b)| b.clone()))
    }

    pub fn meta_cap(&self) -> usize {
        meta_cap(&self.settings())
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
    total_size: u64,
    trackers: usize,
    /// BEP 19 web seeds listed (librqbit 9.0.1 cannot use them).
    web_seeds: usize,
}

fn info_dto(info: syncwatch_torrent::TorrentInfo) -> TorrentInfoDto {
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
    TorrentInfoDto {
        infohash: info.infohash.to_hex(),
        name: info.name,
        files,
        total_size: info.total_size,
        trackers: info.trackers,
        web_seeds: info.web_seeds,
    }
}

/// Host, Change source: a .torrent FILE. Read and validated locally (no network at all):
/// size cap, bencode limits, safe paths, UTF-8, tracker caps. Its bytes are kept for the room.
#[tauri::command]
pub async fn torrent_inspect_file(app: AppHandle, path: String) -> Result<TorrentInfoDto, String> {
    let svc = Arc::clone(&*app.state::<Arc<TorrentService>>());
    let cap = svc.meta_cap();
    let p = PathBuf::from(&path);
    let len = std::fs::metadata(&p)
        .map_err(|_| "That file could not be read.".to_owned())?
        .len();
    if len > cap as u64 {
        return Err("This .torrent file is larger than the limit in Settings > Torrents.".into());
    }
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(p))
        .await
        .map_err(|_| "That file could not be read.".to_owned())?
        .map_err(|_| "That file could not be read.".to_owned())?;
    let m = syncwatch_torrent::meta::inspect(&bytes, cap, None)
        .map_err(|e| meta_error_text(&e).to_owned())?;
    let info = syncwatch_torrent::TorrentInfo::from_meta(&m);
    svc.cache_meta(info.infohash, info.torrent.clone());
    Ok(info_dto(info))
}

pub fn meta_error_text(e: &syncwatch_torrent::meta::MetaError) -> &'static str {
    use syncwatch_torrent::meta::MetaError as E;
    match e {
        E::TooLarge => "This .torrent file is larger than the limit in Settings > Torrents.",
        E::Malformed => "This is not a valid .torrent file.",
        E::TooDeep | E::TooMany => "This .torrent file is too complex and was refused.",
        E::NotUtf8 => "A file name in this torrent is not valid text; it was refused.",
        E::UnsafePath => "This torrent contains an unsafe file name or path and was refused.",
        E::HashMismatch => "The torrent metadata does not belong to this torrent and was refused.",
        E::NoFiles => "This torrent has no files.",
    }
}

pub fn engine_error_text(e: &syncwatch_torrent::EngineError) -> &'static str {
    use syncwatch_torrent::EngineError as E;
    match e {
        E::BadMagnet => "This is not a valid magnet link.",
        E::MetadataTimeout => {
            "No peer sent the torrent's file list in time. Likely causes: nobody is sharing this torrent right now, a firewall blocks the connections, or DHT is off (Settings > Torrents). Try again later, turn DHT on, or use the .torrent file instead of the magnet link."
        }
        E::MetaMismatch => "The torrent metadata does not belong to this torrent and was refused.",
        E::MetaTooLarge => "The torrent metadata is larger than the limit in Settings > Torrents.",
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
    svc.cache_meta(info.infohash, info.torrent.clone());
    Ok(info_dto(info))
}

#[tauri::command]
pub fn torrent_settings_get(app: AppHandle) -> TorrentSettings {
    app.state::<Arc<TorrentService>>().settings()
}

#[tauri::command]
pub fn torrent_settings_set(app: AppHandle, settings: TorrentSettings) -> Result<(), String> {
    settings.validate()?;
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
    fn numeric_settings_are_validated_and_reach_the_engine() {
        let d = TorrentSettings::default();
        assert_eq!(
            (
                d.meta_cap_mb,
                d.window_before_secs,
                d.window_after_secs,
                d.startup_kb
            ),
            (5, 1, 5, 1024)
        );
        assert!(d.validate().is_ok());
        for bad in [
            TorrentSettings {
                meta_cap_mb: 0,
                ..d.clone()
            },
            TorrentSettings {
                meta_cap_mb: 17,
                ..d.clone()
            },
            TorrentSettings {
                window_before_secs: 11,
                ..d.clone()
            },
            TorrentSettings {
                window_after_secs: 1,
                ..d.clone()
            },
            TorrentSettings {
                window_after_secs: 61,
                ..d.clone()
            },
            TorrentSettings {
                startup_kb: 100,
                ..d.clone()
            },
            TorrentSettings {
                startup_kb: 9000,
                ..d.clone()
            },
            TorrentSettings {
                bind: "nope".into(),
                ..d.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        let s = TorrentSettings {
            window_before_secs: 2,
            window_after_secs: 10,
            startup_kb: 512,
            meta_cap_mb: 8,
            ..d
        };
        let c = config_for(Path::new("x"), &s);
        assert_eq!(
            (c.ready.back_ms, c.ready.window_ms, c.ready.startup_bytes),
            (2000, 10_000, 512 * 1024)
        );
        assert_eq!(c.meta_cap, 8 * 1024 * 1024);
        assert!(c.ready.keep_ms < c.ready.window_ms);
        // Defaults match the engine's own defaults (ADR 0012).
        let e = config_for(Path::new("x"), &TorrentSettings::default());
        let r = syncwatch_torrent::ReadyConfig::default();
        assert_eq!(
            (
                e.ready.back_ms,
                e.ready.window_ms,
                e.ready.keep_ms,
                e.ready.startup_bytes
            ),
            (r.back_ms, r.window_ms, r.keep_ms, r.startup_bytes)
        );
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
            E::MetaMismatch,
            E::MetaTooLarge,
        ] {
            let t = engine_error_text(&e);
            assert!(t.ends_with('.') && t.len() > 20, "{e:?}");
        }
    }
}

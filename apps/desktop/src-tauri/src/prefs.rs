//! App preferences (Milestone 8 Settings): Playback, Room defaults, Chat and Files defaults.
//! Persisted in config/prefs.json. Nothing from a room is stored here (ADR 0006): these are the
//! user's own defaults. Validated on save; out-of-range values are refused with a friendly text.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};

use crate::paths::AppPaths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppPrefs {
    /// Hardware decoding (mpv `hwdec=auto-safe`, software fallback); off = software only.
    /// Applies the next time SyncWatch starts.
    pub hwdec: bool,
    /// Subtitle size for text subtitles, percent (mpv `sub-scale`). Image subtitles (PGS) are
    /// not scaled.
    pub sub_scale_pct: u32,
    /// Who controls playback in a new room: "everyone" or "onlyMe".
    pub preset: String,
    /// Ready gate: how long the room waits for a member before resuming without them.
    pub gate_timeout_secs: u32,
    /// "No size limit" for added external files, on by default in new rooms.
    pub no_limit_default: bool,
    /// Manual invites (ADR 0014): how long an invite stays valid, minutes.
    pub invite_ttl_mins: u32,
    /// Manual invites: how long a dropped member's seat is kept for a re-invite, minutes.
    pub seat_keep_mins: u32,
}

impl Default for AppPrefs {
    fn default() -> Self {
        Self {
            hwdec: true,
            sub_scale_pct: 100,
            preset: "everyone".into(),
            gate_timeout_secs: 15,
            no_limit_default: false,
            invite_ttl_mins: 10,
            seat_keep_mins: 5,
        }
    }
}

pub const SUB_SCALE_PCT: (u32, u32) = (50, 200);
pub const GATE_TIMEOUT_SECS: (u32, u32) = (5, 120);
pub const INVITE_TTL_MINS: (u32, u32) = (1, 1440);
pub const SEAT_KEEP_MINS: (u32, u32) = (1, 60);

impl AppPrefs {
    pub fn validate(&self) -> Result<(), String> {
        let range = |v: u32, (lo, hi): (u32, u32), what: &str| {
            if v < lo || v > hi {
                Err(format!("{what} must be between {lo} and {hi}."))
            } else {
                Ok(())
            }
        };
        range(self.sub_scale_pct, SUB_SCALE_PCT, "The subtitle size (%)")?;
        range(
            self.gate_timeout_secs,
            GATE_TIMEOUT_SECS,
            "The wait for members (seconds)",
        )?;
        range(
            self.invite_ttl_mins,
            INVITE_TTL_MINS,
            "How long an invite is valid (minutes)",
        )?;
        range(
            self.seat_keep_mins,
            SEAT_KEEP_MINS,
            "How long a seat is kept (minutes)",
        )?;
        if !matches!(self.preset.as_str(), "everyone" | "onlyMe") {
            return Err("Unknown permission preset.".into());
        }
        Ok(())
    }

    pub fn mpv_hwdec(&self) -> &'static str {
        if self.hwdec { "auto-safe" } else { "no" }
    }

    pub fn gate_timeout_us(&self) -> i64 {
        i64::from(
            self.gate_timeout_secs
                .clamp(GATE_TIMEOUT_SECS.0, GATE_TIMEOUT_SECS.1),
        ) * 1_000_000
    }
}

pub struct PrefsService {
    path: PathBuf,
    prefs: Mutex<AppPrefs>,
}

impl PrefsService {
    pub fn load(paths: &AppPaths) -> Self {
        let path = paths.config.join("prefs.json");
        let prefs = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<AppPrefs>(&b).ok())
            .filter(|p| p.validate().is_ok())
            .unwrap_or_default();
        Self {
            path,
            prefs: Mutex::new(prefs),
        }
    }

    pub fn get(&self) -> AppPrefs {
        self.prefs.lock().map(|g| g.clone()).unwrap_or_default()
    }

    fn save(&self, p: AppPrefs) -> Result<(), String> {
        p.validate()?;
        let bytes = serde_json::to_vec_pretty(&p).map_err(|e| e.to_string())?;
        std::fs::write(&self.path, bytes)
            .map_err(|_| "Could not save the settings.".to_string())?;
        if let Ok(mut g) = self.prefs.lock() {
            *g = p;
        }
        Ok(())
    }
}

#[tauri::command]
pub fn prefs_get(app: AppHandle) -> AppPrefs {
    app.state::<PrefsService>().get()
}

#[tauri::command]
pub fn prefs_set(app: AppHandle, prefs: AppPrefs) -> Result<(), String> {
    app.state::<PrefsService>().save(prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_partial_json_and_validation() {
        let d = AppPrefs::default();
        assert!(d.validate().is_ok());
        assert_eq!(
            (d.mpv_hwdec(), d.gate_timeout_us()),
            ("auto-safe", 15_000_000)
        );
        let p: AppPrefs = serde_json::from_str(r#"{"hwdec": false}"#).unwrap_or_default();
        assert_eq!(
            (p.mpv_hwdec(), p.sub_scale_pct, p.preset.as_str()),
            ("no", 100, "everyone")
        );
        for bad in [
            AppPrefs {
                sub_scale_pct: 49,
                ..d.clone()
            },
            AppPrefs {
                sub_scale_pct: 201,
                ..d.clone()
            },
            AppPrefs {
                gate_timeout_secs: 4,
                ..d.clone()
            },
            AppPrefs {
                gate_timeout_secs: 121,
                ..d.clone()
            },
            AppPrefs {
                preset: "x".into(),
                ..d.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}

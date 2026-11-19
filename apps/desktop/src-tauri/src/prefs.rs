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
    /// Check GitHub for a newer release when SyncWatch starts (Milestone 9, Settings, About).
    pub check_updates: bool,
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
            check_updates: true,
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
        // First start: the installer's default-config.toml (next to the exe) gives the defaults.
        let prefs = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<AppPrefs>(&b).ok())
            .filter(|p| p.validate().is_ok())
            .unwrap_or_else(|| {
                std::fs::read_to_string(paths.exe_dir.join(DEFAULT_CONFIG))
                    .map(|t| defaults_from_toml(&t))
                    .unwrap_or_default()
            });
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

/// Shipped next to syncwatch.exe; read only on the first start (no prefs.json yet).
pub const DEFAULT_CONFIG: &str = "default-config.toml";

/// Defaults from `default-config.toml`: plain `key = value` lines (integers, true/false,
/// "strings"), comments with #. Unknown keys and invalid values are ignored; the result is
/// validated as a whole (anything out of range: the built-in defaults). It never holds
/// credentials or server addresses.
pub fn defaults_from_toml(text: &str) -> AppPrefs {
    let mut p = AppPrefs::default();
    for line in text.lines().take(200) {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let num = || v.parse::<u32>().ok();
        let flag = || match v {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        match k {
            "hwdec" => p.hwdec = flag().unwrap_or(p.hwdec),
            "sub_scale_pct" => p.sub_scale_pct = num().unwrap_or(p.sub_scale_pct),
            "gate_timeout_secs" => p.gate_timeout_secs = num().unwrap_or(p.gate_timeout_secs),
            "no_limit_default" => p.no_limit_default = flag().unwrap_or(p.no_limit_default),
            "invite_ttl_mins" => p.invite_ttl_mins = num().unwrap_or(p.invite_ttl_mins),
            "seat_keep_mins" => p.seat_keep_mins = num().unwrap_or(p.seat_keep_mins),
            "check_updates" => p.check_updates = flag().unwrap_or(p.check_updates),
            "preset" => {
                let s = v.trim_matches('"');
                if matches!(s, "everyone" | "onlyMe") {
                    p.preset = s.to_owned();
                }
            }
            _ => {}
        }
    }
    if p.validate().is_ok() {
        p
    } else {
        AppPrefs::default()
    }
}

#[cfg(test)]
mod default_config_tests {
    use super::*;

    #[test]
    fn shipped_default_config_gives_the_built_in_defaults() {
        let shipped = include_str!("../default-config.toml");
        assert_eq!(defaults_from_toml(shipped), AppPrefs::default());
        // It never names a server or holds a credential.
        let lower = shipped.to_lowercase();
        for bad in [
            "metered", "api_key", "apikey", "password", "wss:", "ws:", "turn:", "signal",
        ] {
            assert!(!lower.contains(bad), "{bad} in default-config.toml");
        }
    }

    #[test]
    fn values_are_read_and_bad_ones_ignored() {
        let p = defaults_from_toml(
            "invite_ttl_mins = 30\ncheck_updates = false # note\npreset = \"onlyMe\"\nunknown = 5\nhwdec = maybe\n",
        );
        assert_eq!(
            (
                p.invite_ttl_mins,
                p.check_updates,
                p.preset.as_str(),
                p.hwdec
            ),
            (30, false, "onlyMe", true)
        );
        // Out of range: all built-in defaults.
        assert_eq!(
            defaults_from_toml("seat_keep_mins = 9999"),
            AppPrefs::default()
        );
    }
}

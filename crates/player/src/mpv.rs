//! libmpv-backed [`Player`].
//!
//! mpv renders into a window we own (passed as `wid`); see ADR 0001. libmpv's render API is
//! deliberately NOT used. mpv's own OSD, OSC, scripts and input handling are disabled: all UI
//! is drawn by the app.
//!
//! The libmpv DLL must already be loaded by absolute path before [`MpvPlayer::new`] is called
//! (the executable delay-loads `libmpv-2.dll`; see ADR 0002).

use crate::tracks::{RawValue, read_track_list};
use crate::{
    EndReason, PlaybackState, Player, PlayerError, PlayerEvent, Result, Track, TrackKind,
    TrackSelection, path_to_mpv,
};
use libmpv2::events::{Event, PropertyData};
use libmpv2::{Format, Mpv};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Startup configuration for [`MpvPlayer`].
#[derive(Debug, Clone)]
pub struct MpvConfig {
    /// Native window handle mpv renders into.
    pub wid: i64,
    /// Folder with bundled subtitle fallback fonts (Noto Sans).
    pub sub_fonts_dir: Option<PathBuf>,
    /// Hardware decoding mode; `auto-safe` with software fallback by default.
    pub hwdec: String,
}

impl MpvConfig {
    pub fn new(wid: i64) -> Self {
        Self {
            wid,
            sub_fonts_dir: None,
            hwdec: "auto-safe".to_owned(),
        }
    }
}

/// Options applied before `mpv_initialize`. `true` = essential (failure aborts startup).
fn init_options(cfg: &MpvConfig) -> Vec<(&'static str, String, bool)> {
    let mut v = vec![
        ("wid", cfg.wid.to_string(), true),
        ("osd-level", "0".into(), true),
        ("osc", "no".into(), false),
        ("load-scripts", "no".into(), false),
        ("ytdl", "no".into(), false),
        ("input-default-bindings", "no".into(), false),
        ("input-vo-keyboard", "no".into(), false),
        ("input-cursor", "no".into(), false),
        ("cursor-autohide", "no".into(), false),
        ("idle", "yes".into(), true),
        ("keep-open", "yes".into(), true),
        ("pause", "yes".into(), true),
        ("hwdec", cfg.hwdec.clone(), false),
        ("background-color", "#000000".into(), false),
        ("cache-pause", "yes".into(), false),
        ("network-timeout", "60".into(), false),
        ("sub-font", "Noto Sans".into(), false),
        ("terminal", "no".into(), false),
        ("msg-level", "all=warn".into(), false),
    ];
    if let Some(dir) = &cfg.sub_fonts_dir
        && let Some(s) = dir.to_str()
    {
        v.push(("sub-fonts-dir", s.to_owned(), false));
    }
    v
}

const OBSERVED: &[(&str, Format)] = &[
    ("pause", Format::Flag),
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("speed", Format::Double),
    ("paused-for-cache", Format::Flag),
    ("eof-reached", Format::Flag),
    ("volume", Format::Double),
    ("mute", Format::Flag),
    ("demuxer-cache-duration", Format::Double),
    ("track-list/count", Format::Int64),
    ("aid", Format::String),
    ("sid", Format::String),
];

/// Minimum interval between `State` events caused only by `time-pos` ticks.
const TIME_POS_THROTTLE: Duration = Duration::from_millis(100);

struct Shared {
    state: PlaybackState,
    tracks: Vec<Track>,
}

pub struct MpvPlayer {
    mpv: Arc<Mpv>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn cmd_err(e: libmpv2::Error) -> PlayerError {
    PlayerError::Command(e.to_string())
}

impl MpvPlayer {
    /// Creates the mpv core and its event thread.
    pub fn new(cfg: MpvConfig) -> Result<(Self, Receiver<PlayerEvent>)> {
        let opts = init_options(&cfg);
        let mpv = Mpv::with_initializer(|init| {
            for (name, value, essential) in &opts {
                if let Err(e) = init.set_option(name, value.as_str()) {
                    if *essential {
                        return Err(e);
                    }
                    tracing::warn!(option = name, error = %e, "optional mpv option rejected");
                }
            }
            Ok(())
        })
        .map_err(|e| PlayerError::Init(e.to_string()))?;

        let _ = mpv.disable_deprecated_events();
        for (i, (name, fmt)) in OBSERVED.iter().enumerate() {
            mpv.observe_property(name, *fmt, i as u64 + 1)
                .map_err(|e| PlayerError::Init(format!("observe {name}: {e}")))?;
        }

        let mpv = Arc::new(mpv);
        let shared = Arc::new(Mutex::new(Shared {
            state: PlaybackState {
                paused: true,
                speed: 1.0,
                volume: 100.0,
                ..PlaybackState::default()
            },
            tracks: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();

        let thread = {
            let mpv = Arc::clone(&mpv);
            let shared = Arc::clone(&shared);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("mpv-events".into())
                .spawn(move || event_loop(&mpv, &shared, &stop, &tx))
                .map_err(|e| PlayerError::Init(e.to_string()))?
        };

        Ok((
            Self {
                mpv,
                shared,
                stop,
                thread: Some(thread),
            },
            rx,
        ))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.mpv.command("quit", &[]);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_tracks(mpv: &Mpv) -> Vec<Track> {
    read_track_list(|name| {
        let int_suffix = ["/id", "/ff-index", "/count", "/demux-channel-count"];
        let flag_suffix = ["/selected", "/external", "/default", "/forced"];
        if int_suffix.iter().any(|s| name.ends_with(s)) {
            mpv.get_property::<i64>(name).ok().map(RawValue::Int)
        } else if flag_suffix.iter().any(|s| name.ends_with(s)) {
            mpv.get_property::<bool>(name).ok().map(RawValue::Flag)
        } else {
            mpv.get_property::<String>(name).ok().map(RawValue::Str)
        }
    })
}

fn end_reason(r: libmpv2::EndFileReason) -> EndReason {
    use libmpv2::mpv_end_file_reason as R;
    match r {
        R::Eof => EndReason::Eof,
        R::Stop => EndReason::Stop,
        R::Quit => EndReason::Quit,
        R::Error => EndReason::Error,
        R::Redirect => EndReason::Redirect,
        _ => EndReason::Unknown,
    }
}

fn event_loop(mpv: &Mpv, shared: &Mutex<Shared>, stop: &AtomicBool, tx: &Sender<PlayerEvent>) {
    let lock = || shared.lock().unwrap_or_else(|p| p.into_inner());
    let mut last_state_emit = Instant::now() - TIME_POS_THROTTLE;
    let mut pending_time_tick = false;
    while !stop.load(Ordering::SeqCst) {
        // `None` means: queue empty for this timeout, OR an unavailable property changed.
        let Some(ev) = mpv.wait_event(0.25) else {
            if pending_time_tick && last_state_emit.elapsed() >= TIME_POS_THROTTLE {
                pending_time_tick = false;
                last_state_emit = Instant::now();
                let _ = tx.send(PlayerEvent::State(lock().state.clone()));
            }
            continue;
        };
        let ev = match ev {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!(error = %e, "mpv event error");
                continue;
            }
        };
        match ev {
            Event::Shutdown => {
                let _ = tx.send(PlayerEvent::Shutdown);
                break;
            }
            Event::FileLoaded => {
                let tracks = read_tracks(mpv);
                let state = {
                    let mut g = lock();
                    g.tracks = tracks.clone();
                    g.state.file_loaded = true;
                    g.state.eof_reached = false;
                    g.state.clone()
                };
                let _ = tx.send(PlayerEvent::FileLoaded { tracks });
                let _ = tx.send(PlayerEvent::State(state));
            }
            Event::PlaybackRestart => {
                let _ = tx.send(PlayerEvent::SeekDone);
            }
            Event::EndFile(r) => {
                let reason = end_reason(r);
                if reason != EndReason::Redirect {
                    let mut g = lock();
                    g.state.file_loaded = false;
                    g.state.time_pos = None;
                    g.state.duration = None;
                    // No file, no tracks: a later snapshot must not show the old ones.
                    g.tracks.clear();
                }
                let _ = tx.send(PlayerEvent::EndFile { reason });
                if reason != EndReason::Redirect {
                    let _ = tx.send(PlayerEvent::TracksChanged { tracks: Vec::new() });
                    let _ = tx.send(PlayerEvent::State(lock().state.clone()));
                }
            }
            Event::PropertyChange { name, change, .. } => {
                let mut tracks_changed = false;
                let mut only_time = false;
                {
                    let mut g = lock();
                    let s = &mut g.state;
                    match (name, change) {
                        ("pause", PropertyData::Flag(v)) => s.paused = v,
                        ("time-pos", PropertyData::Double(v)) => {
                            s.time_pos = Some(v);
                            only_time = true;
                        }
                        ("duration", PropertyData::Double(v)) => s.duration = Some(v),
                        ("speed", PropertyData::Double(v)) => s.speed = v,
                        ("paused-for-cache", PropertyData::Flag(v)) => s.paused_for_cache = v,
                        ("eof-reached", PropertyData::Flag(v)) => s.eof_reached = v,
                        ("volume", PropertyData::Double(v)) => s.volume = v,
                        ("mute", PropertyData::Flag(v)) => s.mute = v,
                        ("demuxer-cache-duration", PropertyData::Double(v)) => {
                            s.cache_ahead = Some(v);
                            only_time = true;
                        }
                        ("track-list/count", _) | ("aid", _) | ("sid", _) => tracks_changed = true,
                        _ => {}
                    }
                }
                if tracks_changed && lock().state.file_loaded {
                    let tracks = read_tracks(mpv);
                    lock().tracks = tracks.clone();
                    let _ = tx.send(PlayerEvent::TracksChanged { tracks });
                } else if only_time && last_state_emit.elapsed() < TIME_POS_THROTTLE {
                    pending_time_tick = true;
                } else if !tracks_changed {
                    pending_time_tick = false;
                    last_state_emit = Instant::now();
                    let _ = tx.send(PlayerEvent::State(lock().state.clone()));
                }
            }
            _ => {}
        }
    }
}

impl Player for MpvPlayer {
    fn load(&self, source: &str) -> Result<()> {
        self.mpv.set_property("pause", true).map_err(cmd_err)?;
        self.mpv
            .command("loadfile", &[source, "replace"])
            .map_err(cmd_err)
    }
    fn unload(&self) -> Result<()> {
        // `stop` ends the current file; with idle=yes the player stays alive with nothing loaded.
        // The EndFile event then clears the state and the track list.
        self.mpv.command("stop", &[]).map_err(cmd_err)?;
        let mut g = self.lock();
        g.state.file_loaded = false;
        g.state.time_pos = None;
        g.state.duration = None;
        g.tracks.clear();
        Ok(())
    }
    fn set_paused(&self, paused: bool) -> Result<()> {
        self.mpv.set_property("pause", paused).map_err(cmd_err)
    }
    fn seek(&self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() {
            return Err(PlayerError::Command("seek target is not a number".into()));
        }
        let t = format!("{:.3}", seconds.max(0.0));
        self.mpv
            .command("seek", &[&t, "absolute+exact"])
            .map_err(cmd_err)
    }
    fn seek_relative(&self, delta: f64) -> Result<()> {
        if !delta.is_finite() {
            return Err(PlayerError::Command("seek offset is not a number".into()));
        }
        let d = format!("{delta:+.3}");
        self.mpv
            .command("seek", &[&d, "relative+exact"])
            .map_err(cmd_err)
    }
    fn set_speed(&self, speed: f64) -> Result<()> {
        self.mpv
            .set_property("speed", speed.clamp(0.25, 4.0))
            .map_err(cmd_err)
    }
    fn set_volume(&self, volume: f64) -> Result<()> {
        self.mpv
            .set_property("volume", volume.clamp(0.0, 130.0))
            .map_err(cmd_err)
    }
    fn set_mute(&self, mute: bool) -> Result<()> {
        self.mpv.set_property("mute", mute).map_err(cmd_err)
    }
    fn select_track(&self, kind: TrackKind, selection: TrackSelection) -> Result<()> {
        if kind == TrackKind::Video {
            return Err(PlayerError::Command(
                "video track selection is not supported".into(),
            ));
        }
        self.mpv
            .set_property(kind.select_property(), selection.to_mpv_value().as_str())
            .map_err(cmd_err)
    }
    fn add_external(&self, kind: TrackKind, path: &Path, select: bool) -> Result<()> {
        let p = path_to_mpv(path)?;
        let cmd = match kind {
            TrackKind::Sub => "sub-add",
            TrackKind::Audio => "audio-add",
            TrackKind::Video => {
                return Err(PlayerError::Command(
                    "external video is not supported".into(),
                ));
            }
        };
        // `auto`: added but not selected (in a room only the host's selection decides).
        let flag = if select { "select" } else { "auto" };
        self.mpv.command(cmd, &[&p, flag]).map_err(cmd_err)
    }
    fn set_sub_delay(&self, seconds: f64) -> Result<()> {
        self.mpv.set_property("sub-delay", seconds).map_err(cmd_err)
    }
    fn set_audio_delay(&self, seconds: f64) -> Result<()> {
        self.mpv
            .set_property("audio-delay", seconds)
            .map_err(cmd_err)
    }
    fn tracks(&self) -> Vec<Track> {
        self.lock().tracks.clone()
    }
    fn state(&self) -> PlaybackState {
        self.lock().state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essential_options_disable_osd_and_start_paused() {
        let o = init_options(&MpvConfig::new(42));
        let get = |k: &str| {
            o.iter()
                .find(|(n, _, _)| *n == k)
                .map(|(_, v, e)| (v.clone(), *e))
        };
        assert_eq!(get("wid"), Some(("42".into(), true)));
        assert_eq!(get("osd-level"), Some(("0".into(), true)));
        assert_eq!(get("pause"), Some(("yes".into(), true)));
        assert_eq!(get("hwdec"), Some(("auto-safe".into(), false)));
        assert!(get("sub-fonts-dir").is_none());
    }
}

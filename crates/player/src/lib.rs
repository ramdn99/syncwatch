//! SyncWatch player abstraction.
//!
//! [`Player`] is the only interface the rest of the app uses to talk to a media player.
//! [`mpv::MpvPlayer`] drives the bundled libmpv; [`mock::MockPlayer`] records calls and lets
//! tests inject events. No implementation ever shells out to an external binary.

pub mod mock;
#[cfg(feature = "libmpv")]
pub mod mpv;
pub mod tracks;

use serde::{Deserialize, Serialize};
use std::path::Path;

pub use tracks::{Track, TrackKind, TrackSelection};

/// Errors surfaced by a [`Player`]. Messages are safe to log (no secrets, paths are redacted
/// by the caller's log layer if needed).
#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("the player engine could not be started: {0}")]
    Init(String),
    #[error("player command failed: {0}")]
    Command(String),
    #[error("path is not valid UTF-8")]
    NonUtf8Path,
    #[error("the player has shut down")]
    Shutdown,
}

pub type Result<T> = std::result::Result<T, PlayerError>;

/// Snapshot of the observable playback state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackState {
    pub paused: bool,
    /// Current position in seconds, `None` when nothing is loaded.
    pub time_pos: Option<f64>,
    pub duration: Option<f64>,
    pub speed: f64,
    pub paused_for_cache: bool,
    pub eof_reached: bool,
    pub volume: f64,
    pub mute: bool,
    pub file_loaded: bool,
    /// Seconds of demuxer cache ahead of the playhead (from `demuxer-cache-state`).
    pub cache_ahead: Option<f64>,
}

/// Why playback of a file ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Unknown,
}

/// Events emitted by a player, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PlayerEvent {
    /// A file was opened and its tracks are known (`file-loaded`).
    FileLoaded { tracks: Vec<Track> },
    /// The track list changed (external track added, selection changed).
    TracksChanged { tracks: Vec<Track> },
    /// One or more playback properties changed.
    State(PlaybackState),
    /// Playback of the current file ended.
    EndFile { reason: EndReason },
    /// The player engine shut down.
    Shutdown,
}

/// The player interface used by the app. All methods are non-blocking; results of commands
/// arrive as [`PlayerEvent`]s.
pub trait Player: Send + Sync {
    /// Opens a local file (or, later, a loopback URL) and starts paused.
    fn load(&self, source: &str) -> Result<()>;
    fn set_paused(&self, paused: bool) -> Result<()>;
    /// Absolute seek in seconds.
    fn seek(&self, seconds: f64) -> Result<()>;
    fn set_speed(&self, speed: f64) -> Result<()>;
    fn set_volume(&self, volume: f64) -> Result<()>;
    fn set_mute(&self, mute: bool) -> Result<()>;
    /// Selects an audio or subtitle track by mpv track id, or turns it off.
    fn select_track(&self, kind: TrackKind, selection: TrackSelection) -> Result<()>;
    /// Adds an external audio or subtitle file and selects it.
    fn add_external(&self, kind: TrackKind, path: &Path) -> Result<()>;
    fn set_sub_delay(&self, seconds: f64) -> Result<()>;
    fn set_audio_delay(&self, seconds: f64) -> Result<()>;
    /// Current tracks (cached from the last event).
    fn tracks(&self) -> Vec<Track>;
    /// Current playback state (cached from the last event).
    fn state(&self) -> PlaybackState;
}

/// Converts a path to the UTF-8 string mpv expects.
pub fn path_to_mpv(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(PlayerError::NonUtf8Path)
}

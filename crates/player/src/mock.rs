//! A mock [`Player`] that records calls and lets tests inject events.

use crate::{
    PlaybackState, Player, PlayerEvent, Result, Track, TrackKind, TrackSelection, path_to_mpv,
};
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

/// A call recorded by [`MockPlayer`].
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    Load(String),
    SetPaused(bool),
    Seek(f64),
    SeekRelative(f64),
    SetSpeed(f64),
    SetVolume(f64),
    SetMute(bool),
    SelectTrack(TrackKind, TrackSelection),
    AddExternal(TrackKind, String),
    SubDelay(f64),
    AudioDelay(f64),
}

#[derive(Default)]
struct Inner {
    calls: Vec<MockCall>,
    tracks: Vec<Track>,
    state: PlaybackState,
}

pub struct MockPlayer {
    inner: Mutex<Inner>,
    tx: Sender<PlayerEvent>,
}

impl MockPlayer {
    /// Creates the mock and the receiver of its events.
    pub fn new() -> (Self, Receiver<PlayerEvent>) {
        let (tx, rx) = channel();
        let inner = Inner {
            state: PlaybackState {
                paused: true,
                speed: 1.0,
                volume: 100.0,
                ..PlaybackState::default()
            },
            ..Inner::default()
        };
        (
            Self {
                inner: Mutex::new(inner),
                tx,
            },
            rx,
        )
    }

    /// All calls recorded so far.
    pub fn calls(&self) -> Vec<MockCall> {
        self.lock().calls.clone()
    }

    /// Simulates mpv's `file-loaded` with the given tracks.
    pub fn emit_file_loaded(&self, tracks: Vec<Track>) {
        {
            let mut g = self.lock();
            g.tracks = tracks.clone();
            g.state.file_loaded = true;
        }
        let _ = self.tx.send(PlayerEvent::FileLoaded { tracks });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned mock only happens after a panicking test; recover the data.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn record(&self, call: MockCall) {
        self.lock().calls.push(call);
    }

    fn emit_state(&self) {
        let s = self.lock().state.clone();
        let _ = self.tx.send(PlayerEvent::State(s));
    }
}

impl Player for MockPlayer {
    fn load(&self, source: &str) -> Result<()> {
        self.record(MockCall::Load(source.to_owned()));
        Ok(())
    }
    fn set_paused(&self, paused: bool) -> Result<()> {
        self.record(MockCall::SetPaused(paused));
        self.lock().state.paused = paused;
        self.emit_state();
        Ok(())
    }
    fn seek(&self, seconds: f64) -> Result<()> {
        self.record(MockCall::Seek(seconds));
        self.lock().state.time_pos = Some(seconds);
        self.emit_state();
        Ok(())
    }
    fn seek_relative(&self, delta: f64) -> Result<()> {
        self.record(MockCall::SeekRelative(delta));
        {
            let mut g = self.lock();
            let pos = g.state.time_pos.unwrap_or(0.0);
            g.state.time_pos = Some((pos + delta).max(0.0));
        }
        self.emit_state();
        Ok(())
    }
    fn set_speed(&self, speed: f64) -> Result<()> {
        self.record(MockCall::SetSpeed(speed));
        self.lock().state.speed = speed;
        self.emit_state();
        Ok(())
    }
    fn set_volume(&self, volume: f64) -> Result<()> {
        self.record(MockCall::SetVolume(volume));
        self.lock().state.volume = volume;
        Ok(())
    }
    fn set_mute(&self, mute: bool) -> Result<()> {
        self.record(MockCall::SetMute(mute));
        self.lock().state.mute = mute;
        Ok(())
    }
    fn select_track(&self, kind: TrackKind, selection: TrackSelection) -> Result<()> {
        self.record(MockCall::SelectTrack(kind, selection));
        let tracks = {
            let mut g = self.lock();
            for t in g.tracks.iter_mut().filter(|t| t.kind == kind) {
                t.selected = selection == TrackSelection::Id(t.id);
            }
            g.tracks.clone()
        };
        let _ = self.tx.send(PlayerEvent::TracksChanged { tracks });
        Ok(())
    }
    fn add_external(&self, kind: TrackKind, path: &Path) -> Result<()> {
        self.record(MockCall::AddExternal(kind, path_to_mpv(path)?));
        Ok(())
    }
    fn set_sub_delay(&self, seconds: f64) -> Result<()> {
        self.record(MockCall::SubDelay(seconds));
        Ok(())
    }
    fn set_audio_delay(&self, seconds: f64) -> Result<()> {
        self.record(MockCall::AudioDelay(seconds));
        Ok(())
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

    fn track(id: i64, kind: TrackKind, selected: bool) -> Track {
        Track {
            id,
            kind,
            ff_index: Some(id),
            lang: None,
            title: None,
            codec: None,
            selected,
            external: false,
            default: false,
            forced: false,
            channels: None,
        }
    }

    #[test]
    fn records_calls_and_selection_updates_tracks() {
        let (p, rx) = MockPlayer::new();
        p.emit_file_loaded(vec![
            track(1, TrackKind::Sub, true),
            track(2, TrackKind::Sub, false),
            track(1, TrackKind::Audio, true),
        ]);
        p.select_track(TrackKind::Sub, TrackSelection::Id(2))
            .unwrap();
        p.select_track(TrackKind::Sub, TrackSelection::Off).unwrap();
        assert!(matches!(rx.recv().unwrap(), PlayerEvent::FileLoaded { .. }));
        let PlayerEvent::TracksChanged { tracks } = rx.recv().unwrap() else {
            panic!("expected TracksChanged")
        };
        assert!(
            tracks
                .iter()
                .any(|t| t.kind == TrackKind::Sub && t.id == 2 && t.selected)
        );
        // Audio selection is untouched by a subtitle change.
        assert!(
            tracks
                .iter()
                .any(|t| t.kind == TrackKind::Audio && t.selected)
        );
        let PlayerEvent::TracksChanged { tracks } = rx.recv().unwrap() else {
            panic!("expected TracksChanged")
        };
        assert!(
            !tracks
                .iter()
                .any(|t| t.kind == TrackKind::Sub && t.selected)
        );
        assert_eq!(p.calls().len(), 2);
    }

    #[test]
    fn events_serialize_with_tag() {
        let ev = PlayerEvent::EndFile {
            reason: crate::EndReason::Eof,
        };
        let s = serde_json::to_string(&ev).unwrap();
        assert_eq!(s, r#"{"type":"endFile","reason":"eof"}"#);
    }
}

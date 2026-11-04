//! The LOCAL participant (host and members alike): follows the authoritative room playback with
//! the local player, owns the local media status ("Choose my file", magnet confirmation) and
//! applies the host's track state.
//!
//! Scheduling rule (regression for the "members started 300 ms early" bug): a room state whose
//! anchor lies in the future is HELD (paused at its position) until the anchor, no matter how it
//! arrived: a command, a heartbeat or a snapshot. Heartbeats never start playback early.

use crate::drift::{DriftAction, DriftController, IGNORE_MS};
use crate::timing::OBS_FRESH_US;
use crate::tracks::TrackApplier;
use crate::types::*;
use syncwatch_protocol::*;

#[derive(Debug, Clone, Copy)]
struct Obs {
    pos_ms: i64,
    mono_us: i64,
    paused: bool,
}

#[derive(Debug, Clone)]
pub struct Local {
    pub me: MemberId,
    pub descriptor: Option<MediaDescriptor>,
    pub room: RoomPlayback,
    pub media: MediaStatus,
    pub tracks: TrackApplier,
    pub duration_ms: Option<i64>,
    loaded: bool,
    load_requested: bool,
    panel_shown_for: Option<MediaEpoch>,
    /// Identity of a local file this participant chose before requesting a media change.
    own_pending: Option<FileIdentity>,
    pending_mismatch: Option<FileIdentity>,
    torrent_started: Option<Infohash>,
    torrent_streaming: bool,
    awaiting_magnet: bool,
    // --- playback following ---
    obs: Option<Obs>,
    applied_paused: Option<bool>,
    applied_speed: SpeedPermille,
    last_cmd_us: i64,
    pending_start: bool,
    awaiting_seek: Option<Epoch>,
    pub ready_epoch: Epoch,
    pub buffering: bool,
    drift: DriftController,
    drift_cooldown_until: i64,
    needs_resync: bool,
    /// When the last seek was issued (local mono) and the learned seek latency (EMA, us).
    seek_issued_us: Option<i64>,
    seek_latency_us: i64,
}

impl Local {
    pub fn new(me: MemberId, now_host: i64) -> Self {
        Self {
            me,
            descriptor: None,
            room: RoomPlayback::initial(MediaEpoch(0), now_host),
            media: MediaStatus::NoMedia,
            tracks: TrackApplier::default(),
            duration_ms: None,
            loaded: false,
            load_requested: false,
            panel_shown_for: None,
            own_pending: None,
            pending_mismatch: None,
            torrent_started: None,
            torrent_streaming: false,
            awaiting_magnet: false,
            obs: None,
            applied_paused: None,
            applied_speed: 1000,
            last_cmd_us: i64::MIN,
            pending_start: false,
            awaiting_seek: None,
            ready_epoch: Epoch(0),
            buffering: false,
            drift: DriftController::default(),
            drift_cooldown_until: i64::MIN,
            needs_resync: false,
            seek_issued_us: None,
            seek_latency_us: 100_000,
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn status(
        &self,
        rtt_ms: Option<u32>,
        track_override: Option<TrackSyncStatus>,
    ) -> MemberStatus {
        MemberStatus {
            media_epoch: self.room.media_epoch,
            media: self.media,
            ready_epoch: self.ready_epoch,
            buffering: self.buffering,
            tracks: track_override.unwrap_or(self.tracks.status),
            rtt_ms,
        }
    }

    /// Remember the identity of the file this participant picked for its own change request.
    pub fn set_own_pending(&mut self, id: Option<FileIdentity>) {
        self.own_pending = id;
    }

    /// Current local position estimate (for chat timestamps).
    pub fn position_estimate(&self, mono: i64) -> Option<MediaMs> {
        let o = self.obs?;
        if o.paused || self.buffering {
            return Some(o.pos_ms);
        }
        Some(o.pos_ms + (mono - o.mono_us) * self.applied_speed as i64 / 1_000_000)
    }

    // ------------------------------------------------------------------ media ------------

    /// Applies a MediaChange (or a join snapshot). Stops the old source completely.
    #[allow(clippy::too_many_arguments)]
    pub fn on_media_change(
        &mut self,
        descriptor: Option<MediaDescriptor>,
        playback: RoomPlayback,
        tracks: TrackState,
        requested_by: Option<MemberId>,
        host_now: Option<i64>,
        mono: i64,
        out: &mut Out,
    ) {
        if self.loaded || self.load_requested {
            out.player(PlayerCmd::Stop);
        }
        if let Some(ih) = self.torrent_started.take() {
            out.push(Effect::StopTorrent { infohash: ih });
        }
        self.loaded = false;
        self.load_requested = false;
        self.torrent_streaming = false;
        self.awaiting_magnet = false;
        self.pending_mismatch = None;
        self.obs = None;
        self.applied_paused = None;
        self.applied_speed = 1000;
        self.pending_start = false;
        self.awaiting_seek = None;
        self.buffering = false;
        self.drift.reset();
        self.duration_ms = None;
        self.ready_epoch = Epoch(0);
        self.room = playback;
        self.tracks.reset(tracks);
        out.ui(UiEvent::MediaChanged {
            title: descriptor.as_ref().map(|d| d.display_name().to_owned()),
        });
        let own = requested_by == Some(self.me);
        self.descriptor = descriptor.clone();
        match descriptor {
            None => self.media = MediaStatus::NoMedia,
            Some(MediaDescriptor::LocalFile(l)) => {
                if own && self.own_pending.is_some_and(|p| p.matches(&l.identity)) {
                    self.media = MediaStatus::Ready;
                    self.request_load(PlayerCmd::LoadLocal, out);
                } else {
                    self.media = MediaStatus::Missing;
                    self.show_panel_once(&l, out);
                }
            }
            Some(MediaDescriptor::Torrent(t)) => {
                self.media = MediaStatus::Missing;
                if own {
                    self.start_torrent(&t, out);
                } else {
                    self.awaiting_magnet = true;
                    out.ui(UiEvent::ConfirmMagnet {
                        name: t.name.clone(),
                        size: t.size,
                        infohash: t.infohash,
                    });
                }
            }
        }
        self.own_pending = None;
        let _ = (host_now, mono);
    }

    fn show_panel_once(&mut self, l: &LocalMedia, out: &mut Out) {
        if self.panel_shown_for != Some(self.room.media_epoch) {
            self.panel_shown_for = Some(self.room.media_epoch);
            out.ui(UiEvent::ShowMissingFilePanel {
                name: l.name.clone(),
                size: l.identity.size,
                duration_ms: l.identity.duration_ms,
            });
        }
    }

    fn request_load(&mut self, cmd: PlayerCmd, out: &mut Out) {
        if self.loaded || self.load_requested {
            out.player(PlayerCmd::Stop);
            self.loaded = false;
            self.tracks.on_unloaded();
        }
        self.load_requested = true;
        self.obs = None;
        self.applied_paused = None;
        out.player(cmd);
    }

    fn start_torrent(&mut self, t: &TorrentMedia, out: &mut Out) {
        self.awaiting_magnet = false;
        self.torrent_started = Some(t.infohash);
        self.media = MediaStatus::Downloading { permille: 0 };
        out.push(Effect::StartTorrent {
            infohash: t.infohash,
            file_index: t.file_index,
        });
    }

    /// "Choose my file": works for every member, no permission involved.
    pub fn on_local_file(&mut self, ev: LocalFileEvent, out: &mut Out) {
        let Some(MediaDescriptor::LocalFile(l)) = self.descriptor.clone() else {
            return;
        };
        match ev {
            LocalFileEvent::Verifying => self.media = MediaStatus::Verifying,
            LocalFileEvent::Failed => self.media = MediaStatus::Missing,
            LocalFileEvent::Identified(id) => {
                if id.matches(&l.identity) {
                    self.pending_mismatch = None;
                    self.media = MediaStatus::Ready;
                    out.ui(UiEvent::FileVerified);
                    self.request_load(PlayerCmd::LoadLocal, out);
                } else {
                    self.pending_mismatch = Some(id);
                    self.media = MediaStatus::Mismatch;
                    out.ui(UiEvent::FileMismatch {
                        expected: l.identity,
                        got: id,
                    });
                }
            }
        }
    }

    /// The member accepts a mismatching file; sync still applies.
    pub fn proceed_with_mismatch(&mut self, out: &mut Out) {
        if self.pending_mismatch.take().is_some() {
            self.media = MediaStatus::Mismatch;
            self.request_load(PlayerCmd::LoadLocal, out);
        }
    }

    pub fn accept_magnet(&mut self, out: &mut Out) {
        if let (true, Some(MediaDescriptor::Torrent(t))) =
            (self.awaiting_magnet, self.descriptor.clone())
        {
            self.start_torrent(&t, out);
        }
    }

    pub fn decline_magnet(&mut self) {
        if self.awaiting_magnet {
            self.awaiting_magnet = false;
            self.media = MediaStatus::Declined;
        }
    }

    pub fn on_torrent(&mut self, ev: TorrentEvent, out: &mut Out) {
        let Some(MediaDescriptor::Torrent(t)) = self.descriptor.clone() else {
            return;
        };
        if self.torrent_started != Some(t.infohash) {
            return;
        }
        match ev {
            TorrentEvent::Progress { permille } => {
                if !self.torrent_streaming {
                    self.media = MediaStatus::Downloading { permille };
                }
            }
            TorrentEvent::Playable => {
                if !self.torrent_streaming {
                    self.torrent_streaming = true;
                    self.media = MediaStatus::Ready;
                    self.request_load(
                        PlayerCmd::LoadTorrentStream {
                            infohash: t.infohash,
                            file_index: t.file_index,
                        },
                        out,
                    );
                }
            }
            TorrentEvent::Unavailable => self.media = MediaStatus::Unavailable,
        }
    }

    // --------------------------------------------------------------- player obs ----------

    pub fn on_player(&mut self, obs: PlayerObs, host_now: Option<i64>, mono: i64, out: &mut Out) {
        match obs {
            PlayerObs::FileLoaded {
                tracks,
                duration_ms,
            } => {
                if !self.load_requested {
                    return;
                }
                self.loaded = true;
                self.duration_ms = duration_ms;
                self.tracks.on_loaded(tracks, out);
                self.sync_player(host_now, mono, true, out);
            }
            PlayerObs::Tracks(t) => self.tracks.on_tracks(t, out),
            PlayerObs::Position {
                position_ms,
                paused,
                buffering,
            } => {
                self.obs = Some(Obs {
                    pos_ms: position_ms,
                    mono_us: mono,
                    paused,
                });
                self.buffering = buffering;
            }
            PlayerObs::SeekDone => {
                if let Some(t0) = self.seek_issued_us.take() {
                    // Learn how long this player takes to seek (EMA, bounded).
                    let took = (mono - t0).clamp(0, 2_000_000);
                    self.seek_latency_us = (self.seek_latency_us * 3 + took) / 4;
                }
                if let Some(e) = self.awaiting_seek.take() {
                    self.ready_epoch = self.ready_epoch.max(e);
                }
            }
            PlayerObs::LoadFailed => {
                self.loaded = false;
                self.load_requested = false;
                self.media = MediaStatus::Missing;
            }
        }
    }

    // ------------------------------------------------------------- room following --------

    /// A new authoritative room state (command, heartbeat with a newer epoch, or snapshot).
    pub fn apply_room(
        &mut self,
        new: RoomPlayback,
        host_now: Option<i64>,
        mono: i64,
        out: &mut Out,
    ) {
        if new.media_epoch != self.room.media_epoch || new.epoch < self.room.epoch {
            return; // stale or for another media
        }
        if new == self.room && !self.needs_resync {
            return;
        }
        self.room = new;
        self.sync_player(host_now, mono, false, out);
    }

    /// Heartbeats only matter when they carry a NEWER epoch (a missed command). Even then the
    /// anchor is respected, so a heartbeat can never start playback before a scheduled start.
    pub fn on_heartbeat(
        &mut self,
        hb: RoomPlayback,
        host_now: Option<i64>,
        mono: i64,
        out: &mut Out,
    ) {
        if hb.media_epoch == self.room.media_epoch && hb.epoch > self.room.epoch {
            self.apply_room(hb, host_now, mono, out);
        }
    }

    fn set_paused(&mut self, p: bool, mono: i64, out: &mut Out) {
        if self.applied_paused != Some(p) {
            self.applied_paused = Some(p);
            self.last_cmd_us = mono;
            out.player(PlayerCmd::SetPaused(p));
        }
    }

    fn set_speed(&mut self, s: SpeedPermille, mono: i64, out: &mut Out) {
        if self.applied_speed != s {
            self.applied_speed = s;
            self.last_cmd_us = mono;
            out.player(PlayerCmd::SetSpeed(s));
        }
    }

    fn seek(&mut self, pos: MediaMs, mono: i64, out: &mut Out) {
        let pos = match self.duration_ms {
            Some(d) => pos.clamp(0, d.max(0)),
            None => pos.max(0),
        };
        self.awaiting_seek = Some(self.room.epoch);
        self.seek_issued_us = Some(mono);
        self.last_cmd_us = mono;
        self.obs = Some(Obs {
            pos_ms: pos,
            mono_us: mono,
            paused: true,
        });
        out.player(PlayerCmd::Seek(pos));
    }

    fn sync_player(&mut self, host_now: Option<i64>, mono: i64, force_seek: bool, out: &mut Out) {
        self.drift.reset();
        if !self.loaded {
            return;
        }
        let r = self.room;
        let Some(now) = host_now else {
            // No clock estimate yet: hold paused at the anchor position; resync later.
            self.needs_resync = true;
            self.set_paused(true, mono, out);
            if force_seek {
                self.seek(r.position_ms, mono, out);
            }
            return;
        };
        self.needs_resync = false;
        let target = r.position_at(now);
        let local = self.position_estimate(mono);
        // While paused a seek is cheap, so align tightly; while playing only beyond the
        // drift dead-band (smaller errors are corrected by speed nudges).
        let holding = r.paused || !r.is_playing_at(now);
        let tolerance = if holding {
            crate::drift::SETTLED_MS
        } else {
            IGNORE_MS
        };
        let need_seek = force_seek || local.is_none_or(|p| (p - target).abs() > tolerance);
        if r.paused || !r.is_playing_at(now) {
            // Paused, or a play scheduled for the future: hold at the anchor position.
            self.set_paused(true, mono, out);
            self.set_speed(r.speed, mono, out);
            if need_seek {
                self.seek(r.position_ms, mono, out);
            } else {
                self.ready_epoch = self.ready_epoch.max(r.epoch);
            }
            self.pending_start = !r.paused;
        } else {
            if need_seek {
                // Playing: aim where the room will be once this player finishes seeking.
                let lead_ms = self.seek_latency_us * r.speed as i64 / 1_000_000;
                self.seek(target + lead_ms, mono, out);
            } else {
                self.ready_epoch = self.ready_epoch.max(r.epoch);
            }
            self.set_speed(r.speed, mono, out);
            self.set_paused(false, mono, out);
            self.pending_start = false;
        }
    }

    /// Local monotonic time at which `tick` must run next (scheduled start).
    pub fn next_deadline(&self, local_for_host: impl Fn(i64) -> Option<i64>) -> Option<i64> {
        if self.pending_start && self.loaded {
            local_for_host(self.room.anchor_host_us)
        } else {
            None
        }
    }

    pub fn tick(&mut self, host_now: Option<i64>, mono: i64, out: &mut Out) {
        if !self.loaded {
            return;
        }
        if self.needs_resync && host_now.is_some() {
            self.sync_player(host_now, mono, false, out);
        }
        let Some(now) = host_now else { return };
        let r = self.room;
        if self.pending_start {
            if r.paused {
                self.pending_start = false;
            } else if now >= r.anchor_host_us {
                self.pending_start = false;
                // Late by (now - anchor): start from the extrapolated position if that matters.
                let target = r.position_at(now);
                if target - r.position_ms > IGNORE_MS {
                    self.seek(target, mono, out);
                }
                self.set_speed(r.speed, mono, out);
                self.set_paused(false, mono, out);
            }
            return;
        }
        // Drift correction (suspended while buffering, seeking or not playing).
        if r.paused
            || !r.is_playing_at(now)
            || self.buffering
            || self.awaiting_seek.is_some()
            || mono < self.drift_cooldown_until
        {
            return;
        }
        let Some(o) = self.obs else { return };
        if o.paused || o.mono_us <= self.last_cmd_us || mono - o.mono_us > OBS_FRESH_US {
            return;
        }
        let Some(local) = self.position_estimate(mono) else {
            return;
        };
        let expected = r.position_at(now);
        match self.drift.decide(local - expected, expected, r.speed) {
            DriftAction::Keep => {}
            DriftAction::SetSpeed(s) => self.set_speed(s, mono, out),
            DriftAction::HardSeek(t) => {
                self.set_speed(r.speed, mono, out);
                let lead_ms = self.seek_latency_us * r.speed as i64 / 1_000_000;
                self.seek(t + lead_ms, mono, out);
                self.drift_cooldown_until = mono + 1_000_000;
            }
        }
    }
}

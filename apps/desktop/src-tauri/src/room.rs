//! Rooms in the app (Milestone 5, ADR 0006): one driver task per room owns the signaling
//! connection and the [`RoomSession`], forwards its outputs to the WebView2 bridge and the
//! player, and keeps a UI state mirror that is pushed to the window as `room-state` events.

#![cfg(windows)]

use crate::bridge::Bridge;
use crate::commands::AppState;
use crate::connection::ConnectionManager;
use crate::paths::AppPaths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use syncwatch_ice_config::store::{DpapiFileStore, KeyringStore, SecretStore};
use syncwatch_net::auth::IdentityKey;
use syncwatch_net::session::*;
use syncwatch_net::signaling::{SignalingConn, configured_url};
use syncwatch_net::*;
use syncwatch_player::Player;
use syncwatch_protocol::signaling::normalize_room_code;
use syncwatch_protocol::*;
use syncwatch_sync_core::{
    Clock, Event, LocalFileEvent, TorrentEvent, UiAction, UiEvent, WindowState,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

const UI_CHAT_KEEP: usize = 500;
pub const PASSWORD_IGNORED_TEXT: &str =
    "This room has no password, so the one you typed was ignored.";
const TICK: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------- identity ------

/// The per-install ed25519 identity, stored beside the connection credentials (Credential
/// Manager, or a DPAPI file in portable mode). Debug builds with SYNCWATCH_DEV_IDENTITY get
/// their own identity so two dev instances are two different people.
fn load_identity(paths: &AppPaths) -> IdentityKey {
    let suffix = if cfg!(debug_assertions) {
        std::env::var("SYNCWATCH_DEV_IDENTITY")
            .ok()
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(|s| format!("-dev-{s}"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let overflow = DpapiFileStore {
        path: paths.config.join("identity.dpapi"),
    };
    let store: Box<dyn SecretStore> = if paths.portable {
        Box::new(overflow)
    } else {
        Box::new(KeyringStore {
            account: format!("identity-ed25519{suffix}"),
            overflow,
        })
    };
    if let Ok(Some(b)) = store.load()
        && let Ok(secret) = <[u8; 32]>::try_from(b.as_slice())
    {
        return IdentityKey::from_secret(secret);
    }
    let key = IdentityKey::generate().unwrap_or_else(|| {
        // No OS randomness: an in-memory identity for this run only.
        IdentityKey::from_secret(*blake3::hash(&std::process::id().to_le_bytes()).as_bytes())
    });
    if store.save(&key.secret()).is_err() {
        tracing::warn!("could not save the identity key; using it for this session only");
    }
    tracing::info!(id = %syncwatch_net::auth::short_id(&key.public()), "created a new identity");
    key
}

// ---------------------------------------------------------------------------- UI state -----

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermsDto {
    can_play_pause: bool,
    can_seek: bool,
    can_change_media: bool,
    can_change_speed: bool,
}

impl From<Permissions> for PermsDto {
    fn from(p: Permissions) -> Self {
        Self {
            can_play_pause: p.can_play_pause,
            can_seek: p.can_seek,
            can_change_media: p.can_change_media,
            can_change_speed: p.can_change_speed,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermsIn {
    can_play_pause: bool,
    can_seek: bool,
    can_change_media: bool,
    can_change_speed: bool,
}

impl From<PermsIn> for Permissions {
    fn from(p: PermsIn) -> Self {
        Permissions {
            can_play_pause: p.can_play_pause,
            can_seek: p.can_seek,
            can_change_media: p.can_change_media,
            can_change_speed: p.can_change_speed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberDto {
    id: u32,
    name: String,
    is_host: bool,
    is_you: bool,
    connected: bool,
    link: Option<LinkState>,
    media: &'static str,
    buffering: bool,
    tracks_in_sync: bool,
    chat_muted: bool,
    permissions: PermsDto,
    words: Option<[&'static str; 4]>,
    /// Torrent rooms: this member's download phase and whole-percent progress.
    torrent: Option<TorrentMemberDto>,
    /// Torrent rooms: what a buffering member waits for (position and window progress).
    wait: Option<WaitDto>,
}

/// A buffering participant's missing window at the room position (ADR 0012).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitDto {
    at_ms: i64,
    missing: u64,
    permille: u16,
}

impl From<BufferWait> for WaitDto {
    fn from(w: BufferWait) -> Self {
        Self {
            at_ms: w.at_ms,
            missing: w.missing,
            permille: w.permille,
        }
    }
}

/// THIS window's buffering popup (its own player stalled on missing data).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StallDto {
    stalled: bool,
    wait: Option<WaitDto>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMemberDto {
    phase: &'static str,
    permille: u16,
}

fn phase_text(p: TorrentPhase) -> &'static str {
    match p {
        TorrentPhase::Metadata => "metadata",
        TorrentPhase::WaitingForHost => "waitingForHost",
        TorrentPhase::Downloading => "downloading",
        TorrentPhase::Streaming => "streaming",
        TorrentPhase::NoPeers => "noPeers",
        TorrentPhase::Complete => "complete",
        TorrentPhase::Failed => "failed",
    }
}

/// This window's own torrent download (progress block, seek bar, buffering overlay).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentDto {
    phase: &'static str,
    have_bytes: u64,
    size: u64,
    down_bps: u64,
    up_bps: u64,
    peers: u32,
    eta_secs: Option<u64>,
    /// Downloaded byte ranges [start, end).
    ranges: Vec<[u64; 2]>,
    /// Readiness at the room position (window + container pieces); display only. The popup
    /// is `RoomStateDto::stall`, never this.
    window: Option<WindowDto>,
    /// Only the playback stream runs (window short / just seeked).
    exclusive: bool,
    /// What is used to find peers (status text while looking).
    trackers: u32,
    dht: bool,
    /// BEP 19 web seeds in the torrent (librqbit 9.0.1 cannot use them).
    web_seeds: u32,
    /// Set once complete: "Downloading and sharing have stopped. No further traffic."
    complete_message: Option<&'static str>,
    error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowDto {
    ready: bool,
    at_ms: u64,
    missing: u64,
    permille: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmMagnetDto {
    name: String,
    size: u64,
    infohash: String,
    torrent_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatDto {
    id: u64,
    sender: u32,
    name: String,
    is_host: bool,
    text: String,
    wall_ms: i64,
    media_time_ms: Option<i64>,
    media_epoch: u64,
    /// A "Now playing" divider instead of a message.
    divider: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingFileDto {
    name: String,
    size: u64,
    duration_ms: Option<u64>,
    /// Increments each time the panel must be shown (once per media change).
    seq: u32,
    /// The panel is shown until the member picks a file (then it closes for good).
    panel_open: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagEvent {
    /// Seconds since the room session started.
    t: f64,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectRow {
    /// None = a joining connection that was not a member yet, or you on the member side.
    member: Option<u32>,
    who: String,
    what: String,
    count: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagDto {
    signaling_host: String,
    signaling_connected: bool,
    signaling_reconnects: u32,
    events: Vec<DiagEvent>,
    rejected: Vec<RejectRow>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeedPasswordDto {
    wrong: bool,
    retry_after_ms: u32,
}

/// This window's chat state, exactly as the host last told it (the UI derives everything about
/// sending from this; the host is never muted).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatStateDto {
    enabled: bool,
    slow_mode_secs: u16,
    muted: bool,
}

impl Default for ChatStateDto {
    fn default() -> Self {
        Self {
            enabled: true,
            slow_mode_secs: 0,
            muted: false,
        }
    }
}

impl From<ChatState> for ChatStateDto {
    fn from(s: ChatState) -> Self {
        Self {
            enabled: s.config.enabled,
            slow_mode_secs: s.config.slow_mode_secs,
            muted: s.muted,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomStateDto {
    active: bool,
    /// Increments for every room this app process enters (the UI remounts the room view).
    session_id: u32,
    room_code: Option<String>,
    phase: Option<Phase>,
    is_host: bool,
    me: Option<u32>,
    has_password: bool,
    locked: bool,
    members: Vec<MemberDto>,
    my_permissions: PermsDto,
    chat: Vec<ChatDto>,
    chat_state: ChatStateDto,
    /// Member side: the words to compare with the host (rooms without a password).
    host_words: Option<[&'static str; 4]>,
    host_link: Option<LinkState>,
    need_password: Option<NeedPasswordDto>,
    missing_file: Option<MissingFileDto>,
    file_status: Option<&'static str>,
    media_title: Option<String>,
    toasts: Vec<String>,
    ended: Option<EndReason>,
    /// The member typed a password but the room has none (it was ignored).
    password_ignored: bool,
    diag: DiagDto,
    /// The room's current media epoch (chat chips of older epochs are not seekable).
    media_epoch: u64,
    media_duration_ms: Option<i64>,
    gate: GateDto,
    externals: Vec<ExternalDto>,
    /// Host: members who declined an external file.
    declined: Vec<String>,
    /// Late joiner / reconnect: loading and catching up.
    joining: bool,
    /// Torrent rooms: our own download.
    torrent: Option<TorrentDto>,
    /// Member: the host's magnet waits for our confirmation (name and size shown).
    confirm_magnet: Option<ConfirmMagnetDto>,
    /// The buffering popup of this window's own player (ADR 0012).
    stall: StallDto,
    /// The player could not open the room's video (damaged or unsupported file).
    load_error: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateDto {
    active: bool,
    waiting: Vec<GateWaitDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateWaitDto {
    member: u32,
    name: String,
    /// "buffering", "loading", "downloading", "missing the file", "verifying the file".
    reason: &'static str,
    /// Buffering: the room position and the missing part of the window.
    wait: Option<WaitDto>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDto {
    id: u32,
    kind: &'static str,
    name: String,
    size: u64,
    /// Member: bytes received so far (None = not downloading).
    received: Option<u64>,
    failed: bool,
    /// Member: the host chose not to send this file to us (relayed connection).
    withheld: bool,
}

fn media_text(m: MediaStatus) -> &'static str {
    match m {
        MediaStatus::NoMedia => "none",
        MediaStatus::Missing => "missing",
        MediaStatus::Verifying => "verifying",
        MediaStatus::Ready => "ready",
        MediaStatus::Mismatch => "mismatch",
        MediaStatus::Downloading { .. } => "downloading",
        MediaStatus::Declined => "declined",
        MediaStatus::Unavailable => "unavailable",
    }
}

fn chat_reject_text(r: ChatRejectReason) -> &'static str {
    match r {
        ChatRejectReason::ChatDisabled => "Chat is turned off by the host.",
        ChatRejectReason::Muted => "You are muted by the host.",
        ChatRejectReason::Empty => "Empty messages are not sent.",
        ChatRejectReason::TooLong => "That message is too long (500 characters at most).",
        ChatRejectReason::RateLimited => "Slow down: at most 5 messages in 5 seconds.",
        ChatRejectReason::SlowMode => "Slow mode is on: wait a moment before the next message.",
        ChatRejectReason::Invalid => "That message could not be sent.",
    }
}

#[derive(Default)]
struct Mirror {
    s: RoomStateDto,
    /// Soak-test counters (debug): times the Reconnecting banner appeared, and times any link
    /// showed Reconnecting.
    banners: u32,
    link_reconnects: u32,
    infos: Vec<MemberInfo>,
    links: BTreeMap<Option<u32>, LinkState>,
    words: BTreeMap<u32, [&'static str; 4]>,
    panel_seq: u32,
    t0: Option<Instant>,
}

const DIAG_EVENTS_KEEP: usize = 200;

impl Mirror {
    fn rebuild_members(&mut self) {
        let me = self.s.me;
        let host_link = self.links.get(&None).copied();
        self.s.host_link = host_link;
        self.s.members = self
            .infos
            .iter()
            .map(|i| {
                let is_you = Some(i.id.0) == me;
                let link = if is_you {
                    None
                } else if i.is_host && !self.s.is_host {
                    host_link
                } else if !i.connected {
                    Some(LinkState::Reconnecting)
                } else {
                    self.links.get(&Some(i.id.0)).copied()
                };
                MemberDto {
                    id: i.id.0,
                    name: i.display_name.clone(),
                    is_host: i.is_host,
                    is_you,
                    connected: i.connected,
                    link,
                    media: media_text(i.media),
                    buffering: i.buffering,
                    tracks_in_sync: matches!(i.tracks, TrackSyncStatus::InSync),
                    chat_muted: i.chat_muted,
                    torrent: i.torrent.map(|t| TorrentMemberDto {
                        phase: phase_text(t.phase),
                        permille: t.permille,
                    }),
                    wait: i.wait.filter(|_| i.buffering).map(WaitDto::from),
                    permissions: i.permissions.into(),
                    words: self.words.get(&i.id.0).copied(),
                }
            })
            .collect();
    }

    fn event(&mut self, text: impl Into<String>) {
        let t0 = *self.t0.get_or_insert_with(Instant::now);
        self.s.diag.events.push(DiagEvent {
            t: (t0.elapsed().as_millis() as f64) / 1000.0,
            text: text.into(),
        });
        let n = self.s.diag.events.len();
        if n > DIAG_EVENTS_KEEP {
            self.s.diag.events.drain(..n - DIAG_EVENTS_KEEP);
        }
    }

    fn name_of(&self, m: u32) -> String {
        self.infos
            .iter()
            .find(|i| i.id.0 == m)
            .map(|i| i.display_name.clone())
            .unwrap_or_else(|| format!("member {m}"))
    }

    fn reject(&mut self, member: Option<u32>, who: String, what: String) {
        self.event(format!("refused ({who}): {what}"));
        match self
            .s
            .diag
            .rejected
            .iter_mut()
            .find(|r| r.member == member && r.what == what)
        {
            Some(r) => r.count += 1,
            None => self.s.diag.rejected.push(RejectRow {
                member,
                who,
                what,
                count: 1,
            }),
        }
    }

    /// The manifest as the UI shows it (names, sizes, my progress).
    fn set_manifest(&mut self, manifest: &[ExternalEntry]) {
        let old = std::mem::take(&mut self.s.externals);
        self.s.externals = manifest
            .iter()
            .map(|e| {
                let prev = old.iter().find(|o| o.id == e.external_id.0);
                ExternalDto {
                    id: e.external_id.0,
                    kind: match e.kind {
                        ExternalKind::Audio => "audio",
                        ExternalKind::Subtitle => "sub",
                    },
                    name: e.display_name.clone(),
                    size: e.size,
                    received: prev.and_then(|p| p.received),
                    failed: prev.is_some_and(|p| p.failed),
                    withheld: prev.is_some_and(|p| p.withheld),
                }
            })
            .collect();
    }

    fn declined(&mut self, member: u32, id: ExternalId) {
        let who = self.name_of(member);
        let file = self
            .s
            .externals
            .iter()
            .find(|e| e.id == id.0)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| format!("file {}", id.0));
        let line = format!("{who} declined {file}");
        self.event(line.clone());
        if !self.s.declined.contains(&line) {
            self.s.declined.push(line);
        }
    }

    /// The member picked a file: the panel closes for good (until the next media change).
    fn file_chosen(&mut self) {
        if let Some(mf) = &mut self.s.missing_file {
            mf.panel_open = false;
        }
        self.s.file_status = Some("verifying");
        self.event("local file chosen; verifying");
    }

    fn members_changed(&mut self, new: &[MemberInfo]) {
        let me = self.s.me;
        for n in new {
            if Some(n.id.0) == me {
                continue;
            }
            match self.infos.iter().find(|o| o.id == n.id) {
                None => self.event(format!("{} joined", n.display_name)),
                Some(o) if o.connected && !n.connected => {
                    self.event(format!("{} dropped (seat kept 20 s)", n.display_name))
                }
                Some(o) if !o.connected && n.connected => {
                    self.event(format!("{} is back", n.display_name))
                }
                Some(o) if o.chat_muted != n.chat_muted => self.event(format!(
                    "{} {} in chat",
                    n.display_name,
                    if n.chat_muted { "muted" } else { "unmuted" }
                )),
                _ => {}
            }
        }
        for o in &self.infos.clone() {
            if Some(o.id.0) != me && !new.iter().any(|n| n.id == o.id) {
                self.event(format!("{} left", o.display_name));
            }
        }
    }

    fn toast(&mut self, t: impl Into<String>) {
        self.s.toasts.push(t.into());
        if self.s.toasts.len() > 5 {
            self.s.toasts.remove(0);
        }
    }

    fn apply(&mut self, u: RoomUi) {
        match u {
            RoomUi::Created { room } => {
                self.event("room created");
                self.s.room_code = Some(room);
            }
            RoomUi::Joined { room } => {
                self.event("signaling: joined the room");
                self.s.room_code = Some(room);
            }
            RoomUi::Phase(p) => {
                self.event(format!("phase: {p:?}"));
                if p == Phase::Reconnecting && self.s.phase != Some(Phase::Reconnecting) {
                    self.banners += 1;
                }
                self.s.phase = Some(p);
                if p == Phase::InRoom {
                    self.s.need_password = None;
                }
            }
            RoomUi::NeedPassword {
                wrong,
                retry_after_ms,
            } => {
                self.event(if wrong {
                    format!(
                        "wrong password (retry in {} s)",
                        retry_after_ms.div_ceil(1000)
                    )
                } else {
                    "the room needs a password".to_owned()
                });
                self.s.need_password = Some(NeedPasswordDto {
                    wrong,
                    retry_after_ms,
                })
            }
            RoomUi::Verification { member, words } => match member {
                None => self.s.host_words = Some(words),
                Some(m) => {
                    self.words.insert(m.0, words);
                    self.rebuild_members();
                }
            },
            RoomUi::Link { member, state } => {
                let key = member.map(|m| m.0);
                if state == LinkState::Reconnecting
                    && self.links.get(&key) != Some(&LinkState::Reconnecting)
                {
                    self.link_reconnects += 1;
                }
                let who = match key {
                    None => "host".to_owned(),
                    Some(m) => self.name_of(m),
                };
                if self.links.get(&key) != Some(&state) {
                    self.event(format!("link to {who}: {state:?}"));
                }
                self.links.insert(key, state);
                self.rebuild_members();
            }
            RoomUi::Error(e) => {
                self.event(format!("error: {e}"));
                self.toast(e);
            }
            RoomUi::Info(text) => {
                self.event(text.clone());
                self.toast(text);
            }
            RoomUi::PasswordIgnored => {
                self.s.password_ignored = true;
                self.event("the room has no password; the typed one was ignored");
                self.toast(PASSWORD_IGNORED_TEXT);
            }
            RoomUi::Rejected { member, what } => {
                let who = match member {
                    Some(m) => self.name_of(m.0),
                    None => "joining connection".to_owned(),
                };
                self.reject(member.map(|m| m.0), who, what);
            }
            RoomUi::Core(ev) => self.apply_core(ev),
        }
    }

    fn apply_core(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Joining => {
                self.event("joining: loading and catching up");
                self.s.joining = true;
            }
            UiEvent::Stall { stalled, wait } => {
                if stalled != self.s.stall.stalled {
                    self.event(if stalled {
                        "buffering: this player waits for data"
                    } else {
                        "buffering over"
                    });
                }
                self.s.stall = StallDto {
                    stalled,
                    wait: wait.map(WaitDto::from),
                };
            }
            UiEvent::Gate(g) => {
                if g.active != self.s.gate.active {
                    self.event(if g.active {
                        "waiting for members (ready gate)"
                    } else {
                        "ready gate released"
                    });
                }
                self.s.gate = GateDto {
                    active: g.active,
                    waiting: g
                        .waiting
                        .iter()
                        .map(|(m, r)| GateWaitDto {
                            member: m.0,
                            name: self.name_of(m.0),
                            reason: match r {
                                NotReadyReason::MissingFile => "missing the file",
                                NotReadyReason::Verifying => "verifying the file",
                                NotReadyReason::Downloading => "downloading",
                                NotReadyReason::Buffering(_) => "buffering",
                                NotReadyReason::Loading => "loading",
                            },
                            wait: match r {
                                NotReadyReason::Buffering(w) => w.map(WaitDto::from),
                                _ => None,
                            },
                        })
                        .collect(),
                };
            }
            UiEvent::ExternalProgress {
                external_id,
                received,
                ..
            } => {
                if let Some(e) = self.s.externals.iter_mut().find(|e| e.id == external_id.0) {
                    e.received = Some(received);
                }
            }
            UiEvent::ExternalWithheld(id) => {
                self.event(format!("the host did not send external file {}", id.0));
                if let Some(e) = self.s.externals.iter_mut().find(|e| e.id == id.0) {
                    e.withheld = true;
                    e.received = None;
                }
            }
            UiEvent::ExternalFailed(id) => {
                self.event(format!("external file {} failed verification", id.0));
                if let Some(e) = self.s.externals.iter_mut().find(|e| e.id == id.0) {
                    e.failed = true;
                }
            }
            UiEvent::TrackUnresolved { kind } => {
                self.event(format!("the host's {kind:?} track is not in your file"));
                self.toast("The host's selected track does not exist in your copy of the video.");
            }
            UiEvent::Joined { me } => {
                self.s.me = Some(me.0);
                self.rebuild_members();
            }
            UiEvent::Members(v) => {
                self.members_changed(&v);
                self.infos = v;
                self.rebuild_members();
            }
            UiEvent::Permissions(p) => {
                self.event("my permissions changed");
                self.s.my_permissions = p.into();
            }
            UiEvent::ChatMessage(c) => {
                let is_host = self.infos.iter().any(|i| i.id == c.sender && i.is_host);
                let dto = ChatDto {
                    id: c.id.0,
                    sender: c.sender.0,
                    name: c.display_name,
                    is_host,
                    text: c.text,
                    wall_ms: c.host_wall_ms,
                    media_time_ms: c.media_time_ms,
                    media_epoch: c.media_epoch.0,
                    divider: None,
                };
                match self
                    .s
                    .chat
                    .iter_mut()
                    .find(|m| m.divider.is_none() && m.id == dto.id)
                {
                    Some(existing) => *existing = dto,
                    None => self.s.chat.push(dto),
                }
                let n = self.s.chat.len();
                if n > UI_CHAT_KEEP {
                    self.s.chat.drain(..n - UI_CHAT_KEEP);
                }
            }
            UiEvent::ChatRejected { reason, .. } => {
                self.reject(
                    None,
                    "you".into(),
                    format!("chat message refused: {reason:?}"),
                );
                // Muted / chat off are normal states (the ChatState update already disabled
                // the input): no error toast for them.
                if !matches!(
                    reason,
                    ChatRejectReason::Muted | ChatRejectReason::ChatDisabled
                ) {
                    self.toast(chat_reject_text(reason));
                }
            }
            // The ONLY source of this window's chat state (input, placeholder, status line).
            UiEvent::ChatState(s) => {
                let new: ChatStateDto = s.into();
                if new.muted != self.s.chat_state.muted {
                    self.event(if new.muted {
                        "you were muted by the host"
                    } else {
                        "you were unmuted"
                    });
                }
                if new.enabled != self.s.chat_state.enabled {
                    self.event(if new.enabled {
                        "chat turned on"
                    } else {
                        "chat turned off"
                    });
                }
                self.s.chat_state = new;
            }
            UiEvent::ChatDeleted(id) => self.s.chat.retain(|m| m.divider.is_some() || m.id != id.0),
            UiEvent::ChatCleared => self.s.chat.clear(),
            UiEvent::ShowMissingFilePanel {
                name,
                size,
                duration_ms,
            } => {
                self.panel_seq += 1;
                self.s.file_status = None;
                self.event("choose-my-file panel shown");
                self.s.missing_file = Some(MissingFileDto {
                    name,
                    size,
                    duration_ms,
                    seq: self.panel_seq,
                    panel_open: true,
                });
            }
            UiEvent::FileVerified => {
                self.event("local file verified");
                if let Some(mf) = &mut self.s.missing_file {
                    mf.panel_open = false;
                }
                self.s.file_status = Some("verified");
            }
            UiEvent::FileMismatch { .. } => {
                self.event("local file differs from the host's");
                if let Some(mf) = &mut self.s.missing_file {
                    mf.panel_open = false;
                }
                self.s.file_status = Some("mismatch");
            }
            UiEvent::MediaChanged { title } => {
                if let Some(t) = &title {
                    self.s.chat.push(ChatDto {
                        id: 0,
                        sender: 0,
                        name: String::new(),
                        is_host: false,
                        text: String::new(),
                        wall_ms: 0,
                        media_time_ms: None,
                        media_epoch: 0,
                        divider: Some(format!("Now playing: {t}")),
                    });
                }
                self.s.media_title = title;
                self.s.file_status = None;
                self.s.torrent = None;
                self.s.confirm_magnet = None;
                // "Choose my file" belongs to the previous local file: a torrent (or no media)
                // has none; a new local file sets it again right after (ShowMissingFilePanel).
                self.s.missing_file = None;
                self.s.load_error = false;
            }
            UiEvent::ConfirmMagnet {
                name,
                size,
                infohash,
                torrent_name,
            } => {
                self.event(format!("the host shared a torrent: {name}"));
                self.s.confirm_magnet = Some(ConfirmMagnetDto {
                    name,
                    size,
                    infohash: infohash.to_hex(),
                    torrent_name,
                });
            }
            UiEvent::Notice(Notice::RequestDenied { reason }) => {
                self.reject(None, "you".into(), format!("request refused: {reason:?}"));
                self.toast("The host did not allow that.");
            }
            UiEvent::Notice(n) => self.toast(match n {
                Notice::MediaChangeSuperseded { .. } => {
                    "Someone else changed the source at the same time; their change won."
                }
                Notice::RequestDenied { .. } => "The host did not allow that.",
                Notice::MemberDeclinedMagnet { .. } => "A member declined the torrent.",
            }),
            UiEvent::BecameHost => {
                self.event("became the host");
                self.toast("You are the host now.")
            }
            UiEvent::HostChanged { .. } => {
                self.event("the host role moved");
                self.toast("The host role moved to another member.")
            }
            UiEvent::Kicked { banned } => self.event(if banned {
                "banned by the host"
            } else {
                "removed by the host"
            }),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------- manager -------

enum Cmd {
    Action(Action),
    Core(Event),
    /// Host: select a track by its local player id (None = off).
    SelectTrack {
        kind: syncwatch_player::TrackKind,
        track_id: Option<i64>,
    },
    /// Host: a validated, hashed external file to add to the room.
    AddExternal(crate::roommedia::PreparedExternal, ExternalKind),
    /// Debug: drop the link to the host (the session reconnects and re-authenticates).
    DevReconnect,
}

pub struct RoomManager {
    key: IdentityKey,
    cmd: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
    /// Bridge events for the active room: (control and everything else, bulk file chunks).
    transport: Mutex<
        Option<(
            mpsc::UnboundedSender<TransportEvent>,
            mpsc::UnboundedSender<TransportEvent>,
        )>,
    >,
    state: Mutex<RoomStateDto>,
    /// The local file this member chose (never sent anywhere).
    chosen: Mutex<Option<PathBuf>>,
    sessions: std::sync::atomic::AtomicU32,
    /// Player events for the active room.
    player_tx: Mutex<Option<mpsc::UnboundedSender<syncwatch_player::PlayerEvent>>>,
    /// Root of the per-room caches.
    rooms_cache: PathBuf,
    /// (The per-room caches live in the app cache folder under rooms, one folder per room.)
    /// The next added external file must not go to relayed members (host choice).
    skip_next_relayed: std::sync::atomic::AtomicBool,
}

/// Clears EVERYTHING a room leaves behind in this app process: the player is stopped and
/// unloaded (no file, no tracks), the chosen local file is forgotten and the UI state is reset
/// (members, chat, permissions, words, choose-my-file, diagnostics). Called when a room starts
/// and when it ends; the session itself (sync-core state: media descriptor and epochs,
/// playback, tracks, manifest, chat history, ready gate, transfers) is dropped with its task.
/// Only nded (why the last room ended) is kept, for the dialog.
fn reset_room_scope(player: &dyn Player, m: &RoomManager, ended: Option<EndReason>) {
    if let Err(e) = player.unload() {
        tracing::warn!(error = %e, "could not unload the player");
    }
    if let Ok(mut g) = m.chosen.lock() {
        *g = None;
    }
    if let Ok(mut g) = m.state.lock() {
        *g = RoomStateDto {
            ended,
            ..RoomStateDto::default()
        };
    }
}

impl RoomManager {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            key: load_identity(paths),
            cmd: Mutex::new(None),
            transport: Mutex::new(None),
            state: Mutex::new(RoomStateDto::default()),
            chosen: Mutex::new(None),
            sessions: std::sync::atomic::AtomicU32::new(0),
            player_tx: Mutex::new(None),
            rooms_cache: {
                // Leftovers from a crash: nothing from an old room may survive.
                let dir = paths.cache.join("rooms");
                let _ = std::fs::remove_dir_all(&dir);
                dir
            },
            skip_next_relayed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Player events reach the active room through this (called by the player event thread).
    pub fn player_event(&self, ev: &syncwatch_player::PlayerEvent) {
        if let Some(tx) = self.player_tx.lock().ok().and_then(|g| g.clone()) {
            let _ = tx.send(ev.clone());
        }
    }

    /// Forwards bridge events to the active room (call once from setup).
    pub fn forward_transport(app: AppHandle, mut events: mpsc::UnboundedReceiver<TransportEvent>) {
        tauri::async_runtime::spawn(async move {
            while let Some(ev) = events.recv().await {
                let m = app.state::<RoomManager>();
                let tx = m.transport.lock().ok().and_then(|g| g.clone());
                if let Some((control, bulk)) = tx {
                    // File chunks get their own queue so the driver serves control, chat and
                    // track commands first (they must never wait behind a transfer).
                    let _ = match ev {
                        TransportEvent::Frame {
                            lane: Lane::Bulk, ..
                        } => bulk.send(ev),
                        other => control.send(other),
                    };
                }
            }
        });
    }

    fn send(&self, c: Cmd) -> Result<(), String> {
        let tx = self.cmd.lock().ok().and_then(|g| g.clone());
        match tx {
            Some(tx) => tx.send(c).map_err(|_| "You are not in a room.".to_owned()),
            None => Err("You are not in a room.".to_owned()),
        }
    }
}

fn clock(epoch: Instant) -> Clock {
    Clock {
        mono_us: epoch.elapsed().as_micros() as i64,
        wall_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    }
}

/// ICE servers for a room: fresh credentials on every create/join (spec section 5).
/// Debug builds: SYNCWATCH_DEV_ICE=none|coturn skips Metered (no quota) for local tests.
pub async fn session_ice(app: &AppHandle) -> Result<IceConfig, String> {
    if cfg!(debug_assertions) {
        match std::env::var("SYNCWATCH_DEV_ICE").as_deref() {
            Ok("none") => return Ok(IceConfig::default()),
            Ok("coturn") => return Ok(crate::netlab::coturn_ice()),
            _ => {}
        }
    }
    let m = app.state::<ConnectionManager>();
    // Debug: SYNCWATCH_DEV_FORCE_RELAY=1 forces relay-only (short live relay tests).
    let relay_only = m.status().settings.relay_only
        || (cfg!(debug_assertions)
            && std::env::var("SYNCWATCH_DEV_FORCE_RELAY").as_deref() == Ok("1"));
    let Some((servers, warning)) = m.servers_for_session().await else {
        return Err("Your connection is not set up. Open Settings > Connection.".into());
    };
    if warning.is_some() {
        tracing::warn!("using the last good relay servers (Metered unreachable)");
    }
    for s in &servers {
        for v in [&s.username, &s.credential].into_iter().flatten() {
            crate::logging::register_secret(v);
        }
    }
    Ok(IceConfig {
        ice_servers: servers
            .iter()
            .map(|s| IceServerJs {
                urls: s.urls.iter().map(|u| u.to_string()).collect(),
                username: s.username.clone(),
                credential: s.credential.clone(),
            })
            .collect(),
        relay_only,
    })
}

enum Mode {
    Create {
        password: Option<String>,
    },
    Join {
        room: String,
        password: Option<String>,
    },
}

/// What happened right after Create/Join, for the dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryOutcome {
    /// "inRoom", "needPassword", "pending" (still connecting) or "ended".
    status: &'static str,
    password_ignored: bool,
    ended: Option<EndReason>,
}

/// Waits (up to max) until the room session reached a state the dialog can report.
async fn wait_outcome(app: &AppHandle, max: Duration) -> EntryOutcome {
    let end = Instant::now() + max;
    loop {
        let s = app
            .state::<RoomManager>()
            .state
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let status = if s.ended.is_some() {
            Some("ended")
        } else if s.need_password.is_some() {
            Some("needPassword")
        } else if s.phase == Some(Phase::InRoom) {
            Some("inRoom")
        } else {
            None
        };
        if let Some(status) = status {
            return EntryOutcome {
                status,
                password_ignored: s.password_ignored,
                ended: s.ended,
            };
        }
        if Instant::now() >= end {
            return EntryOutcome {
                status: "pending",
                password_ignored: false,
                ended: None,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn start(app: AppHandle, name: String, mode: Mode) -> Result<EntryOutcome, String> {
    let m = app.state::<RoomManager>();
    if m.cmd.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Err("You are already in a room. Leave it first.".into());
    }
    if !app.state::<ConnectionManager>().status().can_connect {
        return Err("Set up and test your connection first (Settings > Connection).".into());
    }
    let name: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if name.is_empty() {
        return Err("Enter a display name.".into());
    }
    let ice = session_ice(&app).await?;
    let url = configured_url();
    tracing::info!(signaling_url = %url, "connecting to the signaling server");
    let sig = SignalingConn::connect(&url)
        .await
        .map_err(|e| e.to_string())?;
    let prefs = app.state::<crate::prefs::PrefsService>().get();
    let cfg = SessionConfig {
        key: m.key.clone(),
        display_name: name,
        ice,
        max_members: limits::MAX_MEMBERS as u8,
        gate_timeout_us: Some(prefs.gate_timeout_us()),
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (tr_tx, tr_rx) = mpsc::unbounded_channel();
    let (bk_tx, bk_rx) = mpsc::unbounded_channel();
    let (pl_tx, pl_rx) = mpsc::unbounded_channel();
    if let Ok(mut g) = m.cmd.lock() {
        *g = Some(cmd_tx);
    }
    if let Ok(mut g) = m.player_tx.lock() {
        *g = Some(pl_tx);
    }
    if let Ok(mut g) = m.transport.lock() {
        *g = Some((tr_tx, bk_tx));
    }
    // A new room starts completely clean: nothing loaded, nothing remembered.
    reset_room_scope(&*app.state::<AppState>().player, &m, None);
    let session_id = m.sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let chans = Chans {
            cmd: cmd_rx,
            tr: tr_rx,
            bulk: bk_rx,
            pl: pl_rx,
            tor: app2
                .state::<Arc<crate::torrent::TorrentService>>()
                .attach_room(),
        };
        let ended = drive(
            app2.clone(),
            cfg,
            mode,
            sig,
            DriveIds { url, session_id },
            chans,
        )
        .await;
        let m = app2.state::<RoomManager>();
        // Nothing from this room may survive into the next one.
        reset_room_scope(&*app2.state::<AppState>().player, &m, ended);
        let last = m.state.lock().map(|g| g.clone()).unwrap_or_default();
        let _ = app2.emit("room-state", &last);
        if let Ok(mut g) = m.cmd.lock() {
            *g = None;
        }
        if let Ok(mut g) = m.transport.lock() {
            *g = None;
        }
        if let Ok(mut g) = m.player_tx.lock() {
            *g = None;
        }
    });
    Ok(wait_outcome(&app, Duration::from_secs(20)).await)
}

/// Host name only (no path or query) for the diagnostics view.
fn signaling_host(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or_default()
        .to_owned()
}

async fn recv_sig(
    sig: &mut Option<SignalingConn>,
) -> Result<syncwatch_protocol::signaling::ServerMsg, ()> {
    match sig {
        Some(s) => s.recv().await.map_err(|_| ()),
        None => std::future::pending().await,
    }
}

/// Signaling URL and the room's session number, for the driver.
struct DriveIds {
    url: String,
    session_id: u32,
}

/// The driver's receivers.
struct Chans {
    cmd: mpsc::UnboundedReceiver<Cmd>,
    tr: mpsc::UnboundedReceiver<TransportEvent>,
    /// External-file chunks (served only when nothing else is waiting).
    bulk: mpsc::UnboundedReceiver<TransportEvent>,
    pl: mpsc::UnboundedReceiver<syncwatch_player::PlayerEvent>,
    /// Torrent engine events (the engine is process-wide; one room listens at a time).
    tor: mpsc::UnboundedReceiver<syncwatch_torrent::EngineEvent>,
}

/// Everything the driver loop owns besides the session.
struct Rt {
    app: AppHandle,
    bridge: Arc<Bridge>,
    player: Arc<syncwatch_player::mpv::MpvPlayer>,
    sig: Option<SignalingConn>,
    mirror: Mirror,
    media: crate::roommedia::MediaCtx,
    /// Host: files (by blake3) the host chose not to send to relayed members (quota).
    skip_relayed: std::collections::HashSet<[u8; 32]>,
    /// Member: manifest entries already checked against the per-room cache.
    checked_cache: std::collections::HashSet<ExternalId>,
    /// To notice becoming the host (succession): our downloaded externals become host files.
    was_host: bool,
    torrents: Arc<crate::torrent::TorrentService>,
    /// The room's torrent this window downloads / plays (at most one).
    torrent: Option<syncwatch_protocol::Infohash>,
    /// The duration last given to the torrent engine's time -> byte map.
    torrent_duration_sent: Option<u64>,
    /// Readiness at the room position from the last engine status, for sync-core.
    pending_window: Option<Option<WindowState>>,
    /// Member: the host's verified metadata for the room torrent (bytes, host engine port).
    member_meta: Option<(Infohash, Vec<u8>, u16)>,
    /// Member: accepted, waiting for the host's metadata offer before starting.
    torrent_pending: Option<TorrentMedia>,
}

/// Position reports to sync-core (the drift controller and the chat timestamps).
const POSITION_EVERY: Duration = Duration::from_millis(100);
/// Upper bound of chunks queued per loop turn (the bridge credit is the real limit).
const UPLOAD_BURST: usize = 256;

async fn drive(
    app: AppHandle,
    cfg: SessionConfig,
    mode: Mode,
    sig: SignalingConn,
    ids: DriveIds,
    mut ch: Chans,
) -> Option<EndReason> {
    let DriveIds { url, session_id } = ids;
    let epoch = Instant::now();
    let rooms_cache = app.state::<RoomManager>().rooms_cache.clone();
    let mut rt = Rt {
        bridge: Arc::clone(&*app.state::<Arc<Bridge>>()),
        player: Arc::clone(&app.state::<AppState>().player),
        app: app.clone(),
        sig: Some(sig),
        mirror: Mirror::default(),
        media: crate::roommedia::MediaCtx::new(rooms_cache.join(session_id.to_string())),
        skip_relayed: Default::default(),
        checked_cache: Default::default(),
        was_host: false,
        torrents: Arc::clone(&*app.state::<Arc<crate::torrent::TorrentService>>()),
        torrent: None,
        torrent_duration_sent: None,
        pending_window: None,
        member_meta: None,
        torrent_pending: None,
    };
    let mut reconnect_at: Option<Instant> = None;
    let mut backoff = Duration::from_secs(1);
    rt.mirror.s.active = true;
    rt.mirror.s.session_id = session_id;
    rt.mirror.t0 = Some(epoch);
    rt.mirror.s.diag.signaling_host = signaling_host(&url);
    rt.mirror.s.diag.signaling_connected = true;
    rt.mirror.event("signaling connected");
    let (mut session, outs) = match mode {
        Mode::Create { password } => {
            rt.mirror.s.has_password = password.is_some();
            let (mut s, mut outs) = RoomSession::create(cfg, password, clock(epoch));
            // Room defaults (Settings): who controls playback in a new room.
            let preset = rt.app.state::<crate::prefs::PrefsService>().get().preset;
            if preset == "onlyMe" {
                outs.extend(s.handle(
                    clock(epoch),
                    Input::Action(Action::Ui(UiAction::ApplyPreset {
                        preset: Preset::OnlyMe,
                        custom: Permissions::ALL,
                    })),
                ));
            }
            (s, outs)
        }
        Mode::Join { room, password } => RoomSession::join(cfg, room, password, clock(epoch)),
    };
    let mut ended = exec(&mut rt, &session, outs).await;
    publish(&app, &mut rt, &session);
    let mut last_soak = Instant::now();
    let mut last_pos = Instant::now();
    let mut last_publish = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !ended {
        let mut bulk_step = false;
        let input = tokio::select! {
            // Control before bulk: a track command must not wait behind file chunks.
            biased;
            m = recv_sig(&mut rt.sig) => match m {
                Ok(m) => Some(Input::Signaling(m)),
                Err(()) => {
                    tracing::warn!("signaling connection lost; reconnecting");
                    rt.mirror.s.diag.signaling_connected = false;
                    rt.mirror.event("signaling connection lost; reconnecting");
                    rt.sig = None;
                    reconnect_at = Some(Instant::now() + backoff);
                    None
                }
            },
            ev = ch.tr.recv() => ev.map(Input::Transport),
            pe = ch.pl.recv() => pe.and_then(|ev| {
                // The torrent stream is open: from now on only the ranges the player read to
                // open it count for readiness (not the start-up range).
                match &ev {
                    syncwatch_player::PlayerEvent::FileLoaded { .. } => rt.mirror.s.load_error = false,
                    syncwatch_player::PlayerEvent::EndFile {
                        reason: syncwatch_player::EndReason::Error,
                    } => {
                        rt.mirror.s.load_error = true;
                        rt.mirror.event("the player could not open the video");
                    }
                    _ => {}
                }
                if let (syncwatch_player::PlayerEvent::FileLoaded { .. }, Some(ih), Some(_)) =
                    (&ev, rt.torrent, rt.media.torrent_url.as_ref())
                {
                    rt.torrents.engine.opened(ih);
                }
                crate::roommedia::player_obs(&rt.media, &*rt.player, &ev)
                    .map(|o| Input::Core(Event::Player(o)))
            }),
            te = ch.tor.recv() => te.and_then(|e| on_engine_event(&mut rt, e)),
            c = ch.cmd.recv() => match c {
                Some(Cmd::Action(a)) => {
                    if matches!(a, Action::Ui(UiAction::AcceptMagnet | UiAction::DeclineMagnet)) {
                        rt.mirror.s.confirm_magnet = None;
                    }
                    Some(Input::Action(a))
                }
                Some(Cmd::Core(e)) => {
                    if matches!(e, Event::LocalFile(LocalFileEvent::Verifying)) {
                        rt.mirror.file_chosen();
                    }
                    Some(Input::Core(e))
                }
                Some(Cmd::SelectTrack { kind, track_id }) => {
                    select_track_action(&rt, kind, track_id).map(|a| Input::Action(Action::Ui(a)))
                }
                Some(Cmd::AddExternal(p, kind)) => {
                    rt.mirror.event(format!("external file added: {}", p.display_name));
                    if rt.app.state::<RoomManager>().skip_next_relayed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                        rt.skip_relayed.insert(p.blake3);
                    }
                    rt.media.host_prepared(p.blake3, p.path.clone());
                    Some(Input::Action(Action::Ui(UiAction::AddExternal {
                        kind,
                        display_name: p.display_name,
                        format: p.format,
                        size: p.size,
                        blake3: p.blake3,
                    })))
                }
                Some(Cmd::DevReconnect) => session.host_link_peer().map(|peer| {
                    rt.bridge.close_peer(peer);
                    Input::Transport(TransportEvent::Closed {
                        peer,
                        reason: "dev reconnect".into(),
                    })
                }),
                None => Some(Input::Action(Action::Leave)),
            },
            _ = tick.tick() => Some(Input::Tick),
            ev = ch.bulk.recv() => {
                bulk_step = true;
                ev.map(Input::Transport)
            }
        };
        if rt.sig.is_none() && reconnect_at.is_some_and(|t| Instant::now() >= t) {
            match SignalingConn::connect(&url).await {
                Ok(s) => {
                    tracing::info!("signaling reconnected");
                    rt.mirror.s.diag.signaling_connected = true;
                    rt.mirror.s.diag.signaling_reconnects += 1;
                    rt.mirror.event("signaling reconnected");
                    rt.sig = Some(s);
                    reconnect_at = None;
                    backoff = Duration::from_secs(1);
                    let outs = session.handle(clock(epoch), Input::SignalingReconnected);
                    ended |= exec(&mut rt, &session, outs).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "signaling reconnect failed");
                    rt.mirror.event(format!("signaling reconnect failed: {e}"));
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    reconnect_at = Some(Instant::now() + backoff);
                }
            }
        }
        if let Some(input) = input {
            let outs = session.handle(clock(epoch), input);
            ended |= exec(&mut rt, &session, outs).await;
        }
        if !ended && let Some(w) = rt.pending_window.take() {
            let ev = Event::Torrent(TorrentEvent::Window(w));
            let outs = session.handle(clock(epoch), Input::Core(ev));
            ended |= exec(&mut rt, &session, outs).await;
        }
        // The real player's position feeds sync-core (drift correction, ready gate, chat).
        if !ended && last_pos.elapsed() >= POSITION_EVERY {
            last_pos = Instant::now();
            if let Some(o) = crate::roommedia::position_obs(&*rt.player) {
                let outs = session.handle(clock(epoch), Input::Core(Event::Player(o)));
                ended |= exec(&mut rt, &session, outs).await;
            }
        }
        if !ended {
            ended |= after_step(&mut rt, &mut session, epoch).await;
            pump_uploads(&mut rt, &session);
        }
        // A step that only stored a file chunk publishes at most every 100 ms (publishing
        // serializes the whole UI state; per chunk that once cost seconds per transfer).
        if !bulk_step || ended || last_publish.elapsed() >= Duration::from_millis(100) {
            last_publish = Instant::now();
            publish(&app, &mut rt, &session);
        }
        if last_soak.elapsed() >= Duration::from_secs(5) {
            last_soak = Instant::now();
            write_soak(&rt.mirror, epoch, false);
        }
    }
    // Leaving: our torrent stops completely (tracker "stopped", engine down when idle).
    if let Some(ih) = rt.torrent.take() {
        rt.torrents
            .engine
            .stop(ih, syncwatch_torrent::StopReason::Leave)
            .await;
    }
    rt.torrents.detach_room();
    rt.mirror.s.active = false;
    publish(&app, &mut rt, &session);
    write_soak(&rt.mirror, epoch, true);
    // The per-room cache and every pending transfer go with the room.
    rt.media.clear();
    tracing::info!("room session ended");
    rt.mirror.s.ended.clone()
}

/// Host: room-level selection for a local player track (None = off).
fn select_track_action(
    rt: &Rt,
    kind: syncwatch_player::TrackKind,
    track_id: Option<i64>,
) -> Option<UiAction> {
    let sel = match track_id {
        None => Selection::Off,
        Some(id) => {
            let t = rt
                .player
                .tracks()
                .into_iter()
                .find(|t| t.kind == kind && t.id == id)?;
            Selection::Track(rt.media.track_ref(&t)?)
        }
    };
    match kind {
        syncwatch_player::TrackKind::Audio => Some(UiAction::SelectAudio(sel)),
        syncwatch_player::TrackKind::Sub => Some(UiAction::SelectSub(sel)),
        syncwatch_player::TrackKind::Video => None,
    }
}

/// Manifest bookkeeping after each step: the host maps its prepared files to manifest ids;
/// a member reuses identical files already in the per-room cache instead of downloading.
/// The host's engine as a direct peer for a member on the same machine (the swarm may refuse
/// a second connection from the same address).
fn host_peers(port: u16) -> Vec<std::net::SocketAddr> {
    if port == 0 {
        Vec::new()
    } else {
        vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))]
    }
}

fn waiting_dto(size: u64) -> TorrentDto {
    TorrentDto {
        phase: "waitingForHost",
        have_bytes: 0,
        size,
        down_bps: 0,
        up_bps: 0,
        peers: 0,
        eta_secs: None,
        ranges: Vec::new(),
        window: None,
        exclusive: true,
        trackers: 0,
        dht: false,
        web_seeds: 0,
        complete_message: None,
        error: None,
    }
}

/// Member: the host's metadata arrived. It is used only if its info dictionary hashes to the
/// room's announced infohash and the host's chosen file is in it with the announced size.
fn member_meta_received(
    rt: &mut Rt,
    session: &RoomSession,
    infohash: Infohash,
    torrent: Vec<u8>,
    host_port: u16,
) {
    if session.is_host() {
        return;
    }
    let Some(MediaDescriptor::Torrent(d)) = session.peer().local().descriptor.clone() else {
        return;
    };
    let cap = rt.torrents.meta_cap();
    let ok = syncwatch_torrent::meta::inspect(&torrent, cap, Some(infohash))
        .map_err(|e| e.to_string())
        .and_then(|m| {
            let f = m
                .files
                .iter()
                .find(|f| f.index == d.file_index)
                .ok_or_else(|| "the host's file is not in it".to_owned())?;
            if d.infohash != infohash || f.len != d.size {
                return Err("it does not match the room's torrent".to_owned());
            }
            Ok(m.sanitized())
        });
    match ok {
        Ok(bytes) => {
            rt.mirror
                .event("received the torrent's file list from the host");
            rt.member_meta = Some((infohash, bytes.clone(), host_port));
            if let Some(media) = rt.torrent_pending.take()
                && media.infohash == infohash
            {
                rt.torrents
                    .engine
                    .start_with_meta(media, bytes, host_peers(host_port));
            }
        }
        Err(why) => {
            tracing::warn!(reason = %why, "torrent metadata from the host refused");
            rt.mirror.reject(
                None,
                "host".into(),
                format!("torrent metadata refused: {why}"),
            );
            rt.mirror.toast(
                "The host sent torrent metadata that does not match this torrent. It was refused.",
            );
        }
    }
}

/// Torrent engine events -> the UI mirror and sync-core (ADR 0009/0010). Events of an old
/// torrent (after a source switch) only reach the diagnostics.
fn on_engine_event(rt: &mut Rt, ev: syncwatch_torrent::EngineEvent) -> Option<Input> {
    use syncwatch_torrent::{EngineEvent as E, Phase};
    let current = |ih: &Infohash| rt.torrent == Some(*ih);
    match ev {
        E::Status { infohash, status } if current(&infohash) => {
            let phase = match status.phase {
                Phase::Metadata => TorrentPhase::Metadata,
                Phase::Downloading => TorrentPhase::Downloading,
                Phase::Streaming => TorrentPhase::Streaming,
                Phase::NoPeers => TorrentPhase::NoPeers,
                Phase::Complete => TorrentPhase::Complete,
                Phase::Failed => TorrentPhase::Failed,
            };
            let permille = (status.have_bytes.min(status.size) * 1000)
                .checked_div(status.size)
                .unwrap_or(0) as u16;
            let prev_msg = rt
                .mirror
                .s
                .torrent
                .as_ref()
                .and_then(|t| t.complete_message);
            rt.mirror.s.torrent = Some(TorrentDto {
                phase: phase_text(phase),
                have_bytes: status.have_bytes,
                size: status.size,
                down_bps: status.down_bps,
                up_bps: status.up_bps,
                peers: status.peers,
                eta_secs: status.eta_secs,
                ranges: status.ranges.iter().map(|(a, b)| [*a, *b]).collect(),
                window: status.window.map(|w| WindowDto {
                    ready: w.ready,
                    at_ms: w.at_ms,
                    missing: w.missing,
                    permille: w.permille,
                }),
                exclusive: status.exclusive,
                trackers: status.trackers,
                dht: status.dht,
                web_seeds: status.web_seeds,
                complete_message: prev_msg,
                error: None,
            });
            // Readiness at the room position goes to sync-core right after the progress.
            rt.pending_window = Some(status.window.map(|w| WindowState {
                ready: w.ready,
                at_ms: w.at_ms as i64,
                missing: w.missing,
                permille: w.permille,
            }));
            Some(Input::Core(Event::Torrent(TorrentEvent::Progress {
                phase,
                permille,
            })))
        }
        // Our engine added the torrent: sync-core keeps the metadata (the host offers it to
        // members; anyone keeps it for a later host role).
        E::Metadata {
            infohash,
            torrent,
            port,
        } if current(&infohash) => Some(Input::Core(Event::TorrentMeta {
            infohash,
            torrent,
            host_port: port,
        })),
        E::Playable { infohash, url } if current(&infohash) => {
            // The URL carries a 128-bit token: keep it out of every log line (mpv included).
            if let Some(token) = url.split('/').rev().nth(1) {
                crate::logging::register_secret(token);
            }
            rt.media.torrent_url = Some(url);
            rt.mirror
                .event("torrent playable (streaming while downloading)");
            Some(Input::Core(Event::Torrent(TorrentEvent::Playable)))
        }
        E::Complete { infohash } if current(&infohash) => {
            rt.mirror.event(syncwatch_torrent::COMPLETE_MESSAGE);
            if let Some(t) = rt.mirror.s.torrent.as_mut() {
                t.complete_message = Some(syncwatch_torrent::COMPLETE_MESSAGE);
            }
            rt.mirror.toast(syncwatch_torrent::COMPLETE_MESSAGE);
            None
        }
        E::Failed { infohash, error } if current(&infohash) => {
            let text = crate::torrent::engine_error_text(&error);
            rt.mirror.event(format!("torrent failed: {error}"));
            rt.mirror.toast(text);
            if let Some(t) = rt.mirror.s.torrent.as_mut() {
                t.phase = "failed";
                t.error = Some(text);
            }
            Some(Input::Core(Event::Torrent(TorrentEvent::Unavailable)))
        }
        E::Announced {
            reason, results, ..
        } => {
            let ok = results.iter().filter(|(_, ok)| *ok).count();
            rt.mirror.event(format!(
                "trackers told \"stopped\" ({reason:?}): {ok} of {} answered",
                results.len()
            ));
            None
        }
        E::EngineDown => {
            rt.mirror
                .event("torrent engine shut down: no DHT, no listeners, no sockets");
            None
        }
        _ => None,
    }
}

async fn after_step(rt: &mut Rt, session: &mut RoomSession, epoch: Instant) -> bool {
    // The torrent's time -> byte map needs the duration: the player's, or the host's
    // (heartbeat) before this player opened the file (ADR 0012).
    if let (Some(ih), Some(d)) = (rt.torrent, session.peer().local().media_duration_hint())
        && rt.torrent_duration_sent != Some(d)
    {
        rt.torrent_duration_sent = Some(d);
        rt.torrents.engine.duration(ih, Some(d));
    }
    let manifest = session.peer().local().tracks.desired.manifest.clone();
    rt.mirror.set_manifest(&manifest);
    if session.is_host() {
        if !rt.was_host {
            rt.was_host = true;
            // Host succession: the files we downloaded as a member are what we now serve.
            rt.media.promote_member_files();
        }
        rt.media.sync_manifest(&manifest);
        return false;
    }
    rt.was_host = false;
    let mut ended = false;
    for e in &manifest {
        if rt.checked_cache.insert(e.external_id) && rt.media.cached_identical(e) {
            tracing::info!(
                id = e.external_id.0,
                "identical external file already cached; no transfer"
            );
            let outs = session.handle(
                clock(epoch),
                Input::Core(Event::ExternalCached(e.external_id)),
            );
            ended |= exec(rt, session, outs).await;
        }
    }
    ended
}

/// Streams external-file chunks as fast as the bridge's bulk credit allows (no rate limiter;
/// chunks use their own data channel, so control and chat are never queued behind them).
fn pump_uploads(rt: &mut Rt, session: &RoomSession) {
    let mut i = 0;
    while i < rt.media.uploads.len() {
        let (to, id) = (rt.media.uploads[i].to, rt.media.uploads[i].id);
        let Some(peer) = session.member_peer(to) else {
            i += 1;
            continue;
        };
        let hash = session
            .peer()
            .local()
            .tracks
            .desired
            .manifest
            .iter()
            .find(|e| e.external_id == id)
            .map(|e| e.blake3);
        if hash.is_some_and(|h| rt.skip_relayed.contains(&h)) && session.member_relayed(to) {
            tracing::info!(
                member = to.0,
                id = id.0,
                "not sending to a relayed member (host's choice)"
            );
            // Tell the member, so it shows "not sent to you" instead of a stalled download.
            for o in session.send_to_member(to, &Message::FileWithheld { external_id: id }) {
                if let Output::Send { peer, lane, data } = o {
                    let _ = rt.bridge.send(peer, lane, data);
                }
            }
            rt.media.uploads.remove(i);
            continue;
        }
        let mut sent = 0;
        let mut finished = false;
        while sent < UPLOAD_BURST && rt.bridge.can_send_bulk(peer) {
            match rt.media.next_chunk(i) {
                Some(msg) => {
                    for o in session.send_to_member(to, &msg) {
                        if let Output::Send { peer, lane, data } = o {
                            let _ = rt.bridge.send(peer, lane, data);
                        }
                    }
                    sent += 1;
                }
                None => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            rt.media.uploads.remove(i);
        } else {
            i += 1;
        }
    }
}
/// Debug builds: SYNCWATCH_DEV_SOAK_FILE receives banner counters (scripts/dev/two-windows.ps1).
fn write_soak(mirror: &Mirror, epoch: Instant, done: bool) {
    if !cfg!(debug_assertions) {
        return;
    }
    // An empty value (the dev script sets it for every window) means "no soak file".
    let Some(path) = std::env::var("SYNCWATCH_DEV_SOAK_FILE")
        .ok()
        .filter(|p| !p.is_empty())
    else {
        return;
    };
    let members_disconnected = mirror.s.members.iter().filter(|m| !m.connected).count();
    let j = serde_json::json!({
        "elapsedS": epoch.elapsed().as_secs(),
        "inRoom": mirror.s.phase == Some(Phase::InRoom),
        "isHost": mirror.s.is_host,
        "members": mirror.s.members.len(),
        "membersDisconnected": members_disconnected,
        "reconnectingBanners": mirror.banners,
        "linkReconnecting": mirror.link_reconnects,
        "links": mirror.links.values().map(|l| format!("{l:?}")).collect::<Vec<_>>(),
        "ended": mirror.s.ended,
        "done": done,
    });
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, j.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Pushes the UI mirror to the window when it changed.
fn publish(app: &AppHandle, rt: &mut Rt, session: &RoomSession) {
    let mirror = &mut rt.mirror;
    let local = session.peer().local();
    mirror.s.media_epoch = local.room.media_epoch.0;
    let ps = rt.player.state();
    mirror.s.media_duration_ms = ps
        .duration
        .map(|d| (d * 1000.0) as i64)
        .or(local.duration_ms);
    if mirror.s.joining
        && (local.descriptor.is_none() || ps.file_loaded || local.media == MediaStatus::Missing)
    {
        mirror.s.joining = false;
    }
    mirror.s.is_host = session.is_host();
    if session.is_host() {
        mirror.s.me = Some(session.peer().local().me.0);
        // The host may do everything (track control is host-only anyway).
        mirror.s.my_permissions = Permissions::ALL.into();
        mirror.s.has_password = session.has_password();
        if let syncwatch_sync_core::Peer::Host(h) = session.peer() {
            mirror.s.locked = h.is_locked();
            // The host is never muted and can always send; it sees the room's switch.
            mirror.s.chat_state = h.chat_state().into();
        }
    }
    mirror.rebuild_members();
    let m = app.state::<RoomManager>();
    let changed = m
        .state
        .lock()
        .map(|mut g| {
            let new = serde_json::to_string(&mirror.s).unwrap_or_default();
            let old = serde_json::to_string(&*g).unwrap_or_default();
            *g = mirror.s.clone();
            new != old
        })
        .unwrap_or(true);
    if changed {
        let _ = app.emit("room-state", &mirror.s);
        mirror.s.toasts.clear();
    }
}

/// Executes session outputs. Returns true when the session ended.
async fn exec(rt: &mut Rt, session: &RoomSession, outs: Vec<Output>) -> bool {
    let mut ended = false;
    for o in outs {
        match o {
            Output::Signaling(m) => {
                if let Some(s) = rt.sig.as_mut()
                    && s.send(&m).await.is_err()
                {
                    rt.sig = None;
                }
            }
            Output::CreatePeer { peer, role, ice } => {
                if let Err(e) = rt.bridge.create_peer(peer, role, ice) {
                    tracing::warn!(error = %e, "could not create a peer connection");
                }
            }
            Output::RemoteSignal { peer, data } => {
                let _ = rt.bridge.remote_signal(peer, data);
            }
            Output::Send { peer, lane, data } => {
                let _ = rt.bridge.send(peer, lane, data);
            }
            Output::ClosePeer(peer) => rt.bridge.close_peer(peer),
            Output::Player(cmd) => {
                let chosen = rt
                    .app
                    .state::<RoomManager>()
                    .chosen
                    .lock()
                    .ok()
                    .and_then(|g| g.clone());
                // The host's own new external file: sync-core asks to add it in the same step
                // that created its manifest id, so map prepared files to ids first.
                if matches!(cmd, syncwatch_sync_core::PlayerCmd::AddExternal { .. })
                    && session.is_host()
                {
                    rt.media
                        .sync_manifest(&session.peer().local().tracks.desired.manifest);
                }
                // A room seek re-targets the torrent download before mpv even asks.
                if let (syncwatch_sync_core::PlayerCmd::Seek(ms), Some(ih)) = (&cmd, rt.torrent) {
                    rt.torrents.engine.seek(ih, (*ms).max(0) as u64);
                }
                rt.media.player_cmd(&*rt.player, chosen.as_deref(), cmd);
            }
            Output::Core(e) => core_effect(rt, session, e),
            Output::Ui(u) => rt.mirror.apply(u),
            Output::Ended(r) => {
                tracing::info!(reason = ?r, "left the room");
                rt.mirror.event(format!("session ended: {r:?}"));
                rt.mirror.s.ended = Some(r);
                ended = true;
            }
        }
    }
    ended
}

/// Effects for the app: external-file storage and uploads (torrents: Milestone 7).
fn core_effect(rt: &mut Rt, session: &RoomSession, e: syncwatch_sync_core::Effect) {
    use syncwatch_sync_core::Effect as E;
    let format_of = |id: ExternalId| {
        session
            .peer()
            .local()
            .tracks
            .desired
            .manifest
            .iter()
            .find(|m| m.external_id == id)
            .map(|m| m.format.clone())
            .unwrap_or_default()
    };
    match e {
        E::WriteExternalChunk {
            external_id,
            offset,
            data,
        } => {
            if let Err(err) =
                rt.media
                    .write_chunk(external_id, &format_of(external_id), offset, &data)
            {
                tracing::warn!(error = %err, "could not write to the room cache");
            }
        }
        E::ExternalComplete { external_id } => {
            rt.media.complete(external_id);
            rt.mirror
                .event(format!("external file {} verified", external_id.0));
        }
        E::DiscardExternal { external_id } => rt.media.discard(external_id),
        E::StartUpload {
            to,
            external_id,
            offset,
        } => rt.media.start_upload(to, external_id, offset),
        E::CancelUpload { to, external_id } => {
            rt.media.cancel_upload(to, external_id);
            rt.mirror.declined(to.0, external_id);
        }
        E::StartTorrent {
            infohash,
            file_index,
        } => {
            let media = match session.peer().local().descriptor.clone() {
                Some(MediaDescriptor::Torrent(t))
                    if t.infohash == infohash && t.file_index == file_index =>
                {
                    t
                }
                _ => return,
            };
            if let Some(old) = rt.torrent.replace(infohash)
                && old != infohash
            {
                rt.torrents
                    .engine
                    .stop_nowait(old, syncwatch_torrent::StopReason::Switch);
            }
            rt.torrent_duration_sent = None;
            rt.media.torrent_url = None;
            rt.mirror.event(format!("torrent started: {}", media.name));
            if session.is_host() {
                // The host gets the file list itself (or already has it from the dialog / a
                // .torrent file) and then offers it to the members.
                match rt.torrents.cached_meta(&infohash) {
                    Some(b) => rt.torrents.engine.start_with_meta(media, b, Vec::new()),
                    None => rt.torrents.engine.start(media),
                }
            } else {
                match &rt.member_meta {
                    // A member never asks the swarm for the file list: the host's verified
                    // metadata, and the host's engine as a peer (same machine / LAN).
                    Some((ih, b, port)) if *ih == infohash => {
                        let peers = host_peers(*port);
                        rt.torrents.engine.start_with_meta(media, b.clone(), peers);
                    }
                    _ => {
                        rt.mirror
                            .event("waiting for the host to get the torrent's file list");
                        rt.mirror.s.torrent = Some(waiting_dto(media.size));
                        // Already finished here (switching back, rejoin): plays at once, no
                        // swarm; otherwise nothing happens until the host's metadata arrives.
                        rt.torrents.engine.start_cached_only(media.clone());
                        rt.torrent_pending = Some(media);
                    }
                }
            }
        }
        E::TorrentTarget {
            infohash,
            position_ms,
            playing,
            speed,
            retarget,
        } => rt.torrents.engine.room(
            infohash,
            syncwatch_torrent::RoomTarget {
                ms: position_ms.max(0) as u64,
                playing,
                speed,
                retarget,
            },
        ),
        E::StopTorrent { infohash } => {
            if rt.torrent == Some(infohash) {
                rt.torrent = None;
                rt.media.torrent_url = None;
            }
            rt.torrent_pending = None;
            rt.member_meta = None;
            rt.mirror.event("torrent stopped (source changed)");
            // Never blocks: the engine announces "stopped" in the background.
            rt.torrents
                .engine
                .stop_nowait(infohash, syncwatch_torrent::StopReason::Switch);
        }
        E::TorrentMetaReceived {
            infohash,
            torrent,
            host_port,
        } => member_meta_received(rt, session, infohash, torrent, host_port),
        other => tracing::debug!(effect = ?std::mem::discriminant(&other), "unhandled effect"),
    }
}
// ---------------------------------------------------------------------------- commands ------

#[tauri::command]
pub async fn room_create(
    app: AppHandle,
    name: String,
    password: Option<String>,
) -> Result<EntryOutcome, String> {
    let password = password.filter(|p| !p.is_empty());
    if password.as_ref().is_some_and(|p| p.len() > 256) {
        return Err("That password is too long.".into());
    }
    start(app, name, Mode::Create { password }).await
}

#[tauri::command]
pub async fn room_join(
    app: AppHandle,
    name: String,
    code: String,
    password: Option<String>,
) -> Result<EntryOutcome, String> {
    let Some(room) = normalize_room_code(&code) else {
        return Err(
            "Room codes have 8 characters (letters and digits, no 0, 1, I, L or O).".into(),
        );
    };
    start(
        app,
        name,
        Mode::Join {
            room,
            password: password.filter(|p| !p.is_empty()),
        },
    )
    .await
}

#[tauri::command]
pub fn room_state(m: State<'_, RoomManager>) -> RoomStateDto {
    m.state.lock().map(|g| g.clone()).unwrap_or_default()
}

#[tauri::command]
pub fn room_leave(m: State<'_, RoomManager>) -> Result<(), String> {
    m.send(Cmd::Action(Action::Leave))
}

#[tauri::command]
pub fn room_password(m: State<'_, RoomManager>, password: String) -> Result<(), String> {
    m.send(Cmd::Action(Action::SubmitPassword(password)))
}

/// Room actions from the UI (validated by the session and the host).
///
/// The wire shape is the TypeScript `RoomAction` type in `src/lib/room.ts`: variant names AND
/// field names are camelCase (`rename_all` alone renames only the variant names; that once
/// broke every action with a multi-word field). `room_actions_contract` (below) and
/// `room.contract.test.ts` keep both sides on the same fixture.
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RoomActionIn {
    Chat {
        client_id: u32,
        text: String,
    },
    SetPermissions {
        member: u32,
        permissions: PermsIn,
    },
    Preset {
        preset: String,
    },
    Kick {
        member: u32,
    },
    Ban {
        member: u32,
    },
    Lock {
        locked: bool,
    },
    MakeHost {
        member: u32,
    },
    MuteChat {
        member: u32,
        muted: bool,
    },
    DeleteChat {
        id: u64,
    },
    ClearChat,
    ChatConfig {
        enabled: bool,
        slow_mode_secs: u16,
    },
    LocalMute {
        member: u32,
        muted: bool,
    },
    WaitForEveryone {
        on: bool,
    },
    /// The member's file differs from the host's; continue anyway.
    ProceedWithMismatch,
    // --- playback (requests for members; the host validates and broadcasts) ---
    Play,
    Pause,
    Seek {
        ms: i64,
    },
    SetSpeed {
        permille: u32,
    },
    ResumeWithoutWaiting,
    /// Member: download the host's magnet (after seeing its name and size).
    AcceptMagnet,
    DeclineMagnet,
    /// Member: stop downloading one external file (the host sees "declined").
    CancelDownload {
        external_id: u32,
    },
    /// Host: subtitle or audio delay in milliseconds.
    SetDelay {
        kind: String,
        ms: i32,
    },
}

impl RoomActionIn {
    /// Every variant's wire name (the contract test checks the fixture covers all of them).
    #[cfg(test)]
    fn wire_name(&self) -> &'static str {
        match self {
            RoomActionIn::Chat { .. } => "chat",
            RoomActionIn::SetPermissions { .. } => "setPermissions",
            RoomActionIn::Preset { .. } => "preset",
            RoomActionIn::Kick { .. } => "kick",
            RoomActionIn::Ban { .. } => "ban",
            RoomActionIn::Lock { .. } => "lock",
            RoomActionIn::MakeHost { .. } => "makeHost",
            RoomActionIn::MuteChat { .. } => "muteChat",
            RoomActionIn::DeleteChat { .. } => "deleteChat",
            RoomActionIn::ClearChat => "clearChat",
            RoomActionIn::ChatConfig { .. } => "chatConfig",
            RoomActionIn::LocalMute { .. } => "localMute",
            RoomActionIn::WaitForEveryone { .. } => "waitForEveryone",
            RoomActionIn::ProceedWithMismatch => "proceedWithMismatch",
            RoomActionIn::Play => "play",
            RoomActionIn::Pause => "pause",
            RoomActionIn::Seek { .. } => "seek",
            RoomActionIn::SetSpeed { .. } => "setSpeed",
            RoomActionIn::ResumeWithoutWaiting => "resumeWithoutWaiting",
            RoomActionIn::CancelDownload { .. } => "cancelDownload",
            RoomActionIn::SetDelay { .. } => "setDelay",
            RoomActionIn::AcceptMagnet => "acceptMagnet",
            RoomActionIn::DeclineMagnet => "declineMagnet",
        }
    }
}

#[tauri::command]
pub fn room_action(m: State<'_, RoomManager>, action: RoomActionIn) -> Result<(), String> {
    let ui = to_ui_action(action)?;
    m.send(Cmd::Action(Action::Ui(ui)))
}

fn to_ui_action(action: RoomActionIn) -> Result<UiAction, String> {
    Ok(match action {
        RoomActionIn::Chat { client_id, text } => UiAction::SendChat { client_id, text },
        RoomActionIn::SetPermissions {
            member,
            permissions,
        } => UiAction::SetPermissions {
            member: MemberId(member),
            permissions: permissions.into(),
        },
        RoomActionIn::Preset { preset } => {
            let p = match preset.as_str() {
                "everyone" => Preset::EveryoneControls,
                "onlyMe" => Preset::OnlyMe,
                _ => return Err("Unknown preset.".into()),
            };
            UiAction::ApplyPreset {
                preset: p,
                custom: Permissions::ALL,
            }
        }
        RoomActionIn::Kick { member } => UiAction::Kick(MemberId(member)),
        RoomActionIn::Ban { member } => UiAction::Ban(MemberId(member)),
        RoomActionIn::Lock { locked } => UiAction::Lock(locked),
        RoomActionIn::MakeHost { member } => UiAction::MakeHost(MemberId(member)),
        RoomActionIn::MuteChat { member, muted } => UiAction::Moderate(if muted {
            Moderation::Mute(MemberId(member))
        } else {
            Moderation::Unmute(MemberId(member))
        }),
        RoomActionIn::DeleteChat { id } => UiAction::Moderate(Moderation::Delete(ChatId(id))),
        RoomActionIn::ClearChat => UiAction::Moderate(Moderation::Clear),
        RoomActionIn::ChatConfig {
            enabled,
            slow_mode_secs,
        } => UiAction::SetChatConfig(ChatConfig {
            enabled,
            slow_mode_secs: slow_mode_secs.min(300),
        }),
        RoomActionIn::LocalMute { member, muted } => UiAction::LocalMute {
            member: MemberId(member),
            muted,
        },
        RoomActionIn::WaitForEveryone { on } => UiAction::SetWaitForEveryone(on),
        RoomActionIn::ProceedWithMismatch => UiAction::ProceedWithMismatch,
        RoomActionIn::Play => UiAction::Play,
        RoomActionIn::Pause => UiAction::Pause,
        RoomActionIn::Seek { ms } => UiAction::Seek(ms.max(0)),
        RoomActionIn::SetSpeed { permille } => UiAction::SetSpeed(permille.clamp(250, 4000)),
        RoomActionIn::ResumeWithoutWaiting => UiAction::ResumeWithoutWaiting,
        RoomActionIn::AcceptMagnet => UiAction::AcceptMagnet,
        RoomActionIn::DeclineMagnet => UiAction::DeclineMagnet,
        RoomActionIn::CancelDownload { external_id } => {
            UiAction::CancelExternalDownload(ExternalId(external_id))
        }
        RoomActionIn::SetDelay { kind, ms } => {
            let ms = ms.clamp(-600_000, 600_000);
            match kind.as_str() {
                "audio" => UiAction::SetAudioDelay(ms),
                "sub" => UiAction::SetSubDelay(ms),
                _ => return Err("Unknown delay kind.".into()),
            }
        }
    })
}

/// "Choose my file" (any member, NO permission): hash the chosen file and report it. The path
/// stays on this machine.
#[tauri::command]
pub async fn room_choose_file(app: AppHandle, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("That file could not be found.".into());
    }
    let m = app.state::<RoomManager>();
    if let Ok(mut g) = m.chosen.lock() {
        *g = Some(p.clone());
    }
    m.send(Cmd::Core(Event::LocalFile(LocalFileEvent::Verifying)))?;
    let id = tauri::async_runtime::spawn_blocking(move || crate::media::identify_file(&p))
        .await
        .map_err(|_| "The file could not be read.".to_owned())?;
    let ev = match id {
        Ok(id) => LocalFileEvent::Identified(id),
        Err(_) => LocalFileEvent::Failed,
    };
    app.state::<RoomManager>()
        .send(Cmd::Core(Event::LocalFile(ev)))
}

/// Host: select an audio or subtitle track by its local player id (null = off). Applied on
/// every member immediately, with no seek.
#[tauri::command]
pub fn room_select_track(
    m: State<'_, RoomManager>,
    kind: syncwatch_player::TrackKind,
    track_id: Option<i64>,
) -> Result<(), String> {
    m.send(Cmd::SelectTrack { kind, track_id })
}

/// Result of `room_add_external`: either added, or a warning the host must confirm first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddExternalOutcome {
    added: bool,
    /// Members whose path goes through a relay (a large file uses their Metered quota).
    relayed_members: Vec<String>,
    size: u64,
    converted_from: Option<String>,
}

/// Host: add an external subtitle or audio file. policy: "ask" (warn first when a large file
/// would go to relayed members), "all" or "skipRelayed".
#[tauri::command]
pub async fn room_add_external(
    app: AppHandle,
    kind: String,
    path: String,
    no_limit: bool,
    policy: String,
) -> Result<AddExternalOutcome, String> {
    let kind = match kind.as_str() {
        "sub" => ExternalKind::Subtitle,
        "audio" => ExternalKind::Audio,
        _ => return Err("Unknown file kind.".into()),
    };
    let src = PathBuf::from(&path);
    if !src.is_file() {
        return Err("That file could not be found.".into());
    }
    let m = app.state::<RoomManager>();
    let s = m.state.lock().map(|g| g.clone()).unwrap_or_default();
    if !s.active || !s.is_host {
        return Err("Only the host can add audio or subtitle files.".into());
    }
    let room_dir = m.rooms_cache.join(s.session_id.to_string());
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        crate::roommedia::prepare_external(&room_dir, &src, kind, no_limit)
    })
    .await
    .map_err(|_| "That file could not be read.".to_owned())??;
    let relayed: Vec<String> = s
        .members
        .iter()
        .filter(|mm| !mm.is_you && mm.link == Some(LinkState::Relayed))
        .map(|mm| mm.name.clone())
        .collect();
    let size = prepared.size;
    let converted_from = prepared.converted_from.clone();
    if policy == "ask" && size > crate::roommedia::RELAYED_WARN_BYTES && !relayed.is_empty() {
        let _ = std::fs::remove_file(&prepared.path);
        return Ok(AddExternalOutcome {
            added: false,
            relayed_members: relayed,
            size,
            converted_from,
        });
    }
    if policy == "skipRelayed" {
        m.skip_next_relayed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    m.send(Cmd::AddExternal(prepared, kind))?;
    Ok(AddExternalOutcome {
        added: true,
        relayed_members: Vec::new(),
        size,
        converted_from,
    })
}

/// Host or a member with can_change_media: make this local file the room's media.
#[tauri::command]
pub async fn room_set_local_media(app: AppHandle, path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        return Err("That file could not be found.".into());
    }
    let p2 = p.clone();
    let identity = tauri::async_runtime::spawn_blocking(move || crate::media::identify_file(&p2))
        .await
        .map_err(|_| "The file could not be read.".to_owned())?
        .map_err(|_| "The file could not be read.".to_owned())?;
    let name = crate::media::file_name(&p);
    let m = app.state::<RoomManager>();
    if let Ok(mut g) = m.chosen.lock() {
        *g = Some(p);
    }
    m.send(Cmd::Action(Action::Ui(UiAction::ChangeMedia {
        descriptor: MediaDescriptor::LocalFile(LocalMedia { name, identity }),
        own_file: true,
    })))
}

/// Host, Change source -> Torrent: the room plays file `file_index` of this magnet. Members
/// confirm before downloading; the host's own download starts at once.
#[tauri::command]
pub fn room_set_torrent_media(
    app: AppHandle,
    magnet: String,
    file_index: u32,
    name: String,
    size: u64,
) -> Result<(), String> {
    let m = syncwatch_protocol::magnet::parse(&magnet)
        .map_err(|_| "This is not a valid magnet link.".to_owned())?;
    // The display name is the file's last path component, sanitized like a local file name.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_owned();
    let name = crate::media::file_name(std::path::Path::new(&base));
    let descriptor = MediaDescriptor::Torrent(TorrentMedia {
        infohash: m.infohash,
        file_index,
        name,
        size,
        magnet,
    });
    descriptor
        .validate()
        .map_err(|_| "This torrent cannot be used (name or link too long).".to_owned())?;
    app.state::<RoomManager>()
        .send(Cmd::Action(Action::Ui(UiAction::ChangeMedia {
            descriptor,
            own_file: false,
        })))
}

/// Host, Change source -> Torrent file: the room plays file `file_index` of the .torrent the
/// host inspected (its validated bytes are offered to members; its trackers go into the magnet
/// and the announces).
#[tauri::command]
pub fn room_set_torrent_file(
    app: AppHandle,
    infohash: String,
    file_index: u32,
) -> Result<(), String> {
    let svc = Arc::clone(&*app.state::<Arc<crate::torrent::TorrentService>>());
    let ih = syncwatch_protocol::magnet::parse(&format!("magnet:?xt=urn:btih:{infohash}"))
        .map_err(|_| "Unknown torrent.".to_owned())?
        .infohash;
    let bytes = svc
        .cached_meta(&ih)
        .ok_or_else(|| "Open the .torrent file again.".to_owned())?;
    let m = syncwatch_torrent::meta::inspect(&bytes, svc.meta_cap(), Some(ih))
        .map_err(|e| crate::torrent::meta_error_text(&e).to_owned())?;
    let f = m
        .files
        .iter()
        .find(|f| f.index == file_index && !f.padding)
        .ok_or_else(|| "That file is not in the torrent.".to_owned())?;
    let base = f.path.rsplit('/').next().unwrap_or(&f.path).to_owned();
    let descriptor = MediaDescriptor::Torrent(TorrentMedia {
        infohash: ih,
        file_index,
        name: crate::media::file_name(std::path::Path::new(&base)),
        size: f.len,
        magnet: m.magnet(),
    });
    descriptor
        .validate()
        .map_err(|_| "This torrent cannot be used (name or link too long).".to_owned())?;
    app.state::<RoomManager>()
        .send(Cmd::Action(Action::Ui(UiAction::ChangeMedia {
            descriptor,
            own_file: false,
        })))
}

/// Debug builds: the scripted two-window scenario to run (SYNCWATCH_DEV_SCENARIO), if any.
#[derive(Serialize)]
pub struct DevScenario {
    name: String,
    role: String,
    /// A video the scenario may load (SYNCWATCH_DEV_SCENARIO_MEDIA; only read, never written).
    media: Option<String>,
    /// Extra files (SYNCWATCH_DEV_SCENARIO_EXTRA, ';'-separated; only read).
    extra: Vec<String>,
}

fn dev_shared_file() -> Option<PathBuf> {
    let f = std::env::var("SYNCWATCH_DEV_ROOM_FILE").ok()?;
    Some(PathBuf::from(format!("{f}.room2")))
}

/// Debug builds: the host publishes a second room code for the scenario.
#[tauri::command]
pub fn dev_scenario_share(code: String) {
    if cfg!(debug_assertions)
        && normalize_room_code(&code).is_some()
        && let Some(p) = dev_shared_file()
    {
        let _ = std::fs::write(p, code);
    }
}

/// Debug builds: the second room code, once the host published it.
#[tauri::command]
pub fn dev_scenario_shared() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::fs::read_to_string(dev_shared_file()?)
        .ok()
        .map(|s| s.trim().to_owned())
}

#[tauri::command]
pub fn dev_scenario() -> Option<DevScenario> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let name = std::env::var("SYNCWATCH_DEV_SCENARIO")
        .ok()
        .filter(|s| !s.is_empty())?;
    let role = std::env::var("SYNCWATCH_DEV_AUTOROOM")
        .ok()
        .filter(|s| !s.is_empty())?;
    let media = std::env::var("SYNCWATCH_DEV_SCENARIO_MEDIA")
        .ok()
        .filter(|s| !s.is_empty());
    let role = std::env::var("SYNCWATCH_DEV_SCENARIO_ROLE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or(role);
    let extra = std::env::var("SYNCWATCH_DEV_SCENARIO_EXTRA")
        .map(|s| {
            s.split(';')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Some(DevScenario {
        name,
        role,
        media,
        extra,
    })
}

/// Debug builds: drop this member's link to the host to force a reconnect (scenario hook).
#[tauri::command]
pub fn dev_reconnect(m: State<'_, RoomManager>) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err("debug builds only".into());
    }
    m.send(Cmd::DevReconnect)
}

/// Debug builds: window state for the popup regression scenario. Uses the window API (no
/// mouse or keyboard input). op: "minimize", "restore" or "query"; returns whether the
/// main window is minimized and whether the video window is visible.
#[tauri::command]
pub fn dev_window(app: AppHandle, op: String) -> Result<(bool, bool), String> {
    if !cfg!(debug_assertions) {
        return Err("debug builds only".into());
    }
    let w = app.get_webview_window("main").ok_or("no main window")?;
    match op.as_str() {
        "minimize" => w.minimize().map_err(|e| e.to_string())?,
        "restore" => w.unminimize().map_err(|e| e.to_string())?,
        _ => {}
    }
    let minimized = w.is_minimized().map_err(|e| e.to_string())?;
    let video_visible = app.state::<AppState>().video_visible();
    Ok((minimized, video_visible))
}

/// Debug builds: appends one result line of the scenario to SYNCWATCH_DEV_SCENARIO_FILE.
#[tauri::command]
pub fn dev_scenario_report(line: String) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(path) = std::env::var("SYNCWATCH_DEV_SCENARIO_FILE") else {
        return;
    };
    let line: String = line.chars().filter(|c| !c.is_control()).take(300).collect();
    tracing::info!(%line, "dev scenario");
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Debug builds: SYNCWATCH_DEV_AUTOROOM=host|join creates or joins a room at startup (the room
/// code goes through `$SYNCWATCH_DEV_ROOM_FILE`). Used by scripts/dev/two-windows.ps1 and the
/// soak test; no mouse or keyboard input involved.
pub fn dev_autoroom(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(role) = std::env::var("SYNCWATCH_DEV_AUTOROOM") else {
        return;
    };
    let Ok(file) = std::env::var("SYNCWATCH_DEV_ROOM_FILE") else {
        return;
    };
    let app = app.clone();
    let name = std::env::var("SYNCWATCH_DEV_NAME").unwrap_or_else(|_| role.clone());
    let password = std::env::var("SYNCWATCH_DEV_ROOM_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        // "manual": the scenario joins by itself at the right moment (late joiner checks).
        if role == "manual" {
            return;
        }
        let res = if role == "host" {
            let r = room_create(app.clone(), name, password).await;
            // Publish the code once it exists.
            for _ in 0..100 {
                let code = app
                    .state::<RoomManager>()
                    .state
                    .lock()
                    .ok()
                    .and_then(|g| g.room_code.clone());
                if let Some(c) = code {
                    let _ = std::fs::write(&file, c);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            r
        } else {
            let mut code = None;
            for _ in 0..300 {
                if let Ok(c) = std::fs::read_to_string(&file) {
                    code = Some(c.trim().to_owned());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            match code {
                Some(c) => room_join(app.clone(), name, c, password).await,
                None => Err("no room code".into()),
            }
        };
        if let Err(e) = res {
            tracing::warn!(error = %e, "dev auto-room failed");
        }
        if std::env::var("SYNCWATCH_DEV_MINIMIZE").as_deref() == Ok("1") {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.minimize();
                tracing::info!("dev: window minimized for the soak test");
            }
        }
        let _ = app.emit("room-dev-autoroom", ());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact arguments object the UI passes to `invoke('room_action', ...)`.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RoomActionArgs {
        action: RoomActionIn,
    }

    const ALL_VARIANTS: [&str; 23] = [
        "acceptMagnet",
        "declineMagnet",
        "play",
        "pause",
        "seek",
        "setSpeed",
        "resumeWithoutWaiting",
        "cancelDownload",
        "setDelay",
        "chat",
        "setPermissions",
        "preset",
        "kick",
        "ban",
        "lock",
        "makeHost",
        "muteChat",
        "deleteChat",
        "clearChat",
        "chatConfig",
        "localMute",
        "waitForEveryone",
        "proceedWithMismatch",
    ];

    /// Contract: every payload the TypeScript UI builds (written by `room.contract.test.ts`)
    /// deserializes into the real command argument type, and every Rust variant is covered.
    #[test]
    fn room_actions_contract() {
        let fixture: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../src/lib/room-actions.fixture.json")).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for args in fixture {
            let parsed: RoomActionArgs = serde_json::from_value(args.clone())
                .unwrap_or_else(|e| panic!("UI payload {args} does not match RoomActionIn: {e}"));
            seen.insert(parsed.action.wire_name());
            // And it converts to a sync-core action without error.
            assert!(to_ui_action(parsed.action).is_ok(), "{args}");
        }
        let all: std::collections::BTreeSet<&str> = ALL_VARIANTS.into_iter().collect();
        assert_eq!(seen, all, "the fixture must cover every room action");
    }

    #[test]
    fn a_media_change_forgets_the_previous_missing_file() {
        let mut mirror = Mirror::default();
        mirror.apply(RoomUi::Core(UiEvent::MediaChanged {
            title: Some("a.mkv".into()),
        }));
        mirror.apply(RoomUi::Core(UiEvent::ShowMissingFilePanel {
            name: "a.mkv".into(),
            size: 1,
            duration_ms: None,
        }));
        assert!(mirror.s.missing_file.is_some());
        // The host switches to a torrent: no "Choose a different file..." any more.
        mirror.apply(RoomUi::Core(UiEvent::MediaChanged {
            title: Some("film.mkv".into()),
        }));
        assert!(mirror.s.missing_file.is_none());
    }

    #[test]
    fn snake_case_fields_are_refused() {
        let bad = serde_json::json!({ "action": { "type": "chat", "client_id": 1, "text": "x" } });
        assert!(serde_json::from_value::<RoomActionArgs>(bad).is_err());
    }

    fn info(id: u32, name: &str, connected: bool, muted: bool) -> MemberInfo {
        MemberInfo {
            id: MemberId(id),
            display_name: name.into(),
            is_host: id == 0,
            permissions: Permissions::ALL,
            media: MediaStatus::Missing,
            buffering: false,
            tracks: TrackSyncStatus::InSync,
            chat_muted: muted,
            connected,
            torrent: None,
            wait: None,
        }
    }

    fn manager() -> RoomManager {
        RoomManager {
            key: IdentityKey::from_secret([9; 32]),
            cmd: Mutex::new(None),
            transport: Mutex::new(None),
            state: Mutex::new(RoomStateDto::default()),
            chosen: Mutex::new(None),
            sessions: std::sync::atomic::AtomicU32::new(0),
            player_tx: Mutex::new(None),
            rooms_cache: std::env::temp_dir().join("sw-test-rooms"),
            skip_next_relayed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Spec: on leave/close/kick/ban/room end, reset ALL room-scoped state; a new room in the
    /// same app process starts with nothing from the old one.
    #[test]
    fn nothing_from_a_room_survives_into_the_next() {
        use syncwatch_player::mock::MockPlayer;
        let (player, _rx) = MockPlayer::new();
        let m = manager();
        // --- room 1, fully used: a video loaded, a chosen file, members, chat, panel, words.
        player.load(r"D:\media\old.mkv").unwrap();
        assert!(player.state().file_loaded);
        *m.chosen.lock().unwrap() = Some(PathBuf::from(r"D:\media\old.mkv"));
        let mut mirror = Mirror::default();
        mirror.s.active = true;
        mirror.apply(RoomUi::Created {
            room: "ROOMAAAA".into(),
        });
        mirror.apply(RoomUi::Core(UiEvent::Members(vec![
            info(0, "H", true, false),
            info(1, "Mo", true, true),
        ])));
        mirror.apply(RoomUi::Core(UiEvent::Permissions(Permissions::NONE)));
        mirror.apply(RoomUi::Core(UiEvent::MediaChanged {
            title: Some("old.mkv".into()),
        }));
        mirror.apply(RoomUi::Core(UiEvent::ShowMissingFilePanel {
            name: "old.mkv".into(),
            size: 1,
            duration_ms: None,
        }));
        mirror.apply(RoomUi::Verification {
            member: None,
            words: ["a", "b", "c", "d"],
        });
        mirror.apply(RoomUi::PasswordIgnored);
        mirror.apply(RoomUi::Rejected {
            member: Some(MemberId(1)),
            what: "x".into(),
        });
        *m.state.lock().unwrap() = mirror.s.clone();
        // --- room 1 ends (close, leave, kick, ban and room end all go through this).
        reset_room_scope(&player, &m, Some(EndReason::RoomClosed));
        assert!(!player.state().file_loaded, "mpv unloaded");
        assert!(player.tracks().is_empty(), "no tracks left");
        assert!(player.state().time_pos.is_none());
        assert!(m.chosen.lock().unwrap().is_none(), "chosen file forgotten");
        let after = m.state.lock().unwrap().clone();
        let empty = RoomStateDto {
            ended: Some(EndReason::RoomClosed),
            ..RoomStateDto::default()
        };
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&empty).unwrap(),
            "only the end reason is kept"
        );
        // --- room 2 is created in the same process: start() resets again, then a new session.
        reset_room_scope(&player, &m, None);
        let cfg = SessionConfig {
            key: m.key.clone(),
            display_name: "H".into(),
            ice: IceConfig::default(),
            max_members: 12,
            gate_timeout_us: None,
        };
        let clock = Clock {
            mono_us: 5_000_000,
            wall_ms: 0,
        };
        let (s, outs) = RoomSession::create(cfg, None, clock);
        let syncwatch_sync_core::Peer::Host(h) = s.peer() else {
            panic!()
        };
        assert!(
            h.descriptor().is_none(),
            "no media until the host picks a source"
        );
        assert_eq!(h.room().media_epoch, MediaEpoch(0));
        assert_eq!(h.room().position_ms, 0);
        assert!(h.room().paused);
        assert_eq!(
            *h.tracks(),
            TrackState::default(),
            "default track state, empty manifest"
        );
        assert!(h.chat_history().is_empty(), "no chat history");
        assert!(h.bans().is_empty() && !h.is_locked() && h.connected_members().is_empty());
        let mut fresh = Mirror::default();
        for o in outs {
            if let Output::Ui(u) = o {
                fresh.apply(u);
            }
        }
        assert!(
            fresh.s.chat.is_empty()
                && fresh.s.missing_file.is_none()
                && fresh.s.media_title.is_none()
        );
        assert!(!player.state().file_loaded);
        assert!(m.state.lock().unwrap().ended.is_none());
    }

    #[test]
    fn choose_my_file_panel_closes_for_good_once_a_file_is_chosen() {
        let mut m = Mirror::default();
        let panel = UiEvent::ShowMissingFilePanel {
            name: "movie.mkv".into(),
            size: 10,
            duration_ms: Some(1000),
        };
        m.apply(RoomUi::Core(panel.clone()));
        assert!(
            m.s.missing_file.as_ref().unwrap().panel_open,
            "shown on join"
        );
        m.file_chosen();
        assert!(
            !m.s.missing_file.as_ref().unwrap().panel_open,
            "closed as soon as a file is picked"
        );
        assert_eq!(m.s.file_status, Some("verifying"));
        m.apply(RoomUi::Core(UiEvent::FileVerified));
        assert!(
            !m.s.missing_file.as_ref().unwrap().panel_open,
            "stays closed after verification"
        );
        assert_eq!(m.s.file_status, Some("verified"));
        // Unrelated updates (members, chat, permissions) never reopen it.
        m.apply(RoomUi::Core(UiEvent::Members(vec![info(
            0, "H", true, false,
        )])));
        m.apply(RoomUi::Core(UiEvent::Permissions(Permissions::NONE)));
        assert!(!m.s.missing_file.as_ref().unwrap().panel_open);
        // A mismatch closes it too (the warning is shown in the room instead).
        m.apply(RoomUi::Core(panel.clone()));
        let seq2 = m.s.missing_file.as_ref().unwrap().seq;
        assert!(
            m.s.missing_file.as_ref().unwrap().panel_open,
            "a new media change shows it once more"
        );
        m.file_chosen();
        m.apply(RoomUi::Core(UiEvent::FileMismatch {
            expected: FileIdentity {
                sample_blake3: [1; 32],
                size: 1,
                duration_ms: None,
            },
            got: FileIdentity {
                sample_blake3: [2; 32],
                size: 1,
                duration_ms: None,
            },
        }));
        assert!(!m.s.missing_file.as_ref().unwrap().panel_open);
        assert_eq!(m.s.file_status, Some("mismatch"));
        assert_eq!(m.s.missing_file.as_ref().unwrap().seq, seq2);
    }

    /// Regression (M7 Part A): the member's chat state must reach the UI mirror on every
    /// change: it once fell through a catch-all and the input stayed enabled while muted.
    #[test]
    fn chat_state_updates_reach_the_ui_mirror() {
        let mut m = Mirror::default();
        assert!(
            m.s.chat_state.enabled && !m.s.chat_state.muted,
            "default: chat on"
        );
        let st = |enabled, muted| {
            RoomUi::Core(UiEvent::ChatState(ChatState {
                config: ChatConfig {
                    enabled,
                    slow_mode_secs: 0,
                },
                muted,
            }))
        };
        m.apply(st(true, true));
        assert!(m.s.chat_state.muted);
        m.apply(st(false, true));
        assert!(!m.s.chat_state.enabled && m.s.chat_state.muted);
        m.apply(st(true, false));
        assert!(m.s.chat_state.enabled && !m.s.chat_state.muted);
        // A muted / chat-off rejection is a normal state: no error toast.
        m.s.toasts.clear();
        m.apply(RoomUi::Core(UiEvent::ChatRejected {
            client_id: 1,
            reason: ChatRejectReason::Muted,
        }));
        assert!(m.s.toasts.is_empty());
    }

    #[test]
    fn own_permissions_follow_the_host_live() {
        let mut m = Mirror::default();
        m.apply(RoomUi::Core(UiEvent::Permissions(Permissions::ALL)));
        assert!(m.s.my_permissions.can_seek);
        let mut p = Permissions::ALL;
        p.can_seek = false;
        m.apply(RoomUi::Core(UiEvent::Permissions(p)));
        assert!(!m.s.my_permissions.can_seek && m.s.my_permissions.can_play_pause);
    }

    #[test]
    fn password_ignored_notice_and_diagnostics() {
        let mut m = Mirror::default();
        m.apply(RoomUi::PasswordIgnored);
        assert!(m.s.password_ignored);
        assert_eq!(m.s.toasts, vec![PASSWORD_IGNORED_TEXT.to_owned()]);
        // Rejections are counted per member and reason.
        m.apply(RoomUi::Core(UiEvent::Members(vec![
            info(0, "H", true, false),
            info(3, "Mo", true, false),
        ])));
        for _ in 0..3 {
            m.apply(RoomUi::Rejected {
                member: Some(MemberId(3)),
                what: "chat message refused: RateLimited".into(),
            });
        }
        assert_eq!(m.s.diag.rejected.len(), 1);
        assert_eq!(m.s.diag.rejected[0].count, 3);
        assert_eq!(m.s.diag.rejected[0].who, "Mo");
        // Member list changes become events.
        m.apply(RoomUi::Core(UiEvent::Members(vec![
            info(0, "H", true, false),
            info(3, "Mo", false, true),
        ])));
        m.apply(RoomUi::Core(UiEvent::Members(vec![info(
            0, "H", true, false,
        )])));
        let texts: Vec<&str> = m.s.diag.events.iter().map(|e| e.text.as_str()).collect();
        assert!(texts.contains(&"Mo joined"));
        assert!(texts.contains(&"Mo dropped (seat kept 20 s)"));
        assert!(texts.contains(&"Mo left"));
    }
}

/// "2026-11-14 12:34:56 UTC" for a Unix time in ms (no date crate needed).
fn utc_text(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// The current room's chat as plain text (one line per message, "Now playing" dividers).
fn chat_text(s: &RoomStateDto) -> (String, usize) {
    use std::fmt::Write;
    let mut out = String::new();
    let mut n = 0;
    for c in &s.chat {
        match &c.divider {
            Some(d) => {
                let _ = writeln!(out, "--- {d} ---");
            }
            None => {
                n += 1;
                let host = if c.is_host { " (host)" } else { "" };
                let _ = writeln!(
                    out,
                    "[{}] {}{host}: {}",
                    utc_text(c.wall_ms),
                    c.name,
                    c.text
                );
            }
        }
    }
    (out, n)
}

/// Settings, Chat: export the CURRENT room's chat to a text file the user chose (nothing from
/// a room is kept by the app itself, ADR 0006). Returns the number of messages written.
#[tauri::command]
pub fn room_chat_export(m: State<'_, RoomManager>, path: String) -> Result<usize, String> {
    let s = m
        .state
        .lock()
        .map_err(|_| "Busy, try again.".to_string())?
        .clone();
    if !s.active {
        return Err("Join a room first: only the current room's chat can be exported.".into());
    }
    let (text, n) = chat_text(&s);
    std::fs::write(&path, text).map_err(|_| "Could not write the file.".to_string())?;
    Ok(n)
}

#[cfg(test)]
mod export_tests {
    use super::*;

    #[test]
    fn utc_text_and_chat_export_format() {
        assert_eq!(utc_text(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_text(1_794_653_505_775), "2026-11-14 10:51:45 UTC");
        assert_eq!(utc_text(951_782_400_000), "2000-02-29 00:00:00 UTC");
        let msg = |name: &str, text: &str, host: bool| ChatDto {
            id: 1,
            sender: 1,
            name: name.into(),
            is_host: host,
            text: text.into(),
            wall_ms: 0,
            media_time_ms: None,
            media_epoch: 0,
            divider: None,
        };
        let mut s = RoomStateDto::default();
        s.chat.push(ChatDto {
            divider: Some("Now playing: a.mkv".into()),
            ..msg("", "", false)
        });
        s.chat.push(msg("Ana", "hi", true));
        s.chat.push(msg("Bo", "hello", false));
        let (t, n) = chat_text(&s);
        assert_eq!(n, 2);
        assert_eq!(
            t,
            "--- Now playing: a.mkv ---\n[1970-01-01 00:00:00 UTC] Ana (host): hi\n[1970-01-01 00:00:00 UTC] Bo: hello\n"
        );
    }
}

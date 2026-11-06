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
use syncwatch_sync_core::{Clock, Event, LocalFileEvent, UiAction, UiEvent};
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
    can_chat: bool,
}

impl From<Permissions> for PermsDto {
    fn from(p: Permissions) -> Self {
        Self {
            can_play_pause: p.can_play_pause,
            can_seek: p.can_seek,
            can_change_media: p.can_change_media,
            can_change_speed: p.can_change_speed,
            can_chat: p.can_chat,
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
    can_chat: bool,
}

impl From<PermsIn> for Permissions {
    fn from(p: PermsIn) -> Self {
        Permissions {
            can_play_pause: p.can_play_pause,
            can_seek: p.can_seek,
            can_change_media: p.can_change_media,
            can_change_speed: p.can_change_speed,
            can_chat: p.can_chat,
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

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfigDto {
    enabled: bool,
    hidden_from_members: bool,
    slow_mode_secs: u16,
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
    chat_config: ChatConfigDto,
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
    reason: &'static str,
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
        ChatRejectReason::Disabled => "Chat is turned off in this room.",
        ChatRejectReason::NoPermission => "The host has not allowed you to chat.",
        ChatRejectReason::Muted => "The host muted you in chat.",
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
                                NotReadyReason::Buffering => "buffering",
                                NotReadyReason::Loading => "loading",
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
                self.toast(chat_reject_text(reason));
            }
            UiEvent::ChatDeleted(id) => self.s.chat.retain(|m| m.divider.is_some() || m.id != id.0),
            UiEvent::ChatCleared => self.s.chat.clear(),
            UiEvent::ChatConfig(c) => {
                self.s.chat_config = ChatConfigDto {
                    enabled: c.enabled,
                    hidden_from_members: c.hidden_from_members,
                    slow_mode_secs: c.slow_mode_secs,
                }
            }
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
}

pub struct RoomManager {
    key: IdentityKey,
    cmd: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
    transport: Mutex<Option<mpsc::UnboundedSender<TransportEvent>>>,
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
                if let Some(tx) = tx {
                    let _ = tx.send(ev);
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
    let cfg = SessionConfig {
        key: m.key.clone(),
        display_name: name,
        ice,
        max_members: limits::MAX_MEMBERS as u8,
    };
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (tr_tx, tr_rx) = mpsc::unbounded_channel();
    let (pl_tx, pl_rx) = mpsc::unbounded_channel();
    if let Ok(mut g) = m.cmd.lock() {
        *g = Some(cmd_tx);
    }
    if let Ok(mut g) = m.player_tx.lock() {
        *g = Some(pl_tx);
    }
    if let Ok(mut g) = m.transport.lock() {
        *g = Some(tr_tx);
    }
    // A new room starts completely clean: nothing loaded, nothing remembered.
    reset_room_scope(&*app.state::<AppState>().player, &m, None);
    let session_id = m.sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let chans = Chans {
            cmd: cmd_rx,
            tr: tr_rx,
            pl: pl_rx,
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
    pl: mpsc::UnboundedReceiver<syncwatch_player::PlayerEvent>,
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
            RoomSession::create(cfg, password, clock(epoch))
        }
        Mode::Join { room, password } => RoomSession::join(cfg, room, password, clock(epoch)),
    };
    let mut ended = exec(&mut rt, &session, outs).await;
    publish(&app, &mut rt, &session);
    let mut last_soak = Instant::now();
    let mut last_pos = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !ended {
        let input = tokio::select! {
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
                crate::roommedia::player_obs(&rt.media, &*rt.player, &ev)
                    .map(|o| Input::Core(Event::Player(o)))
            }),
            c = ch.cmd.recv() => match c {
                Some(Cmd::Action(a)) => Some(Input::Action(a)),
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
                None => Some(Input::Action(Action::Leave)),
            },
            _ = tick.tick() => Some(Input::Tick),
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
        publish(&app, &mut rt, &session);
        if last_soak.elapsed() >= Duration::from_secs(5) {
            last_soak = Instant::now();
            write_soak(&rt.mirror, epoch, false);
        }
    }
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
async fn after_step(rt: &mut Rt, session: &mut RoomSession, epoch: Instant) -> bool {
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
        E::StartTorrent { .. } | E::StopTorrent { .. } => {
            tracing::info!("torrent effects arrive in Milestone 7");
        }
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
        hidden_from_members: bool,
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
            hidden_from_members,
            slow_mode_secs,
        } => UiAction::SetChatConfig(ChatConfig {
            enabled,
            hidden_from_members,
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

    const ALL_VARIANTS: [&str; 21] = [
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
        mirror.apply(RoomUi::Core(UiEvent::Permissions(Permissions::CHAT_ONLY)));
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
        m.apply(RoomUi::Core(UiEvent::Permissions(Permissions::CHAT_ONLY)));
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

    #[test]
    fn own_permissions_follow_the_host_live() {
        let mut m = Mirror::default();
        m.apply(RoomUi::Core(UiEvent::Permissions(Permissions::ALL)));
        assert!(m.s.my_permissions.can_seek);
        let mut p = Permissions::ALL;
        p.can_seek = false;
        m.apply(RoomUi::Core(UiEvent::Permissions(p)));
        assert!(!m.s.my_permissions.can_seek && m.s.my_permissions.can_chat);
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

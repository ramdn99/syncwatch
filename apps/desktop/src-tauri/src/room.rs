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
use syncwatch_sync_core::{Clock, Event, LocalFileEvent, PlayerCmd, UiAction, UiEvent};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;

const UI_CHAT_KEEP: usize = 500;
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
}

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

    fn toast(&mut self, t: impl Into<String>) {
        self.s.toasts.push(t.into());
        if self.s.toasts.len() > 5 {
            self.s.toasts.remove(0);
        }
    }

    fn apply(&mut self, u: RoomUi) {
        match u {
            RoomUi::Created { room } | RoomUi::Joined { room } => self.s.room_code = Some(room),
            RoomUi::Phase(p) => {
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
                self.links.insert(key, state);
                self.rebuild_members();
            }
            RoomUi::Error(e) => self.toast(e),
            RoomUi::Core(ev) => self.apply_core(ev),
        }
    }

    fn apply_core(&mut self, ev: UiEvent) {
        match ev {
            UiEvent::Joined { me } => {
                self.s.me = Some(me.0);
                self.rebuild_members();
            }
            UiEvent::Members(v) => {
                self.infos = v;
                self.rebuild_members();
            }
            UiEvent::Permissions(p) => self.s.my_permissions = p.into(),
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
            UiEvent::ChatRejected { reason, .. } => self.toast(chat_reject_text(reason)),
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
                self.s.missing_file = Some(MissingFileDto {
                    name,
                    size,
                    duration_ms,
                    seq: self.panel_seq,
                });
            }
            UiEvent::FileVerified => self.s.file_status = Some("verified"),
            UiEvent::FileMismatch { .. } => self.s.file_status = Some("mismatch"),
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
                        divider: Some(format!("Now playing: {t}")),
                    });
                }
                self.s.media_title = title;
                self.s.file_status = None;
            }
            UiEvent::Notice(n) => self.toast(match n {
                Notice::MediaChangeSuperseded { .. } => {
                    "Someone else changed the source at the same time; their change won."
                }
                Notice::RequestDenied { .. } => "The host did not allow that.",
                Notice::MemberDeclinedMagnet { .. } => "A member declined the torrent.",
            }),
            UiEvent::BecameHost => self.toast("You are the host now."),
            UiEvent::HostChanged { .. } => self.toast("The host role moved to another member."),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------- manager -------

enum Cmd {
    Action(Action),
    Core(Event),
}

pub struct RoomManager {
    key: IdentityKey,
    cmd: Mutex<Option<mpsc::UnboundedSender<Cmd>>>,
    transport: Mutex<Option<mpsc::UnboundedSender<TransportEvent>>>,
    state: Mutex<RoomStateDto>,
    /// The local file this member chose (never sent anywhere).
    chosen: Mutex<Option<PathBuf>>,
}

impl RoomManager {
    pub fn new(paths: &AppPaths) -> Self {
        Self {
            key: load_identity(paths),
            cmd: Mutex::new(None),
            transport: Mutex::new(None),
            state: Mutex::new(RoomStateDto::default()),
            chosen: Mutex::new(None),
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

async fn start(app: AppHandle, name: String, mode: Mode) -> Result<(), String> {
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
    if let Ok(mut g) = m.cmd.lock() {
        *g = Some(cmd_tx);
    }
    if let Ok(mut g) = m.transport.lock() {
        *g = Some(tr_tx);
    }
    if let Ok(mut g) = m.chosen.lock() {
        *g = None;
    }
    if let Ok(mut g) = m.state.lock() {
        *g = RoomStateDto::default();
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        drive(app2.clone(), cfg, mode, sig, url, cmd_rx, tr_rx).await;
        let m = app2.state::<RoomManager>();
        if let Ok(mut g) = m.cmd.lock() {
            *g = None;
        }
        if let Ok(mut g) = m.transport.lock() {
            *g = None;
        }
    });
    Ok(())
}

async fn recv_sig(
    sig: &mut Option<SignalingConn>,
) -> Result<syncwatch_protocol::signaling::ServerMsg, ()> {
    match sig {
        Some(s) => s.recv().await.map_err(|_| ()),
        None => std::future::pending().await,
    }
}

async fn drive(
    app: AppHandle,
    cfg: SessionConfig,
    mode: Mode,
    sig: SignalingConn,
    url: String,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    mut tr_rx: mpsc::UnboundedReceiver<TransportEvent>,
) {
    let epoch = Instant::now();
    let bridge = Arc::clone(&*app.state::<Arc<Bridge>>());
    let mut sig = Some(sig);
    let mut reconnect_at: Option<Instant> = None;
    let mut backoff = Duration::from_secs(1);
    let mut mirror = Mirror::default();
    mirror.s.active = true;
    let (mut session, outs) = match mode {
        Mode::Create { password } => {
            mirror.s.has_password = password.is_some();
            RoomSession::create(cfg, password, clock(epoch))
        }
        Mode::Join { room, password } => RoomSession::join(cfg, room, password, clock(epoch)),
    };
    let mut ended = exec(&app, &bridge, &mut sig, &mut mirror, &session, outs).await;
    publish(&app, &mut mirror, &session);
    let mut last_soak = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !ended {
        let input = tokio::select! {
            m = recv_sig(&mut sig) => match m {
                Ok(m) => Some(Input::Signaling(m)),
                Err(()) => {
                    tracing::warn!("signaling connection lost; reconnecting");
                    sig = None;
                    reconnect_at = Some(Instant::now() + backoff);
                    None
                }
            },
            ev = tr_rx.recv() => ev.map(Input::Transport),
            c = cmd_rx.recv() => match c {
                Some(Cmd::Action(a)) => Some(Input::Action(a)),
                Some(Cmd::Core(e)) => Some(Input::Core(e)),
                None => Some(Input::Action(Action::Leave)),
            },
            _ = tick.tick() => Some(Input::Tick),
        };
        if sig.is_none() && reconnect_at.is_some_and(|t| Instant::now() >= t) {
            match SignalingConn::connect(&url).await {
                Ok(s) => {
                    tracing::info!("signaling reconnected");
                    sig = Some(s);
                    reconnect_at = None;
                    backoff = Duration::from_secs(1);
                    let outs = session.handle(clock(epoch), Input::SignalingReconnected);
                    ended |= exec(&app, &bridge, &mut sig, &mut mirror, &session, outs).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "signaling reconnect failed");
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                    reconnect_at = Some(Instant::now() + backoff);
                }
            }
        }
        if let Some(input) = input {
            let outs = session.handle(clock(epoch), input);
            ended |= exec(&app, &bridge, &mut sig, &mut mirror, &session, outs).await;
        }
        publish(&app, &mut mirror, &session);
        if last_soak.elapsed() >= Duration::from_secs(5) {
            last_soak = Instant::now();
            write_soak(&mirror, epoch, false);
        }
    }
    mirror.s.active = false;
    publish(&app, &mut mirror, &session);
    write_soak(&mirror, epoch, true);
    tracing::info!("room session ended");
}

/// Debug builds: SYNCWATCH_DEV_SOAK_FILE receives banner counters (scripts/dev/two-windows.ps1).
fn write_soak(mirror: &Mirror, epoch: Instant, done: bool) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Ok(path) = std::env::var("SYNCWATCH_DEV_SOAK_FILE") else {
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
fn publish(app: &AppHandle, mirror: &mut Mirror, session: &RoomSession) {
    mirror.s.is_host = session.is_host();
    if session.is_host() {
        mirror.s.me = Some(session.peer().local().me.0);
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
async fn exec(
    app: &AppHandle,
    bridge: &Arc<Bridge>,
    sig: &mut Option<SignalingConn>,
    mirror: &mut Mirror,
    session: &RoomSession,
    outs: Vec<Output>,
) -> bool {
    let mut ended = false;
    for o in outs {
        match o {
            Output::Signaling(m) => {
                if let Some(s) = sig.as_mut()
                    && s.send(&m).await.is_err()
                {
                    *sig = None;
                }
            }
            Output::CreatePeer { peer, role, ice } => {
                if let Err(e) = bridge.create_peer(peer, role, ice) {
                    tracing::warn!(error = %e, "could not create a peer connection");
                }
            }
            Output::RemoteSignal { peer, data } => {
                let _ = bridge.remote_signal(peer, data);
            }
            Output::Send { peer, lane, data } => {
                let _ = bridge.send(peer, lane, data);
            }
            Output::ClosePeer(peer) => bridge.close_peer(peer),
            Output::Player(cmd) => player_cmd(app, cmd),
            Output::Core(e) => {
                tracing::debug!(effect = ?std::mem::discriminant(&e), "effect for a later milestone")
            }
            Output::Ui(u) => mirror.apply(u),
            Output::Ended(r) => {
                tracing::info!(reason = ?r, "left the room");
                mirror.s.ended = Some(r);
                ended = true;
            }
        }
    }
    let _ = session;
    ended
}

/// Player commands from the room. Milestone 5 loads the chosen file; synchronized playback
/// control of the real player is Milestone 6.
fn player_cmd(app: &AppHandle, cmd: PlayerCmd) {
    match cmd {
        PlayerCmd::LoadLocal => {
            let path = app
                .state::<RoomManager>()
                .chosen
                .lock()
                .ok()
                .and_then(|g| g.clone());
            if let Some(p) = path {
                let st = app.state::<AppState>();
                if let Err(e) = st.player.load(&p.to_string_lossy()) {
                    tracing::warn!(error = %e, "could not load the chosen file");
                }
            }
        }
        other => {
            tracing::debug!(cmd = ?other, "player command (synchronized playback: Milestone 6)")
        }
    }
}

// ---------------------------------------------------------------------------- commands ------

#[tauri::command]
pub async fn room_create(
    app: AppHandle,
    name: String,
    password: Option<String>,
) -> Result<(), String> {
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
) -> Result<(), String> {
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
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
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
}

#[tauri::command]
pub fn room_action(m: State<'_, RoomManager>, action: RoomActionIn) -> Result<(), String> {
    let ui = match action {
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
    };
    m.send(Cmd::Action(Action::Ui(ui)))
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

//! The message enum. Topology is a host star: members talk only to the host.
//! Members send REQUESTS; the host validates and broadcasts one authoritative COMMAND.

use crate::ProtocolError;
use crate::ids::*;
use crate::limits::*;
use crate::media::MediaDescriptor;
use crate::permissions::Permissions;
use crate::tracks::TrackState;
use serde::{Deserialize, Serialize};

/// Authoritative room playback state, anchored at a host time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPlayback {
    pub media_epoch: MediaEpoch,
    pub epoch: Epoch,
    /// Position at `anchor_host_us`.
    pub position_ms: MediaMs,
    /// When this state takes effect (host clock). May be in the FUTURE for a scheduled play.
    pub anchor_host_us: HostMicros,
    pub paused: bool,
    pub speed: SpeedPermille,
}

impl RoomPlayback {
    pub fn initial(media_epoch: MediaEpoch, now: HostMicros) -> Self {
        Self {
            media_epoch,
            epoch: Epoch(0),
            position_ms: 0,
            anchor_host_us: now,
            paused: true,
            speed: 1000,
        }
    }

    /// Position at host time `t`. Before the anchor (a scheduled start) the position is held.
    pub fn position_at(&self, t: HostMicros) -> MediaMs {
        if self.paused || t <= self.anchor_host_us {
            return self.position_ms;
        }
        let elapsed_us = (t - self.anchor_host_us) as i128;
        let advanced_ms = elapsed_us * self.speed as i128 / 1_000_000;
        self.position_ms.saturating_add(advanced_ms as i64)
    }

    /// Whether the room is actually moving at host time `t`.
    pub fn is_playing_at(&self, t: HostMicros) -> bool {
        !self.paused && t >= self.anchor_host_us
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Play,
    Pause,
    Seek {
        position_ms: MediaMs,
    },
    SetSpeed {
        speed: SpeedPermille,
    },
    ChangeMedia {
        request_id: u32,
        descriptor: MediaDescriptor,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaStatus {
    NoMedia,
    Missing,
    Verifying,
    Ready,
    /// The member's file differs from the host's; they chose to proceed anyway.
    Mismatch,
    Downloading {
        permille: u16,
    },
    /// The member declined to download the host's magnet.
    Declined,
    /// Torrent engine not available (before Milestone 7) or failed.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackSyncStatus {
    InSync,
    Applying,
    Unresolved,
    Downloading { permille: u16 },
}

/// Member -> host status report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberStatus {
    pub media_epoch: MediaEpoch,
    pub media: MediaStatus,
    /// Highest playback epoch the member has fully applied (seeked and loaded).
    pub ready_epoch: Epoch,
    pub buffering: bool,
    pub tracks: TrackSyncStatus,
    pub rtt_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub id: MemberId,
    pub display_name: String,
    pub is_host: bool,
    pub permissions: Permissions,
    pub media: MediaStatus,
    pub buffering: bool,
    pub tracks: TrackSyncStatus,
    pub chat_muted: bool,
    /// False while a dropped member's seat is kept (shown as reconnecting).
    pub connected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Moderation {
    Delete(ChatId),
    Clear,
    Mute(MemberId),
    Unmute(MemberId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatConfig {
    pub enabled: bool,
    /// Members receive no chat (only their own echo); the host still sees everything.
    pub hidden_from_members: bool,
    /// 0 = off.
    pub slow_mode_secs: u16,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hidden_from_members: false,
            slow_mode_secs: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotReadyReason {
    MissingFile,
    Verifying,
    Downloading,
    Buffering,
    Loading,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInfo {
    pub active: bool,
    pub waiting: Vec<(MemberId, NotReadyReason)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenyReason {
    NoPermission,
    StaleMedia,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Notice {
    /// Your ChangeMedia request was replaced by a later one from `by`.
    MediaChangeSuperseded {
        request_id: u32,
        by: MemberId,
    },
    RequestDenied {
        reason: DenyReason,
    },
    MemberDeclinedMagnet {
        member: MemberId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatRejectReason {
    Disabled,
    NoPermission,
    Muted,
    Empty,
    TooLong,
    RateLimited,
    SlowMode,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: ChatId,
    pub seq: u64,
    pub host_wall_ms: i64,
    pub sender: MemberId,
    pub display_name: String,
    pub media_epoch: MediaEpoch,
    /// Room media position when sent; None when no media.
    pub media_time_ms: Option<MediaMs>,
    pub text: String,
    /// Sender's client id, so the sender can reconcile its optimistic copy.
    pub client_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectReason {
    Version,
    Full,
    Locked,
    Banned,
    BadPassword,
    TooManyAttempts,
}

/// Late-joiner / reconnect snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub your_id: MemberId,
    pub host_identity: Identity,
    pub descriptor: Option<MediaDescriptor>,
    pub playback: RoomPlayback,
    pub tracks: TrackState,
    pub members: Vec<MemberInfo>,
    pub your_permissions: Permissions,
    pub chat: Vec<ChatMessage>,
    pub chat_config: ChatConfig,
}

/// Everything that moves with the host role (spec section 9 "Host transfer").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostHandover {
    pub new_host: MemberId,
    pub new_host_identity: Identity,
    pub descriptor: Option<MediaDescriptor>,
    pub playback: RoomPlayback,
    pub tracks: TrackState,
    pub members: Vec<MemberInfo>,
    pub identities: Vec<(MemberId, Identity)>,
    pub default_permissions: Permissions,
    pub chat: Vec<ChatMessage>,
    pub chat_config: ChatConfig,
    pub chat_next_seq: u64,
    pub bans: Vec<Identity>,
    pub locked: bool,
    pub next_external_id: u32,
}

/// The room switches to new media (or to none). Everyone stops the old source completely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaChange {
    pub descriptor: Option<MediaDescriptor>,
    pub playback: RoomPlayback,
    pub tracks: TrackState,
    pub requested_by: MemberId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Playback(RoomPlayback),
    /// Boxed: by far the largest command.
    MediaChange(Box<MediaChange>),
    Tracks(TrackState),
    Permissions {
        member: MemberId,
        permissions: Permissions,
    },
    Members(Vec<MemberInfo>),
    ChatModeration(Moderation),
    ChatConfig(ChatConfig),
    ReadyGate(GateInfo),
    Notice(Notice),
    Kicked {
        banned: bool,
    },
    HostTransfer(Box<HostHandover>),
    RoomClosed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    // --- handshake (authentication details: Milestone 5) ---
    Hello {
        protocol: u16,
        display_name: String,
        identity: Identity,
    },
    Welcome(Box<Snapshot>),
    Reject {
        reason: RejectReason,
    },
    Bye,
    // --- clock sync and liveness (unreliable channel) ---
    TimePing {
        id: u32,
        t0_us: i64,
    },
    TimePong {
        id: u32,
        t0_us: i64,
        t1_host_us: HostMicros,
        t2_host_us: HostMicros,
    },
    Heartbeat {
        playback: RoomPlayback,
        host_now_us: HostMicros,
    },
    // --- member -> host ---
    Request(Request),
    Status(MemberStatus),
    ChatSend {
        client_id: u32,
        text: String,
        claimed_media_ms: Option<MediaMs>,
    },
    // --- host -> members ---
    Command(Command),
    Chat(ChatMessage),
    ChatRejected {
        client_id: u32,
        reason: ChatRejectReason,
    },
    // --- external subtitle/audio transfer (manifest entries ONLY) ---
    FileRequest {
        external_id: ExternalId,
        offset: u64,
    },
    FileChunk {
        external_id: ExternalId,
        offset: u64,
        data: Vec<u8>,
    },
    /// Member already holds an identical file (blake3 match); host skips the transfer.
    FileHave {
        external_id: ExternalId,
    },
    /// Member cancels/declines their own download.
    FileCancel {
        external_id: ExternalId,
    },
}

/// Data-channel kind a message travels on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Reliable,
    Unreliable,
}

/// Outbound priority class (control/chat/heartbeat before file chunks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Control = 0,
    Chat = 1,
    Heartbeat = 2,
    Bulk = 3,
}

/// Field-free mirror of [`Message`] for enumeration tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Hello,
    Welcome,
    Reject,
    Bye,
    TimePing,
    TimePong,
    Heartbeat,
    Request,
    Status,
    ChatSend,
    Command,
    Chat,
    ChatRejected,
    FileRequest,
    FileChunk,
    FileHave,
    FileCancel,
}

impl MessageKind {
    pub const ALL: [MessageKind; 17] = [
        MessageKind::Hello,
        MessageKind::Welcome,
        MessageKind::Reject,
        MessageKind::Bye,
        MessageKind::TimePing,
        MessageKind::TimePong,
        MessageKind::Heartbeat,
        MessageKind::Request,
        MessageKind::Status,
        MessageKind::ChatSend,
        MessageKind::Command,
        MessageKind::Chat,
        MessageKind::ChatRejected,
        MessageKind::FileRequest,
        MessageKind::FileChunk,
        MessageKind::FileHave,
        MessageKind::FileCancel,
    ];

    /// Whether this kind carries an opaque byte payload. Exhaustive on purpose: adding a message
    /// type forces a decision here, and the no-media test allows exactly one such kind
    /// (`FileChunk`, which only ever refers to a manifest ExternalId).
    pub fn carries_bulk_bytes(self) -> bool {
        match self {
            MessageKind::FileChunk => true,
            MessageKind::Hello
            | MessageKind::Welcome
            | MessageKind::Reject
            | MessageKind::Bye
            | MessageKind::TimePing
            | MessageKind::TimePong
            | MessageKind::Heartbeat
            | MessageKind::Request
            | MessageKind::Status
            | MessageKind::ChatSend
            | MessageKind::Command
            | MessageKind::Chat
            | MessageKind::ChatRejected
            | MessageKind::FileRequest
            | MessageKind::FileHave
            | MessageKind::FileCancel => false,
        }
    }
}

fn check_str(s: &str, max: usize, what: &'static str) -> Result<(), ProtocolError> {
    if s.len() > max {
        Err(ProtocolError::Limit(what))
    } else {
        Ok(())
    }
}

fn check_chat_message(m: &ChatMessage) -> Result<(), ProtocolError> {
    check_str(&m.display_name, MAX_NAME_BYTES, "chat display name")?;
    check_str(&m.text, MAX_CHAT_BYTES, "chat text")
}

fn check_members(v: &[MemberInfo]) -> Result<(), ProtocolError> {
    if v.len() > MAX_MEMBERS {
        return Err(ProtocolError::Limit("member list"));
    }
    v.iter()
        .try_for_each(|m| check_str(&m.display_name, MAX_NAME_BYTES, "member name"))
}

impl Message {
    pub fn kind(&self) -> MessageKind {
        match self {
            Message::Hello { .. } => MessageKind::Hello,
            Message::Welcome(_) => MessageKind::Welcome,
            Message::Reject { .. } => MessageKind::Reject,
            Message::Bye => MessageKind::Bye,
            Message::TimePing { .. } => MessageKind::TimePing,
            Message::TimePong { .. } => MessageKind::TimePong,
            Message::Heartbeat { .. } => MessageKind::Heartbeat,
            Message::Request(_) => MessageKind::Request,
            Message::Status(_) => MessageKind::Status,
            Message::ChatSend { .. } => MessageKind::ChatSend,
            Message::Command(_) => MessageKind::Command,
            Message::Chat(_) => MessageKind::Chat,
            Message::ChatRejected { .. } => MessageKind::ChatRejected,
            Message::FileRequest { .. } => MessageKind::FileRequest,
            Message::FileChunk { .. } => MessageKind::FileChunk,
            Message::FileHave { .. } => MessageKind::FileHave,
            Message::FileCancel { .. } => MessageKind::FileCancel,
        }
    }

    pub fn channel(&self) -> Channel {
        match self.kind() {
            MessageKind::TimePing | MessageKind::TimePong | MessageKind::Heartbeat => {
                Channel::Unreliable
            }
            _ => Channel::Reliable,
        }
    }

    pub fn priority(&self) -> Priority {
        match self.kind() {
            MessageKind::FileChunk => Priority::Bulk,
            MessageKind::TimePing | MessageKind::TimePong | MessageKind::Heartbeat => {
                Priority::Heartbeat
            }
            MessageKind::Chat | MessageKind::ChatSend | MessageKind::ChatRejected => Priority::Chat,
            _ => Priority::Control,
        }
    }

    /// Size limits on every field (untrusted input).
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Message::Hello { display_name, .. } => {
                check_str(display_name, MAX_NAME_BYTES, "hello name")
            }
            Message::Welcome(s) => {
                if let Some(d) = &s.descriptor {
                    d.validate()?;
                }
                s.tracks.validate()?;
                check_members(&s.members)?;
                if s.chat.len() > SNAPSHOT_CHAT {
                    return Err(ProtocolError::Limit("snapshot chat"));
                }
                s.chat.iter().try_for_each(check_chat_message)
            }
            Message::Request(Request::ChangeMedia { descriptor, .. }) => descriptor.validate(),
            Message::Request(_) => Ok(()),
            Message::ChatSend { text, .. } => check_str(text, MAX_CHAT_BYTES, "chat text"),
            Message::Chat(m) => check_chat_message(m),
            Message::FileChunk { data, .. } => {
                if data.len() > CHUNK_BYTES || data.is_empty() {
                    Err(ProtocolError::Limit("chunk size"))
                } else {
                    Ok(())
                }
            }
            Message::Command(c) => match c {
                Command::MediaChange(mc) => {
                    let MediaChange {
                        descriptor, tracks, ..
                    } = mc.as_ref();
                    if let Some(d) = descriptor {
                        d.validate()?;
                    }
                    tracks.validate()
                }
                Command::Tracks(t) => t.validate(),
                Command::Members(m) => check_members(m),
                Command::ReadyGate(g) => {
                    if g.waiting.len() > MAX_MEMBERS {
                        Err(ProtocolError::Limit("gate list"))
                    } else {
                        Ok(())
                    }
                }
                Command::HostTransfer(h) => {
                    if let Some(d) = &h.descriptor {
                        d.validate()?;
                    }
                    h.tracks.validate()?;
                    check_members(&h.members)?;
                    if h.chat.len() > HISTORY_CHAT
                        || h.bans.len() > 1024
                        || h.identities.len() > MAX_MEMBERS
                    {
                        return Err(ProtocolError::Limit("handover"));
                    }
                    h.chat.iter().try_for_each(check_chat_message)
                }
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }
}

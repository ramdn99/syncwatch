//! Inputs and outputs of the state machine.

use syncwatch_protocol::*;

/// Local time sources, supplied by the caller on every event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// Local monotonic time in microseconds.
    pub mono_us: i64,
    /// Local wall-clock time in ms since the Unix epoch (only used to stamp chat).
    pub wall_ms: i64,
}

/// Who sent a network message. Members only ever talk to the host (star topology).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sender {
    pub member: MemberId,
    pub identity: Identity,
}

/// A track as the LOCAL player sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTrack {
    /// The local player's track id (differs between members).
    pub id: i64,
    pub kind: TrackKind,
    pub ff_index: Option<i64>,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
    /// Set by the app when this track came from a room external file.
    pub external_id: Option<ExternalId>,
}

/// Observations from the local player.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerObs {
    FileLoaded {
        tracks: Vec<LocalTrack>,
        duration_ms: Option<i64>,
    },
    Tracks(Vec<LocalTrack>),
    /// Periodic position report.
    Position {
        position_ms: i64,
        paused: bool,
        buffering: bool,
    },
    /// A seek finished (mpv `playback-restart`).
    SeekDone,
    LoadFailed,
}

/// Commands to the local player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerCmd {
    /// Load the local file the user chose (the app knows the path; it is never sent anywhere).
    LoadLocal,
    /// Load the torrent loopback stream for this torrent (Milestone 7).
    LoadTorrentStream {
        infohash: Infohash,
        file_index: u32,
        /// Open directly at the room position (ADR 0012): the player reads the container
        /// header and index it needs, then the room's part, not the start of the file.
        start_ms: MediaMs,
    },
    Stop,
    SetPaused(bool),
    Seek(MediaMs),
    SetSpeed(SpeedPermille),
    /// `None` = off.
    SelectAudio(Option<i64>),
    SelectSub(Option<i64>),
    SubDelay(i32),
    AudioDelay(i32),
    /// Add a verified external file from the per-room cache (by manifest id).
    AddExternal {
        external_id: ExternalId,
        kind: ExternalKind,
    },
}

/// Results of the app's local-file handling for "Choose my file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalFileEvent {
    /// The user picked a file; hashing has started.
    Verifying,
    /// Hashing finished.
    Identified(FileIdentity),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TorrentEvent {
    Progress {
        phase: TorrentPhase,
        permille: u16,
    },
    /// Enough is downloaded to start streaming (head/tail + pre-buffer).
    Playable,
    /// Readiness at the ROOM position (ADR 0012): the read window plus the container pieces
    /// are on disk (`ready`), or what is missing. `None`: not known yet.
    Window(Option<WindowState>),
    /// Torrent engine not available (before Milestone 7) or failed.
    Unavailable,
}

/// This participant's read window at the room position (from the torrent engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub ready: bool,
    pub at_ms: MediaMs,
    pub missing: u64,
    pub permille: u16,
}

/// Local user actions.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Play,
    Pause,
    Seek(MediaMs),
    SetSpeed(SpeedPermille),
    /// Change the room's media. `own_file`: the requester already picked this local file.
    ChangeMedia {
        descriptor: MediaDescriptor,
        own_file: bool,
    },
    ProceedWithMismatch,
    AcceptMagnet,
    DeclineMagnet,
    SendChat {
        client_id: u32,
        text: String,
    },
    LocalMute {
        member: MemberId,
        muted: bool,
    },
    CancelExternalDownload(ExternalId),
    // --- host only ---
    SelectAudio(Selection),
    SelectSub(Selection),
    SetSubDelay(i32),
    SetAudioDelay(i32),
    /// The host picked, validated and hashed an external file.
    AddExternal {
        kind: ExternalKind,
        display_name: String,
        format: String,
        size: u64,
        blake3: [u8; 32],
    },
    SetPermissions {
        member: MemberId,
        permissions: Permissions,
    },
    SetDefaultPermissions(Permissions),
    ApplyPreset {
        preset: Preset,
        custom: Permissions,
    },
    Kick(MemberId),
    Ban(MemberId),
    Lock(bool),
    MakeHost(MemberId),
    CloseRoom,
    Moderate(Moderation),
    SetChatConfig(ChatConfig),
    SetWaitForEveryone(bool),
    /// Resume now, without the members currently blocking the ready gate.
    ResumeWithoutWaiting,
    // --- host succession (the host crashed; ADR 0007) ---
    /// Member: become the host of this room from what this member knows.
    TakeOverAsSuccessor,
    /// Member: the room will continue with a different host (accept its identity and clock).
    ExpectNewHost,
}

/// Everything that can happen to a peer.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Periodic timer (the app ticks at least every 50 ms and at `next_deadline`).
    Tick,
    Ui(UiAction),
    Net {
        from: Sender,
        msg: Message,
    },
    /// Host: a member passed the handshake.
    PeerJoined {
        member: MemberId,
        identity: Identity,
        display_name: String,
    },
    /// Host: a member's transport closed.
    PeerLeft {
        member: MemberId,
    },
    Player(PlayerObs),
    LocalFile(LocalFileEvent),
    Torrent(TorrentEvent),
    /// The app found an identical external file (blake3 match) in its cache.
    ExternalCached(ExternalId),
    /// The app has the room torrent's metadata (host: from its engine or a .torrent file).
    /// The host offers it to members; any participant keeps it for a later role change.
    TorrentMeta {
        infohash: Infohash,
        torrent: Vec<u8>,
        host_port: u16,
    },
}

/// Torrent metadata held for the room's announced torrent (moves with the host role).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentMetaStore {
    pub infohash: Infohash,
    pub torrent: Vec<u8>,
    pub host_port: u16,
}

/// UI notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    Joining,
    Joined {
        me: MemberId,
    },
    /// Show the Missing-file panel (once per media change).
    ShowMissingFilePanel {
        name: String,
        size: u64,
        duration_ms: Option<u64>,
    },
    FileVerified,
    FileMismatch {
        expected: FileIdentity,
        got: FileIdentity,
    },
    ConfirmMagnet {
        /// The host's chosen file (name and size).
        name: String,
        size: u64,
        infohash: Infohash,
        /// The torrent's own name (magnet "dn"), if known.
        torrent_name: Option<String>,
    },
    MediaChanged {
        title: Option<String>,
    },
    TrackUnresolved {
        kind: TrackKind,
    },
    ChatMessage(ChatMessage),
    ChatRejected {
        client_id: u32,
        reason: ChatRejectReason,
    },
    ChatDeleted(ChatId),
    ChatCleared,
    /// My chat state changed (or arrived with a snapshot): can I send, am I muted.
    ChatState(ChatState),
    Members(Vec<MemberInfo>),
    Permissions(Permissions),
    Gate(GateInfo),
    /// THIS window's own player is stalled on missing data (the buffering popup), with what
    /// it waits for when known (torrent window). Never about another member.
    Stall {
        stalled: bool,
        wait: Option<BufferWait>,
    },
    Notice(Notice),
    ExternalProgress {
        external_id: ExternalId,
        received: u64,
        size: u64,
    },
    ExternalFailed(ExternalId),
    /// The host did not send this external file to us (relayed connection, host choice).
    ExternalWithheld(ExternalId),
    Kicked {
        banned: bool,
    },
    RoomClosed,
    BecameHost,
    HostChanged {
        new_host: MemberId,
    },
}

/// Outputs.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Member -> host.
    SendToHost(Message),
    /// Host -> one member.
    SendTo(MemberId, Message),
    /// Host -> every connected member.
    Broadcast(Message),
    Player(PlayerCmd),
    Ui(UiEvent),
    /// Host: close this member's transport AFTER flushing messages already queued for it
    /// (so a Kicked/Reject notice still arrives).
    Disconnect(MemberId),
    StartTorrent {
        infohash: Infohash,
        file_index: u32,
    },
    /// The ROOM's position for the torrent engine (ADR 0012): it fetches that window first,
    /// also before the player opened the file or while it waits at the gate. `retarget`:
    /// room seek, media change, pause or resume.
    TorrentTarget {
        infohash: Infohash,
        position_ms: MediaMs,
        playing: bool,
        speed: SpeedPermille,
        retarget: bool,
    },
    /// Stop ALL traffic of this torrent (spec section 10).
    StopTorrent {
        infohash: Infohash,
    },
    /// Member: the host's metadata for the room torrent arrived complete (the app verifies
    /// its SHA-1 against `infohash` before using it).
    TorrentMetaReceived {
        infohash: Infohash,
        torrent: Vec<u8>,
        host_port: u16,
    },
    /// Member: persist a verified-in-order chunk of an external file to the per-room cache.
    WriteExternalChunk {
        external_id: ExternalId,
        offset: u64,
        data: Vec<u8>,
    },
    /// Member: the external file is complete and its blake3 matches the manifest.
    ExternalComplete {
        external_id: ExternalId,
    },
    /// Member: discard a partial external file.
    DiscardExternal {
        external_id: ExternalId,
    },
    /// Host: stream this external file to `to`, starting at `offset` (bulk priority).
    StartUpload {
        to: MemberId,
        external_id: ExternalId,
        offset: u64,
    },
    CancelUpload {
        to: MemberId,
        external_id: ExternalId,
    },
    /// Logged (without secrets or chat content) and counted.
    ProtocolViolation {
        from: MemberId,
        what: &'static str,
    },
}

/// Collects effects.
#[derive(Debug, Default)]
pub struct Out(pub Vec<Effect>);

impl Out {
    pub fn push(&mut self, e: Effect) {
        self.0.push(e);
    }
    pub fn player(&mut self, c: PlayerCmd) {
        self.0.push(Effect::Player(c));
    }
    pub fn ui(&mut self, u: UiEvent) {
        self.0.push(Effect::Ui(u));
    }
    pub fn into_vec(self) -> Vec<Effect> {
        self.0
    }
}

/// Timing constants (spec section 9).
pub mod timing {
    /// Heartbeat period (host -> members).
    pub const HEARTBEAT_US: i64 = 1_000_000;
    /// Clock-sync ping period after the initial burst.
    pub const PING_US: i64 = 1_000_000;
    /// Initial ping burst spacing.
    pub const PING_BURST_US: i64 = 100_000;
    pub const PING_BURST: u32 = 6;
    /// Minimum / maximum lead time for scheduled starts.
    pub const MIN_LEAD_US: i64 = 300_000;
    pub const MAX_LEAD_US: i64 = 1_500_000;
    /// Default ready-gate timeout.
    pub const GATE_TIMEOUT_US: i64 = 15_000_000;
    /// Two ChangeMedia requests within this window from different members are "concurrent".
    pub const CONCURRENT_CHANGE_US: i64 = 3_000_000;
    /// Chat timestamps claimed by members are honored within this distance of the room position.
    pub const CHAT_CLAIM_TOLERANCE_MS: i64 = 5_000;
    /// A member's position estimate older than this is not used for drift correction.
    pub const OBS_FRESH_US: i64 = 600_000;
    /// A dropped member keeps their seat this long (spec section 1).
    pub const SEAT_RETENTION_US: i64 = 20_000_000;
}

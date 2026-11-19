//! The room session (Milestone 5, ADR 0006): signaling + transport links + authentication +
//! the sync-core state machine, as one deterministic input -> output machine.
//!
//! The driver (the desktop app, or the in-process test harness) feeds [`Input`]s with the
//! current [`Clock`] and executes the returned [`Output`]s. No I/O, no threads, no timers here.
//!
//! * Host star: the host keeps one link per member; members keep one link to the host.
//! * Every link authenticates first ([`crate::auth`]); room messages on a link that has not
//!   finished authentication are dropped.
//! * App frames on a data channel start with a kind byte: [`FRAME_AUTH`] or [`FRAME_ROOM`].
//! * "Reconnecting" is shown only when traffic really stopped (the transport's `Interrupted`
//!   event) or the link is being renegotiated, and it clears itself on recovery.
//! * Host transfer over the network: the old host sends the room secret to the new host,
//!   broadcasts the sync-core handover, then tells signaling. Members reconnect to the new host
//!   (pinning its identity) once they have seen both the handover and the signaling change.

use crate::auth::*;
use crate::handshake::HandshakeMode;
use crate::invite::{self, Candidate, Invite, InviteError, Reply};
use crate::transport::*;
use std::collections::BTreeMap;
use syncwatch_protocol::auth::{AuthMsg, AuthReject, decode_auth, encode_auth};
use syncwatch_protocol::signaling::{ClientMsg, ServerMsg, SigError, SigPeer, SignalData};
use syncwatch_protocol::{self as proto, Channel, Identity, MemberId, Message, RejectReason};
use syncwatch_sync_core::{
    Clock, Effect, Event, HostConfig, Peer, PlayerCmd, Sender, UiAction, UiEvent,
};

pub const FRAME_AUTH: u8 = 0;
pub const FRAME_ROOM: u8 = 1;
/// Time given to queued messages (Kicked, Reject, RoomClosed) before a link is closed.
const FLUSH_US: i64 = 400_000;
/// An authentication that has not finished in this time is abandoned.
const AUTH_TIMEOUT_US: i64 = 30_000_000;
/// Waiting for the user to type a password.
const PASSWORD_TIMEOUT_US: i64 = 180_000_000;
/// Host: a member whose link died waits this long for a new offer before the link is dropped.
const REOFFER_WAIT_US: i64 = 20_000_000;
/// Member: reconnect to a new host after this even if only one of the two signals arrived.
const TRANSFER_WAIT_US: i64 = 8_000_000;
/// Host succession: after the host vanished (signaling and our link both gone), the member
/// ranked r among the connected members claims the room after (r + 1) times this.
const SUCCESSION_WAIT_US: i64 = 8_000_000;
/// Manual invites: candidate gathering that has not finished in this time is used as it is.
const GATHER_WAIT_US: i64 = 12_000_000;
/// Manual invites: a link silent this long (no ICE restart possible without signaling) ends.
const LOST_WAIT_US: i64 = 30_000_000;
/// Manual invites: at most this many open invites at a time.
const MAX_OPEN_INVITES: usize = 12;

#[derive(Clone)]
pub struct SessionConfig {
    pub key: IdentityKey,
    pub display_name: String,
    pub ice: IceConfig,
    pub max_members: u8,
    /// Host: the ready gate's timeout (Settings, Room defaults); `None` = the default.
    pub gate_timeout_us: Option<i64>,
    /// How the first offer and answer travel (ADR 0014).
    pub handshake: HandshakeMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Signaling(ServerMsg),
    /// The driver re-established the signaling connection after it dropped.
    SignalingReconnected,
    Transport(TransportEvent),
    Action(Action),
    /// Local events for sync-core (player, local file, torrent, cache). Not network events.
    Core(Event),
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Ui(UiAction),
    SubmitPassword(String),
    Leave,
    /// Host, manual invites: a new invite (a re-invite when bound to a member's seat).
    CreateInvite {
        for_member: Option<MemberId>,
    },
    /// Host: a pasted reply text.
    SubmitReply(String),
    /// Host: admit or refuse the member who replied to this invite (id in hex).
    Admit(String),
    Refuse(String),
    /// Host: forget an invite (closes its pending connection).
    RemoveInvite(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Signaling(ClientMsg),
    CreatePeer {
        peer: PeerId,
        role: IceRole,
        ice: IceConfig,
    },
    RemoteSignal {
        peer: PeerId,
        data: SignalData,
    },
    Send {
        peer: PeerId,
        lane: Lane,
        data: Vec<u8>,
    },
    ClosePeer(PeerId),
    Player(PlayerCmd),
    /// Other sync-core effects for the app (external files, torrents).
    Core(Effect),
    Ui(RoomUi),
    /// The session is over.
    Ended(EndReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkState {
    Connecting,
    Authenticating,
    Direct,
    Relayed,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Connecting,
    Authenticating,
    InRoom,
    /// Shown as the "Reconnecting..." banner (member side only).
    Reconnecting,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum EndReason {
    Left,
    RoomClosed,
    Kicked {
        banned: bool,
    },
    /// Authentication refused (too many attempts, version...).
    AuthRejected {
        reason: String,
    },
    /// The room refused us (locked, full, banned).
    Refused {
        reason: String,
    },
    NoSuchRoom,
    RoomFull,
    /// The host could not prove its identity or the handshake was tampered with.
    HostNotVerified,
    /// Manual invites: the link to the host is gone; a re-invite brings the member back.
    ConnectionLost {
        seat_keep_secs: u64,
    },
    /// Manual invites: the host did not admit this member before the invite expired.
    NotAdmitted,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RoomUi {
    Created {
        room: String,
    },
    Joined {
        room: String,
    },
    Phase(Phase),
    NeedPassword {
        wrong: bool,
        retry_after_ms: u32,
    },
    /// Verification words (rooms without a password). `member`: None = my link to the host.
    Verification {
        member: Option<MemberId>,
        words: VerificationWords,
    },
    /// Connection state of a link. `member`: None = my link to the host.
    Link {
        member: Option<MemberId>,
        state: LinkState,
    },
    Error(String),
    /// Something worth telling the user (not an error).
    Info(String),
    /// The member typed a password but the room has none (it was ignored). Once per join.
    PasswordIgnored,
    /// Host: something from member was refused (None: a joining link not yet a member).
    Rejected {
        member: Option<MemberId>,
        what: String,
    },
    /// Host, manual invites: the full list of invites (after every change).
    Invites(Vec<InviteView>),
    /// Member, manual invites: the reply to send back, and the words to compare.
    Reply {
        text: String,
        words: VerificationWords,
    },
    /// A pasted invite or reply was refused (friendly text pointing at the right step).
    InviteError(String),
    Core(UiEvent),
}

/// One invite as the host's UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteView {
    /// Hex id (used by Admit, Refuse, Remove).
    pub id: String,
    /// preparing, waiting, replied, admitted, joined, refused, expired, failed.
    pub status: &'static str,
    /// The invite text (once ready).
    pub text: Option<String>,
    pub expires_unix: u64,
    /// After a reply: the member's display name and the words to compare.
    pub member_name: Option<String>,
    pub words: Option<VerificationWords>,
    /// A re-invite: the seat it is bound to.
    pub for_member: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvState {
    Preparing,
    Waiting,
    Replied,
    Admitted,
    Joined,
    Refused,
    Expired,
    Failed,
}

impl InvState {
    fn text(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Waiting => "waiting",
            Self::Replied => "replied",
            Self::Admitted => "admitted",
            Self::Joined => "joined",
            Self::Refused => "refused",
            Self::Expired => "expired",
            Self::Failed => "failed",
        }
    }
}

/// Host side of one manual invite.
struct PendingInvite {
    link: PeerId,
    expires_unix: u64,
    for_member: Option<(MemberId, Identity)>,
    state: InvState,
    text: Option<String>,
    local_fp: Option<String>,
    reply: Option<Reply>,
    words: Option<VerificationWords>,
}

/// Local offer or answer collected until candidate gathering ends (manual invites).
struct Gather {
    sdp: Option<String>,
    cands: Vec<Candidate>,
    deadline: i64,
    done: bool,
}

enum LinkAuth {
    Pending,
    Host(Box<HostAuth>),
    Member(Box<MemberAuth>),
    Done,
}

struct Link {
    /// Host side: the member's signaling number (None once signaling forgot it).
    sig: Option<SigPeer>,
    open: bool,
    info: Option<PeerInfo>,
    auth: LinkAuth,
    auth_started: Option<i64>,
    need_password: bool,
    /// Host side: the seat after authentication. Member side: unused.
    member: Option<MemberId>,
    identity: Option<Identity>,
    interrupted: bool,
    close_at: Option<i64>,
    /// Host: drop the link unless the member re-offers before this.
    reoffer_deadline: Option<i64>,
    /// Manual invites: the local offer or answer being collected.
    gather: Option<Gather>,
    /// Manual invites: the remote DTLS fingerprint the invite or reply announced.
    expect_fp: Option<String>,
    /// Manual invites: the words compared before admission (shown instead of the auth words).
    manual_words: Option<VerificationWords>,
    /// Host, manual invites: the invite this link belongs to.
    invite_id: Option<[u8; 16]>,
    /// Manual invites: silent since (no ICE restart without signaling).
    lost_since: Option<i64>,
    /// Host: this member runs another protocol version and was told so once.
    version_rejected: bool,
}

impl Link {
    fn new(sig: Option<SigPeer>) -> Self {
        Self {
            sig,
            open: false,
            info: None,
            auth: LinkAuth::Pending,
            auth_started: None,
            need_password: false,
            member: None,
            identity: None,
            interrupted: false,
            close_at: None,
            reoffer_deadline: None,
            version_rejected: false,
            gather: None,
            expect_fp: None,
            manual_words: None,
            invite_id: None,
            lost_since: None,
        }
    }

    fn authed(&self) -> bool {
        matches!(self.auth, LinkAuth::Done) && self.open
    }

    fn steady_state(&self) -> LinkState {
        if self.info.as_ref().is_some_and(|i| i.relayed) {
            LinkState::Relayed
        } else {
            LinkState::Direct
        }
    }
}

struct TransferWait {
    p2p: bool,
    sig: bool,
    since: i64,
}

pub struct RoomSession {
    cfg: SessionConfig,
    peer: Peer,
    room: Option<String>,
    token: Option<String>,
    password: Option<String>,
    /// This session is the host as far as signaling knows.
    sig_host: bool,
    links: BTreeMap<PeerId, Link>,
    next_link: PeerId,
    /// Member side: the link to the host.
    host_link: Option<PeerId>,
    /// Member side: the host identity we expect (pinned after the first authentication).
    host_identity: Option<Identity>,
    throttle: AuthThrottle,
    /// New host: signals that arrived before the sync-core handover made us host.
    pending_signals: Vec<(SigPeer, SignalData)>,
    transfer: Option<TransferWait>,
    handover_password: Option<Option<String>>,
    joined_once: bool,
    phase: Phase,
    ending: Option<(i64, EndReason)>,
    ended: bool,
    /// Host: members whose link closed; sync-core gets PeerLeft for them.
    pending_left: Vec<MemberId>,
    /// Old host: the link and signaling number of the member receiving the host role.
    transfer_target: Option<(PeerId, SigPeer)>,
    /// Member: when signaling reported the host gone (host succession, ADR 0007).
    host_gone_at: Option<i64>,
    claimed: bool,
    /// Manual invites: the room's random id and display name.
    room_id: [u8; 16],
    room_name: String,
    /// Host, manual invites, by invite id.
    invites: BTreeMap<[u8; 16], PendingInvite>,
    /// Member, manual invites: the invite we answer.
    my_invite: Option<Invite>,
    /// Member, manual invites: end if the host has not admitted us by then.
    admit_deadline: Option<i64>,
}

impl RoomSession {
    /// Creates a room as its host.
    pub fn create(
        cfg: SessionConfig,
        password: Option<String>,
        clock: Clock,
    ) -> (Self, Vec<Output>) {
        let mut hc = HostConfig::new(cfg.key.public(), &cfg.display_name);
        hc.max_members = cfg.max_members as usize;
        if let Some(t) = cfg.gate_timeout_us {
            hc.gate_timeout_us = t;
        }
        // The app enforces the external-file ceilings itself (with the host's No-limit option).
        hc.max_external_sub = None;
        hc.max_external_audio = None;
        let peer = Peer::host(hc, clock.mono_us);
        let max = cfg.max_members;
        let s = Self::new(cfg, peer, password, true);
        (
            s,
            vec![
                Output::Ui(RoomUi::Phase(Phase::Connecting)),
                Output::Signaling(ClientMsg::Create { max_members: max }),
            ],
        )
    }

    /// Joins an existing room as a member.
    pub fn join(
        cfg: SessionConfig,
        room: String,
        password: Option<String>,
        _clock: Clock,
    ) -> (Self, Vec<Output>) {
        let peer = Peer::member(cfg.key.public(), &cfg.display_name);
        let mut s = Self::new(cfg, peer, password, false);
        s.room = Some(room.clone());
        (
            s,
            vec![
                Output::Ui(RoomUi::Phase(Phase::Connecting)),
                Output::Signaling(ClientMsg::Join { room }),
            ],
        )
    }

    /// Creates a room as its host, with manual invites (no signaling server, ADR 0014).
    pub fn create_manual(
        cfg: SessionConfig,
        password: Option<String>,
        room_name: &str,
        clock: Clock,
    ) -> (Self, Vec<Output>) {
        let mut hc = HostConfig::new(cfg.key.public(), &cfg.display_name);
        hc.max_members = cfg.max_members as usize;
        if let Some(t) = cfg.gate_timeout_us {
            hc.gate_timeout_us = t;
        }
        if let HandshakeMode::ManualInvite { seat_keep_us, .. } = cfg.handshake {
            hc.seat_keep_us = seat_keep_us;
        }
        hc.max_external_sub = None;
        hc.max_external_audio = None;
        let peer = Peer::host(hc, clock.mono_us);
        let mut s = Self::new(cfg, peer, password, true);
        s.room_id = invite::random16();
        s.room = Some(invite::hex16(&s.room_id));
        s.room_name = clean_name(room_name);
        s.joined_once = true;
        s.phase = Phase::InRoom;
        let out = vec![
            Output::Ui(RoomUi::Created {
                room: s.room_name.clone(),
            }),
            Output::Ui(RoomUi::Phase(Phase::InRoom)),
            Output::Ui(RoomUi::Invites(Vec::new())),
        ];
        (s, out)
    }

    /// Joins with a pasted invite (already decoded and checked by [`Invite::decode`]). The
    /// session prepares the reply; the connection starts once the host admits us.
    pub fn join_invite(
        cfg: SessionConfig,
        inv: Invite,
        password: Option<String>,
        clock: Clock,
    ) -> (Self, Vec<Output>) {
        let peer = Peer::member(cfg.key.public(), &cfg.display_name);
        let mut s = Self::new(cfg, peer, password, false);
        s.room_id = inv.room_id;
        s.room = Some(invite::hex16(&inv.room_id));
        s.room_name = clean_name(&inv.room_name);
        // The host must prove the identity the invite names (pinned like a reconnect).
        s.host_identity = Some(inv.host);
        let now = clock.mono_us;
        let mut out = vec![
            Output::Ui(RoomUi::Phase(Phase::Connecting)),
            Output::Ui(RoomUi::Joined {
                room: s.room_name.clone(),
            }),
        ];
        let id = s.alloc_link(None);
        s.host_link = Some(id);
        if let Some(l) = s.links.get_mut(&id) {
            l.expect_fp = inv.fingerprint();
            l.gather = Some(Gather {
                sdp: None,
                cands: Vec::new(),
                deadline: now + GATHER_WAIT_US,
                done: false,
            });
        }
        out.push(Output::CreatePeer {
            peer: id,
            role: IceRole::Answerer,
            ice: s.cfg.ice.clone(),
        });
        for data in inv.signals() {
            out.push(Output::RemoteSignal { peer: id, data });
        }
        let left_secs = inv.expires_unix.saturating_sub(unix(clock));
        s.admit_deadline = Some(now + (left_secs as i64 + 120) * 1_000_000);
        out.push(Output::Ui(RoomUi::Link {
            member: None,
            state: LinkState::Connecting,
        }));
        s.my_invite = Some(inv);
        (s, out)
    }

    /// The handshake is copy-paste invites (no signaling server).
    pub fn is_manual(&self) -> bool {
        self.cfg.handshake.is_manual()
    }

    /// The room's display name (manual invites) or its code (signaling).
    pub fn room_label(&self) -> String {
        if self.is_manual() {
            self.room_name.clone()
        } else {
            self.room.clone().unwrap_or_default()
        }
    }

    fn new(cfg: SessionConfig, peer: Peer, password: Option<String>, host: bool) -> Self {
        Self {
            cfg,
            peer,
            room: None,
            token: None,
            password: password.filter(|p| !p.is_empty()),
            sig_host: host,
            links: BTreeMap::new(),
            next_link: 1,
            host_link: None,
            host_identity: None,
            throttle: AuthThrottle::default(),
            pending_signals: Vec::new(),
            transfer: None,
            handover_password: None,
            joined_once: false,
            phase: Phase::Connecting,
            ending: None,
            ended: false,
            pending_left: Vec::new(),
            transfer_target: None,
            host_gone_at: None,
            claimed: false,
            room_id: [0; 16],
            room_name: String::new(),
            invites: BTreeMap::new(),
            my_invite: None,
            admit_deadline: None,
        }
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    pub fn is_host(&self) -> bool {
        self.peer.is_host()
    }

    pub fn room_code(&self) -> Option<&str> {
        self.room.as_deref()
    }

    pub fn has_password(&self) -> bool {
        self.password.is_some()
    }

    pub fn is_ended(&self) -> bool {
        self.ended
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn next_deadline(&self) -> Option<i64> {
        self.peer.next_deadline()
    }

    /// Member: the current link to the host (debug reconnect hook).
    pub fn host_link_peer(&self) -> Option<PeerId> {
        self.host_link
    }

    /// Host: the transport peer of an authenticated member (for bulk flow control).
    pub fn member_peer(&self, m: MemberId) -> Option<PeerId> {
        self.link_of_member(m)
    }

    /// Host: whether that member's selected path goes through a TURN relay.
    pub fn member_relayed(&self, m: MemberId) -> bool {
        self.link_of_member(m)
            .and_then(|id| self.links.get(&id))
            .and_then(|l| l.info.as_ref())
            .is_some_and(|i| i.relayed)
    }

    /// Host: one room message to one member (used to stream external-file chunks, which the
    /// driver paces by the bridge's bulk credit, not by a rate limiter).
    pub fn send_to_member(&self, m: MemberId, msg: &Message) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(id) = self.link_of_member(m) {
            self.send_room(id, msg, &mut out);
        }
        out
    }

    pub fn handle(&mut self, clock: Clock, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        if self.ended {
            return out;
        }
        match input {
            Input::Signaling(m) => self.on_signaling(clock, m, &mut out),
            Input::SignalingReconnected => self.on_signaling_reconnected(&mut out),
            Input::Transport(e) => self.on_transport(clock, e, &mut out),
            Input::Action(a) => self.on_action(clock, a, &mut out),
            Input::Core(ev) => self.core(clock, ev, &mut out),
            Input::Tick => self.on_tick(clock, &mut out),
        }
        // Departures are delivered in the same step as the close that caused them, so a quick
        // reconnect can never be overtaken by its own stale PeerLeft.
        self.flush_pending_left(clock, &mut out);
        out
    }

    // ------------------------------------------------------------------ helpers -----------

    fn host_auth_cfg(&self) -> HostAuthConfig {
        HostAuthConfig {
            room: self.room.clone().unwrap_or_default(),
            key: self.cfg.key.clone(),
            password: self.password.clone(),
        }
    }

    fn set_phase(&mut self, p: Phase, out: &mut Vec<Output>) {
        if self.phase != p {
            self.phase = p;
            out.push(Output::Ui(RoomUi::Phase(p)));
        }
    }

    fn link_ui(&self, id: PeerId, state: LinkState, out: &mut Vec<Output>) {
        let Some(l) = self.links.get(&id) else { return };
        let member = if Some(id) == self.host_link {
            None
        } else {
            match l.member {
                Some(m) => Some(m),
                None => return, // not yet a member: nothing to show
            }
        };
        out.push(Output::Ui(RoomUi::Link { member, state }));
    }

    fn alloc_link(&mut self, sig: Option<SigPeer>) -> PeerId {
        let id = self.next_link;
        self.next_link += 1;
        self.links.insert(id, Link::new(sig));
        id
    }

    fn send_auth(&self, peer: PeerId, msg: &AuthMsg, out: &mut Vec<Output>) {
        match encode_auth(msg) {
            Ok(b) => {
                let mut data = Vec::with_capacity(b.len() + 1);
                data.push(FRAME_AUTH);
                data.extend(b);
                out.push(Output::Send {
                    peer,
                    lane: Lane::Reliable,
                    data,
                });
            }
            Err(e) => tracing::warn!(error = %e, "could not encode an auth message"),
        }
    }

    fn send_room(&self, peer: PeerId, msg: &Message, out: &mut Vec<Output>) {
        match proto::encode(msg) {
            Ok(b) => {
                let mut data = Vec::with_capacity(b.len() + 1);
                data.push(FRAME_ROOM);
                data.extend(b);
                let lane = match (msg.channel(), msg.priority()) {
                    (_, proto::Priority::Bulk) => Lane::Bulk,
                    (Channel::Reliable, _) => Lane::Reliable,
                    (Channel::Unreliable, _) => Lane::Unreliable,
                };
                out.push(Output::Send { peer, lane, data });
            }
            Err(e) => tracing::warn!(error = %e, kind = ?msg.kind(), "could not encode a message"),
        }
    }

    fn close_link_now(&mut self, id: PeerId, out: &mut Vec<Output>) {
        if let Some(l) = self.links.remove(&id) {
            out.push(Output::ClosePeer(id));
            if Some(id) == self.host_link {
                self.host_link = None;
            }
            // Host: a member whose link is gone keeps the seat for 20 s (sync-core).
            // (delivered to sync-core by flush_pending_left with the current clock).
            if let (true, Some(m)) = (self.peer.is_host(), l.member)
                && !self.links.values().any(|o| o.member == Some(m))
            {
                self.pending_left.push(m);
            }
        }
    }

    fn end_soon(&mut self, now: i64, reason: EndReason) {
        if self.ending.is_none() {
            self.ending = Some((now + FLUSH_US, reason));
        }
    }

    fn end_now(&mut self, reason: EndReason, out: &mut Vec<Output>) {
        if self.ended {
            return;
        }
        if !self.is_manual() {
            out.push(Output::Signaling(ClientMsg::Leave));
        }
        let ids: Vec<PeerId> = self.links.keys().copied().collect();
        for id in ids {
            out.push(Output::ClosePeer(id));
        }
        self.links.clear();
        self.host_link = None;
        self.ended = true;
        out.push(Output::Ended(reason));
    }

    // ------------------------------------------------------------------ sync-core ---------

    fn core(&mut self, clock: Clock, ev: Event, out: &mut Vec<Output>) {
        let was_host = self.peer.is_host();
        let identity = self.cfg.key.public();
        let name = self.cfg.display_name.clone();
        let effects = self.peer.handle(clock, ev, identity, &name);
        self.apply_effects(clock, effects, out);
        if was_host && !self.peer.is_host() {
            self.after_handing_over(clock.mono_us, out);
        }
        self.flush_pending_left(clock, out);
    }

    fn flush_pending_left(&mut self, clock: Clock, out: &mut Vec<Output>) {
        while let Some(m) = self.pending_left.pop() {
            if self.peer.is_host() {
                self.core(clock, Event::PeerLeft { member: m }, out);
            }
        }
    }

    fn link_of_member(&self, m: MemberId) -> Option<PeerId> {
        self.links
            .iter()
            .find(|(_, l)| l.member == Some(m) && l.authed())
            .map(|(id, _)| *id)
    }

    fn apply_effects(&mut self, clock: Clock, effects: Vec<Effect>, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        for e in effects {
            match e {
                Effect::SendToHost(msg) => {
                    if let Some(h) = self
                        .host_link
                        .filter(|h| self.links.get(h).is_some_and(Link::authed))
                    {
                        self.send_room(h, &msg, out);
                    }
                }
                Effect::SendTo(m, msg) => {
                    // Rejections the host sends are recorded for the diagnostics view.
                    let rejected = match &msg {
                        Message::ChatRejected { reason, .. } => {
                            Some(format!("chat message refused: {reason:?}"))
                        }
                        Message::Command(proto::Command::Notice(
                            proto::Notice::RequestDenied { reason },
                        )) => Some(format!("request refused: {reason:?}")),
                        Message::Reject { reason } => Some(format!("join refused: {reason:?}")),
                        _ => None,
                    };
                    if let Some(what) = rejected {
                        out.push(Output::Ui(RoomUi::Rejected {
                            member: Some(m),
                            what,
                        }));
                    }
                    if let Some(id) = self.link_of_member(m) {
                        self.send_room(id, &msg, out);
                    }
                }
                Effect::Broadcast(msg) => {
                    let ids: Vec<PeerId> = self
                        .links
                        .iter()
                        .filter(|(id, l)| {
                            l.authed() && l.member.is_some() && Some(**id) != self.host_link
                        })
                        .map(|(id, _)| *id)
                        .collect();
                    for id in ids {
                        self.send_room(id, &msg, out);
                    }
                }
                Effect::Disconnect(m) => {
                    for l in self.links.values_mut().filter(|l| l.member == Some(m)) {
                        l.close_at.get_or_insert(now + FLUSH_US);
                    }
                }
                Effect::Player(c) => out.push(Output::Player(c)),
                Effect::ProtocolViolation { from, what } => {
                    tracing::warn!(member = from.0, what, "protocol violation");
                    out.push(Output::Ui(RoomUi::Rejected {
                        member: Some(from),
                        what: format!("protocol violation: {what}"),
                    }));
                }
                Effect::Ui(ev) => {
                    match &ev {
                        UiEvent::Kicked { banned } if !self.peer.is_host() => {
                            self.end_soon(now, EndReason::Kicked { banned: *banned });
                        }
                        UiEvent::RoomClosed => {
                            if self.peer.is_host() {
                                self.end_soon(now, EndReason::Left);
                            } else {
                                self.end_soon(now, EndReason::RoomClosed);
                            }
                        }
                        UiEvent::BecameHost => {
                            out.push(Output::Ui(RoomUi::Core(ev)));
                            self.after_becoming_host(clock, out);
                            continue;
                        }
                        UiEvent::HostChanged { .. } if !self.peer.is_host() => {
                            let w = self.transfer.get_or_insert(TransferWait {
                                p2p: false,
                                sig: false,
                                since: now,
                            });
                            w.p2p = true;
                            if let Peer::Member(m) = &self.peer {
                                self.host_identity = m.host_identity;
                            }
                        }
                        _ => {}
                    }
                    out.push(Output::Ui(RoomUi::Core(ev)));
                }
                other => out.push(Output::Core(other)),
            }
        }
        self.maybe_reconnect_after_transfer(now, out);
    }

    // ------------------------------------------------------------------ host transfer -----

    /// Old host, right after the sync-core handover was broadcast.
    fn after_handing_over(&mut self, now: i64, out: &mut Vec<Output>) {
        let target = self.transfer_target.take();
        let Some((link_id, sig)) = target else {
            tracing::warn!("host transfer without a target link");
            return;
        };
        if let Some(l) = self.links.get_mut(&link_id) {
            self.host_identity = l.identity;
            l.member = None;
        }
        self.host_link = Some(link_id);
        for (id, l) in self.links.iter_mut() {
            if *id != link_id {
                l.close_at.get_or_insert(now + FLUSH_US);
            }
        }
        out.push(Output::Signaling(ClientMsg::TransferHost { to: sig }));
        tracing::info!("host role handed over");
    }

    /// New host, right after the handover made our sync-core the host.
    fn after_becoming_host(&mut self, clock: Clock, out: &mut Vec<Output>) {
        if let Some(pw) = self.handover_password.take() {
            self.password = pw;
        }
        let old_host = self.host_identity;
        let old_link = self.host_link.take();
        if let (Peer::Host(h), Some(id), Some(ident)) = (&self.peer, old_link, old_host) {
            let seat = h.seat_of(&ident);
            if let Some(l) = self.links.get_mut(&id) {
                l.member = seat;
                l.identity = Some(ident);
            }
        }
        // Everyone else must reconnect to us: their seats are kept for 20 s meanwhile.
        if let Peer::Host(h) = &self.peer {
            let linked: Vec<MemberId> = self.links.values().filter_map(|l| l.member).collect();
            for m in h.connected_members() {
                if !linked.contains(&m) {
                    self.pending_left.push(m);
                }
            }
        }
        self.transfer = None;
        self.set_phase(Phase::InRoom, out);
        if self.sig_host {
            self.flush_pending_signals(clock, out);
        }
        tracing::info!("became the room host");
    }

    fn flush_pending_signals(&mut self, clock: Clock, out: &mut Vec<Output>) {
        let pending = std::mem::take(&mut self.pending_signals);
        for (p, d) in pending {
            self.on_host_signal(clock, p, d, out);
        }
    }

    fn maybe_reconnect_after_transfer(&mut self, now: i64, out: &mut Vec<Output>) {
        let Some(w) = &self.transfer else { return };
        if self.peer.is_host() {
            self.transfer = None;
            return;
        }
        if (w.p2p && w.sig) || now - w.since >= TRANSFER_WAIT_US {
            self.transfer = None;
            tracing::info!("reconnecting to the new host");
            self.connect_to_host(now, out);
        }
    }

    /// Host succession: claim the room when the host is gone for long enough, in rank order.
    fn maybe_claim_host(&mut self, now: i64, out: &mut Vec<Output>) {
        let Some(gone) = self.host_gone_at else {
            return;
        };
        if self.claimed || self.peer.is_host() || self.ending.is_some() {
            return;
        }
        // If our link to the host is still fine, the host only lost signaling: keep waiting.
        let link_ok = self
            .host_link
            .and_then(|h| self.links.get(&h))
            .is_some_and(|l| l.authed() && !l.interrupted);
        if link_ok {
            return;
        }
        let Peer::Member(m) = &self.peer else { return };
        let Some(me) = m.me else { return };
        let mut ranked: Vec<MemberId> = m
            .members
            .iter()
            .filter(|i| !i.is_host && i.connected)
            .map(|i| i.id)
            .collect();
        ranked.sort();
        let Some(rank) = ranked.iter().position(|id| *id == me) else {
            return;
        };
        if now - gone >= SUCCESSION_WAIT_US * (rank as i64 + 1) {
            self.claimed = true;
            tracing::info!(rank, "host succession: claiming the room");
            out.push(Output::Signaling(ClientMsg::ClaimHost));
        }
    }

    // ------------------------------------------------------------------ signaling ---------

    fn on_signaling_reconnected(&mut self, out: &mut Vec<Output>) {
        let Some(room) = self.room.clone() else {
            return;
        };
        if self.sig_host {
            if let Some(token) = self.token.clone() {
                out.push(Output::Signaling(ClientMsg::Resume { room, token }));
            }
        } else {
            out.push(Output::Signaling(ClientMsg::Join { room }));
        }
    }

    fn on_signaling(&mut self, clock: Clock, m: ServerMsg, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        match m {
            ServerMsg::Created { room, token } => {
                self.room = Some(room.clone());
                self.token = Some(token);
                self.joined_once = true;
                out.push(Output::Ui(RoomUi::Created { room }));
                self.set_phase(Phase::InRoom, out);
            }
            ServerMsg::Joined { room, .. } => {
                self.room = Some(room.clone());
                if !self.joined_once {
                    out.push(Output::Ui(RoomUi::Joined { room }));
                }
                let alive = self
                    .host_link
                    .and_then(|h| self.links.get(&h))
                    .is_some_and(|l| l.open && !l.interrupted);
                if !alive && !self.peer.is_host() {
                    self.connect_to_host(now, out);
                }
            }
            ServerMsg::BecameHost { token, .. }
                if self.host_gone_at.is_some() && !self.peer.is_host() =>
            {
                // Host succession: our claim won. Continue the room as its host.
                self.token = Some(token);
                self.sig_host = true;
                self.host_gone_at = None;
                if let Some(old) = self.host_link.take() {
                    self.links.remove(&old);
                    out.push(Output::ClosePeer(old));
                }
                self.host_identity = None;
                tracing::info!("host succession: taking over the room");
                self.core(clock, Event::Ui(UiAction::TakeOverAsSuccessor), out);
                out.push(Output::Ui(RoomUi::Info(
                    "The host left. You are the host now.".into(),
                )));
            }
            ServerMsg::BecameHost { token, .. } => {
                self.token = Some(token);
                self.sig_host = true;
                if self.peer.is_host() {
                    self.flush_pending_signals(clock, out);
                }
            }
            ServerMsg::HostChanged { you: None }
                if self.host_gone_at.is_some() && !self.peer.is_host() =>
            {
                // Host succession by another member: reconnect to it (its identity is new).
                self.host_gone_at = None;
                self.claimed = false;
                self.transfer = None;
                self.host_identity = None;
                self.core(clock, Event::Ui(UiAction::ExpectNewHost), out);
                out.push(Output::Ui(RoomUi::Info(
                    "The host left. The room continues with a new host.".into(),
                )));
                tracing::info!("host succession: reconnecting to the new host");
                self.connect_to_host(now, out);
            }
            ServerMsg::HostChanged { you } => {
                if you.is_some() {
                    self.sig_host = false;
                } else if !self.peer.is_host() {
                    let w = self.transfer.get_or_insert(TransferWait {
                        p2p: false,
                        sig: false,
                        since: now,
                    });
                    w.sig = true;
                    self.maybe_reconnect_after_transfer(now, out);
                }
            }
            ServerMsg::PeerLeft { peer } if self.sig_host => {
                let ids: Vec<PeerId> = self
                    .links
                    .iter()
                    .filter(|(_, l)| l.sig == Some(peer))
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    let keep = self
                        .links
                        .get(&id)
                        .is_some_and(|l| l.authed() && !l.interrupted);
                    if keep {
                        // Signaling dropped but the data channel is fine: keep the link.
                        if let Some(l) = self.links.get_mut(&id) {
                            l.sig = None;
                        }
                    } else {
                        self.close_link_now(id, out);
                    }
                }
            }
            ServerMsg::Signal { from, data } => {
                if self.sig_host {
                    let Some(p) = from else { return };
                    if self.peer.is_host() {
                        self.on_host_signal(clock, p, data, out);
                    } else if self.pending_signals.len() < 256 {
                        self.pending_signals.push((p, data));
                    }
                } else if from.is_none()
                    && let Some(h) = self.host_link
                {
                    out.push(Output::RemoteSignal { peer: h, data });
                }
            }
            ServerMsg::RoomClosed => {
                if !self.peer.is_host() {
                    self.end_soon(now, EndReason::RoomClosed);
                }
            }
            ServerMsg::HostGone { .. } if !self.peer.is_host() => {
                tracing::info!("signaling: the host is gone; waiting for it to come back");
                self.host_gone_at.get_or_insert(now);
            }
            ServerMsg::HostBack => {
                self.host_gone_at = None;
                self.claimed = false;
            }
            ServerMsg::Error {
                code: SigError::BadToken,
            } if self.sig_host => {
                // We lost the room while away (a member took over after the grace period).
                self.end_now(
                    EndReason::Error {
                        message: "The room continued with a new host while you were disconnected."
                            .into(),
                    },
                    out,
                );
            }
            ServerMsg::Error { code } => {
                tracing::warn!(?code, "signaling error");
                if !self.joined_once {
                    let reason = match code {
                        SigError::NoSuchRoom => EndReason::NoSuchRoom,
                        SigError::RoomFull => EndReason::RoomFull,
                        other => EndReason::Error {
                            message: format!("signaling: {other:?}"),
                        },
                    };
                    if matches!(
                        code,
                        SigError::NoSuchRoom | SigError::RoomFull | SigError::ServerBusy
                    ) {
                        self.end_now(reason, out);
                    }
                }
            }
            ServerMsg::Welcome { .. }
            | ServerMsg::Resumed { .. }
            | ServerMsg::PeerJoined { .. }
            | ServerMsg::PeerLeft { .. }
            | ServerMsg::HostGone { .. }
            | ServerMsg::Pong => {}
        }
    }

    /// Host: SDP or a candidate from member `p`.
    fn on_host_signal(
        &mut self,
        clock: Clock,
        p: SigPeer,
        data: SignalData,
        out: &mut Vec<Output>,
    ) {
        let existing = self
            .links
            .iter()
            .find(|(_, l)| l.sig == Some(p))
            .map(|(id, _)| *id);
        let id = match (&data, existing) {
            (SignalData::Offer { .. }, Some(id)) => {
                // A new offer on a known link = the member restarted it: fresh connection.
                let old_member = self.links.get(&id).and_then(|l| l.member);
                if let Some(l) = self.links.get_mut(&id) {
                    *l = Link::new(Some(p));
                }
                if let Some(m) = old_member {
                    self.core(clock, Event::PeerLeft { member: m }, out);
                }
                out.push(Output::CreatePeer {
                    peer: id,
                    role: IceRole::Answerer,
                    ice: self.cfg.ice.clone(),
                });
                id
            }
            (SignalData::Offer { .. }, None) => {
                if self.links.len() >= 64 {
                    return;
                }
                let id = self.alloc_link(Some(p));
                out.push(Output::CreatePeer {
                    peer: id,
                    role: IceRole::Answerer,
                    ice: self.cfg.ice.clone(),
                });
                id
            }
            (_, Some(id)) => id,
            (_, None) => return,
        };
        out.push(Output::RemoteSignal { peer: id, data });
    }

    /// Member: open (or reopen) the link to the host.
    fn connect_to_host(&mut self, now: i64, out: &mut Vec<Output>) {
        if self.is_manual() {
            // No signaling: a new connection needs a re-invite from the host.
            self.lost_host(out);
            return;
        }
        if let Some(old) = self.host_link.take() {
            self.links.remove(&old);
            out.push(Output::ClosePeer(old));
        }
        let id = self.alloc_link(None);
        self.host_link = Some(id);
        if let Some(l) = self.links.get_mut(&id) {
            l.auth_started = Some(now);
        }
        out.push(Output::CreatePeer {
            peer: id,
            role: IceRole::Offerer,
            ice: self.cfg.ice.clone(),
        });
        out.push(Output::Ui(RoomUi::Link {
            member: None,
            state: if self.joined_once {
                LinkState::Reconnecting
            } else {
                LinkState::Connecting
            },
        }));
        if self.joined_once {
            self.set_phase(Phase::Reconnecting, out);
        }
    }

    // ------------------------------------------------------------------ transport ---------

    fn on_transport(&mut self, clock: Clock, e: TransportEvent, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        match e {
            TransportEvent::LocalSignal { peer, data } => {
                if self.is_manual() {
                    self.manual_local_signal(peer, data, out);
                    return;
                }
                let Some(l) = self.links.get(&peer) else {
                    return;
                };
                if Some(peer) == self.host_link {
                    out.push(Output::Signaling(ClientMsg::Signal { to: None, data }));
                } else if let Some(sig) = l.sig {
                    out.push(Output::Signaling(ClientMsg::Signal {
                        to: Some(sig),
                        data,
                    }));
                }
            }
            TransportEvent::Open { peer, info } => self.on_open(now, peer, info, out),
            TransportEvent::Frame { peer, data, .. } => self.on_frame(clock, peer, data, out),
            TransportEvent::Interrupted { peer, diag } => {
                let Some(l) = self.links.get_mut(&peer) else {
                    return;
                };
                if l.interrupted {
                    return;
                }
                l.interrupted = true;
                if self.cfg.handshake.is_manual() {
                    l.lost_since.get_or_insert(now);
                }
                tracing::warn!(
                    link = peer,
                    silence_ms = diag.silence_ms,
                    frames_rx = diag.frames_rx,
                    ice = %diag.ice_state,
                    conn = %diag.connection_state,
                    cause = diag.cause,
                    "link interrupted"
                );
                self.link_ui(peer, LinkState::Reconnecting, out);
                if Some(peer) == self.host_link {
                    self.set_phase(Phase::Reconnecting, out);
                }
            }
            TransportEvent::Recovered { peer } => {
                let Some(l) = self.links.get_mut(&peer) else {
                    return;
                };
                if !l.interrupted {
                    return;
                }
                l.interrupted = false;
                l.lost_since = None;
                let st = l.steady_state();
                let authed = l.authed();
                self.link_ui(peer, st, out);
                if Some(peer) == self.host_link && authed {
                    self.set_phase(Phase::InRoom, out);
                }
            }
            TransportEvent::RestartNeeded { peer, diag } => {
                if self.is_manual() {
                    // No ICE restart without signaling: give the link time to come back.
                    if let Some(l) = self.links.get_mut(&peer) {
                        l.lost_since.get_or_insert(now);
                    }
                    tracing::warn!(
                        link = peer,
                        silence_ms = diag.silence_ms,
                        "silent link (invites: no restart)"
                    );
                    return;
                }
                tracing::warn!(
                    link = peer,
                    silence_ms = diag.silence_ms,
                    "restarting a silent link"
                );
                if Some(peer) == self.host_link {
                    self.connect_to_host(now, out);
                } else if let Some(l) = self.links.get_mut(&peer) {
                    if l.sig.is_none() {
                        self.close_link_now(peer, out);
                    } else {
                        l.reoffer_deadline.get_or_insert(now + REOFFER_WAIT_US);
                    }
                }
            }
            TransportEvent::Closed { peer, reason } => {
                if !self.links.contains_key(&peer) {
                    return;
                }
                tracing::info!(link = peer, %reason, "link closed");
                self.invite_link_closed(peer, out);
                let was_host_link = Some(peer) == self.host_link;
                self.close_link_now(peer, out);
                if was_host_link
                    && self.ending.is_none()
                    && self.transfer.is_none()
                    && !self.peer.is_host()
                {
                    self.connect_to_host(now, out);
                }
            }
            TransportEvent::BridgeReloaded => {
                if !self.peer.is_host() && self.host_link.is_none() && self.ending.is_none() {
                    self.connect_to_host(now, out);
                }
            }
        }
    }

    fn on_open(&mut self, now: i64, peer: PeerId, info: PeerInfo, out: &mut Vec<Output>) {
        let is_host_link = Some(peer) == self.host_link;
        let (Some(local), Some(remote)) = (
            info.local_fingerprint.clone(),
            info.remote_fingerprint.clone(),
        ) else {
            tracing::warn!(link = peer, "no DTLS fingerprints; closing the link");
            self.close_link_now(peer, out);
            return;
        };
        let host_cfg = self.host_auth_cfg();
        let Some(l) = self.links.get_mut(&peer) else {
            return;
        };
        // Manual invites: the connection must use the certificate the invite or reply named
        // (the one the words were made from).
        if let Some(want) = l.expect_fp.as_deref()
            && !want.eq_ignore_ascii_case(&remote)
        {
            tracing::warn!(
                link = peer,
                "the connection does not match the invite; closing"
            );
            out.push(Output::Ui(RoomUi::Error(
                "The connection did not match the invite. Ask for a new invite.".into(),
            )));
            self.close_link_now(peer, out);
            if is_host_link {
                self.end_now(EndReason::HostNotVerified, out);
            }
            return;
        }
        l.lost_since = None;
        l.open = true;
        l.info = Some(info);
        l.interrupted = false;
        l.auth_started = Some(now);
        if is_host_link {
            let fps = Fingerprints {
                member: local,
                host: remote,
            };
            l.auth = LinkAuth::Member(Box::new(MemberAuth::new(
                self.cfg.key.clone(),
                &self.cfg.display_name,
                self.room.as_deref().unwrap_or_default(),
                self.password.clone(),
                fps,
                self.host_identity,
            )));
            out.push(Output::Ui(RoomUi::Link {
                member: None,
                state: LinkState::Authenticating,
            }));
            if !self.joined_once {
                self.set_phase(Phase::Authenticating, out);
            }
        } else if self.peer.is_host() {
            let fps = Fingerprints {
                member: remote,
                host: local,
            };
            let (h, hello) = HostAuth::start(&host_cfg, fps, random_nonce());
            l.auth = LinkAuth::Host(Box::new(h));
            self.send_auth(peer, &hello, out);
        } else {
            // Not our host link and we are not the host: a stale link.
            self.close_link_now(peer, out);
        }
    }

    fn on_frame(&mut self, clock: Clock, peer: PeerId, data: Vec<u8>, out: &mut Vec<Output>) {
        let Some((&kind, body)) = data.split_first() else {
            return;
        };
        match kind {
            FRAME_AUTH => match decode_auth(body) {
                Ok(m) => self.on_auth(clock, peer, m, out),
                Err(e) => {
                    tracing::warn!(link = peer, error = %e, "bad auth frame; closing the link");
                    self.close_link_now(peer, out);
                }
            },
            FRAME_ROOM => {
                let Some(l) = self.links.get(&peer) else {
                    return;
                };
                if !l.authed() {
                    tracing::warn!(link = peer, "room message before authentication dropped");
                    return;
                }
                let msg = match proto::decode(body) {
                    Ok(m) => m,
                    // Another SyncWatch version: the envelope version differs, so neither side
                    // can read the other's Hello or Reject. End with a clear reason instead of
                    // waiting forever.
                    Err(proto::ProtocolError::Version(v)) => {
                        let to_host = Some(peer) == self.host_link;
                        tracing::warn!(
                            link = peer,
                            theirs = v,
                            ours = proto::PROTOCOL_VERSION,
                            "peer runs another SyncWatch version"
                        );
                        if to_host {
                            self.end_soon(
                                clock.mono_us,
                                EndReason::Refused {
                                    reason: "version".to_owned(),
                                },
                            );
                        } else if self.peer.is_host()
                            && let Some(l) = self.links.get_mut(&peer)
                            && !l.version_rejected
                        {
                            // In our version: the member cannot decode it, and its own version
                            // check then ends its join with the same reason.
                            l.version_rejected = true;
                            let reject = Message::Reject {
                                reason: RejectReason::Version,
                            };
                            self.send_room(peer, &reject, out);
                        }
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(link = peer, error = %e, "malformed room message dropped");
                        return;
                    }
                };
                let from = if Some(peer) == self.host_link {
                    if let Message::Reject { reason } = &msg {
                        let r = match reason {
                            RejectReason::Banned => "banned",
                            RejectReason::Locked => "locked",
                            RejectReason::Full => "full",
                            RejectReason::Version => "version",
                            RejectReason::BadPassword => "bad password",
                            RejectReason::TooManyAttempts => "too many attempts",
                        };
                        self.end_soon(
                            clock.mono_us,
                            EndReason::Refused {
                                reason: r.to_owned(),
                            },
                        );
                    }
                    Sender {
                        member: MemberId::HOST,
                        identity: self.host_identity.unwrap_or(Identity([0; 32])),
                    }
                } else {
                    match (l.member, l.identity) {
                        (Some(member), Some(identity)) => Sender { member, identity },
                        _ => return,
                    }
                };
                self.core(clock, Event::Net { from, msg }, out);
            }
            _ => tracing::warn!(link = peer, "unknown frame kind dropped"),
        }
    }

    fn on_auth(&mut self, clock: Clock, peer: PeerId, msg: AuthMsg, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        let is_host_link = Some(peer) == self.host_link;
        // The room secret that moves with the host role (from the current host only).
        if let AuthMsg::HandoverSecret { password } = &msg {
            if is_host_link && self.links.get(&peer).is_some_and(Link::authed) {
                self.handover_password = Some(password.clone());
            } else {
                tracing::warn!(link = peer, "unexpected handover secret ignored");
            }
            return;
        }
        let host_cfg = self.host_auth_cfg();
        let Some(l) = self.links.get_mut(&peer) else {
            return;
        };
        match &mut l.auth {
            LinkAuth::Host(h) => {
                let step = h.on_msg(&host_cfg, &mut self.throttle, now, msg);
                self.on_host_step(clock, peer, step, out);
            }
            LinkAuth::Member(m) => {
                let step = m.on_msg(msg);
                self.on_member_step(now, peer, step, out);
            }
            LinkAuth::Pending | LinkAuth::Done => {}
        }
    }

    fn on_host_step(&mut self, clock: Clock, peer: PeerId, step: HostStep, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        match step {
            HostStep::Wait => {}
            HostStep::Send(v) => {
                for m in &v {
                    self.send_auth(peer, m, out);
                }
            }
            HostStep::Reject { send, close } => {
                for m in &send {
                    self.send_auth(peer, m, out);
                    if let AuthMsg::Rejected { reason, .. } = m {
                        out.push(Output::Ui(RoomUi::Rejected {
                            member: None,
                            what: format!("join attempt refused (link {peer}): {reason:?}"),
                        }));
                    }
                }
                if close && let Some(l) = self.links.get_mut(&peer) {
                    l.close_at.get_or_insert(now + FLUSH_US);
                }
            }
            HostStep::Authenticated {
                identity,
                display_name,
                words,
                send,
            } => {
                for m in &send {
                    self.send_auth(peer, m, out);
                }
                let seat = match &self.peer {
                    Peer::Host(h) => h.seat_of(&identity).unwrap_or_else(|| h.free_member_id()),
                    Peer::Member(_) => return,
                };
                // An older link of the same member (it reconnected) is replaced.
                let dupes: Vec<PeerId> = self
                    .links
                    .iter()
                    .filter(|(id, l)| **id != peer && l.member == Some(seat))
                    .map(|(id, _)| *id)
                    .collect();
                for d in dupes {
                    self.links.remove(&d);
                    out.push(Output::ClosePeer(d));
                }
                let Some(l) = self.links.get_mut(&peer) else {
                    return;
                };
                l.auth = LinkAuth::Done;
                l.member = Some(seat);
                l.identity = Some(identity);
                let st = l.steady_state();
                tracing::info!(member = seat.0, id = %short_id(&identity), "member authenticated");
                self.core(
                    clock,
                    Event::PeerJoined {
                        member: seat,
                        identity,
                        display_name,
                    },
                    out,
                );
                let manual_words = self.links.get(&peer).and_then(|l| l.manual_words);
                self.invite_joined(peer, out);
                if self.links.get(&peer).is_some_and(|l| l.close_at.is_none()) {
                    // With invites the words were compared before admission: show the same ones.
                    if let Some(w) = manual_words {
                        out.push(Output::Ui(RoomUi::Verification {
                            member: Some(seat),
                            words: w,
                        }));
                    } else if self.password.is_none() {
                        out.push(Output::Ui(RoomUi::Verification {
                            member: Some(seat),
                            words,
                        }));
                    }
                    out.push(Output::Ui(RoomUi::Link {
                        member: Some(seat),
                        state: st,
                    }));
                }
            }
        }
    }

    fn on_member_step(&mut self, now: i64, peer: PeerId, step: MemberStep, out: &mut Vec<Output>) {
        match step {
            MemberStep::Wait => {}
            MemberStep::Send(v) => {
                if let Some(l) = self.links.get_mut(&peer) {
                    l.need_password = false;
                }
                for m in &v {
                    self.send_auth(peer, m, out);
                }
            }
            MemberStep::NeedPassword => {
                if let Some(l) = self.links.get_mut(&peer) {
                    l.need_password = true;
                }
                out.push(Output::Ui(RoomUi::NeedPassword {
                    wrong: false,
                    retry_after_ms: 0,
                }));
            }
            MemberStep::Authenticated {
                host,
                words,
                password_ignored,
                ..
            } => {
                if password_ignored && !self.joined_once {
                    out.push(Output::Ui(RoomUi::PasswordIgnored));
                }
                let pw_room = self.password.is_some();
                let Some(l) = self.links.get_mut(&peer) else {
                    return;
                };
                l.auth = LinkAuth::Done;
                l.identity = Some(host);
                let st = l.steady_state();
                let manual_words = l.manual_words;
                self.host_identity = Some(host);
                self.admit_deadline = None;
                tracing::info!(host = %short_id(&host), "host authenticated");
                if let Some(w) = manual_words {
                    out.push(Output::Ui(RoomUi::Verification {
                        member: None,
                        words: w,
                    }));
                } else if !pw_room {
                    out.push(Output::Ui(RoomUi::Verification {
                        member: None,
                        words,
                    }));
                }
                out.push(Output::Ui(RoomUi::Link {
                    member: None,
                    state: st,
                }));
                self.joined_once = true;
                self.set_phase(Phase::InRoom, out);
            }
            MemberStep::Rejected {
                reason,
                retry_after_ms,
            } => {
                if retry_after_ms > 0 {
                    if let Some(l) = self.links.get_mut(&peer) {
                        l.need_password = true;
                        l.auth_started = Some(now);
                    }
                    self.password = None;
                    out.push(Output::Ui(RoomUi::NeedPassword {
                        wrong: reason == AuthReject::BadPassword,
                        retry_after_ms,
                    }));
                } else {
                    self.end_now(
                        EndReason::AuthRejected {
                            reason: format!("{reason:?}"),
                        },
                        out,
                    );
                }
            }
            MemberStep::Failed(why) => {
                tracing::warn!(why, "the host failed authentication");
                self.end_now(EndReason::HostNotVerified, out);
            }
        }
    }

    // ------------------------------------------------------------------ actions -----------

    fn on_action(&mut self, clock: Clock, a: Action, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        match a {
            Action::SubmitPassword(p) => {
                self.password = Some(p.clone()).filter(|p| !p.is_empty());
                let Some(h) = self.host_link else { return };
                let Some(l) = self.links.get_mut(&h) else {
                    return;
                };
                if let LinkAuth::Member(m) = &mut l.auth {
                    let step = m.submit_password(p);
                    self.on_member_step(now, h, step, out);
                }
            }
            Action::Leave => {
                if self.peer.is_host() {
                    self.core(clock, Event::Ui(UiAction::CloseRoom), out);
                } else {
                    if let Some(h) = self
                        .host_link
                        .filter(|h| self.links.get(h).is_some_and(Link::authed))
                    {
                        self.send_room(h, &Message::Bye, out);
                    }
                    self.end_soon(now, EndReason::Left);
                }
            }
            Action::CreateInvite { for_member } => self.create_invite(clock, for_member, out),
            Action::SubmitReply(text) => self.submit_reply(clock, &text, out),
            Action::Admit(id) => self.admit(&id, out),
            Action::Refuse(id) => self.refuse_or_remove(&id, false, out),
            Action::RemoveInvite(id) => self.refuse_or_remove(&id, true, out),
            Action::Ui(UiAction::MakeHost(_)) if self.is_manual() => {
                // Moving the host role needs every member to reach the new host: not possible
                // without a signaling server yet (ADR 0014).
                out.push(Output::Ui(RoomUi::Error(
                    "Make host is not available in rooms joined with invites.".into(),
                )));
            }
            Action::Ui(UiAction::MakeHost(m)) if self.peer.is_host() => {
                let target = self
                    .link_of_member(m)
                    .and_then(|id| self.links.get(&id).and_then(|l| l.sig).map(|s| (id, s)));
                let Some((link_id, sig)) = target else {
                    out.push(Output::Ui(RoomUi::Error(
                        "That member is not connected right now.".into(),
                    )));
                    return;
                };
                // The room secret first (over the authenticated link), then the handover.
                self.send_auth(
                    link_id,
                    &AuthMsg::HandoverSecret {
                        password: self.password.clone(),
                    },
                    out,
                );
                self.transfer_target = Some((link_id, sig));
                self.core(clock, Event::Ui(UiAction::MakeHost(m)), out);
            }
            Action::Ui(ui) => self.core(clock, Event::Ui(ui), out),
        }
    }

    // ------------------------------------------------------------------ timers ------------

    fn on_tick(&mut self, clock: Clock, out: &mut Vec<Output>) {
        let now = clock.mono_us;
        self.core(clock, Event::Tick, out);
        let due: Vec<PeerId> = self
            .links
            .iter()
            .filter(|(_, l)| {
                l.close_at.is_some_and(|t| now >= t)
                    || l.reoffer_deadline.is_some_and(|t| now >= t)
                    || (l.open
                        && !matches!(l.auth, LinkAuth::Done)
                        && l.auth_started.is_some_and(|t| {
                            now - t
                                >= if l.need_password {
                                    PASSWORD_TIMEOUT_US
                                } else {
                                    AUTH_TIMEOUT_US
                                }
                        }))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let was_host_link = Some(id) == self.host_link;
            self.close_link_now(id, out);
            if was_host_link && self.ending.is_none() && !self.peer.is_host() {
                self.end_now(
                    EndReason::Error {
                        message: "Could not finish joining the room in time.".into(),
                    },
                    out,
                );
            }
        }
        self.manual_tick(clock, out);
        self.flush_pending_left(clock, out);
        self.maybe_reconnect_after_transfer(now, out);
        self.maybe_claim_host(now, out);
        if let Some((t, reason)) = self.ending.clone()
            && now >= t
        {
            self.end_now(reason, out);
        }
    }
}

// ---------------------------------------------------------------------- manual invites ----

/// Unix seconds from the session clock.
fn unix(clock: Clock) -> u64 {
    (clock.wall_ms.max(0) / 1000) as u64
}

/// A display name: no control characters, at most 40 characters, never empty.
fn clean_name(s: &str) -> String {
    let n: String = s
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(invite::MAX_NAME_CHARS)
        .collect();
    if n.is_empty() {
        "SyncWatch room".into()
    } else {
        n
    }
}

fn parse_hex16(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    for (i, out) in b.iter_mut().enumerate() {
        *out = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(b)
}

impl RoomSession {
    /// Manual invites: collects the local offer or answer and its candidates. Returns true if
    /// the signal was taken (nothing goes to a signaling server in this mode).
    fn manual_local_signal(
        &mut self,
        peer: PeerId,
        data: SignalData,
        out: &mut Vec<Output>,
    ) -> bool {
        if !self.is_manual() {
            return false;
        }
        let Some(g) = self.links.get_mut(&peer).and_then(|l| l.gather.as_mut()) else {
            return true;
        };
        if g.done {
            return true;
        }
        let end = match data {
            SignalData::Offer { sdp } | SignalData::Answer { sdp } => {
                g.sdp = Some(sdp);
                false
            }
            SignalData::Candidate {
                candidate,
                sdp_mid,
                sdp_m_line_index,
            } => {
                if g.cands.len() < invite::MAX_CANDIDATES
                    && candidate.len() <= invite::MAX_CANDIDATE_LEN
                    && !candidate.is_empty()
                {
                    g.cands.push(Candidate {
                        candidate,
                        mid: sdp_mid,
                        mline: sdp_m_line_index,
                    });
                }
                false
            }
            SignalData::EndOfCandidates => true,
        };
        if end {
            self.finish_gather(peer, out);
        }
        true
    }

    /// Candidate gathering ended (or timed out): the invite or reply text is ready.
    fn finish_gather(&mut self, peer: PeerId, out: &mut Vec<Output>) {
        let Some(l) = self.links.get_mut(&peer) else {
            return;
        };
        let Some(g) = l.gather.as_mut() else {
            return;
        };
        if g.done {
            return;
        }
        g.done = true;
        let Some(sdp) = g.sdp.clone() else {
            tracing::warn!(link = peer, "no local description gathered");
            out.push(Output::Ui(RoomUi::InviteError(
                "The connection could not be prepared. Check Settings > Connection and try again."
                    .into(),
            )));
            if let Some(id) = l.invite_id
                && let Some(inv) = self.invites.get_mut(&id)
            {
                inv.state = InvState::Failed;
            }
            self.publish_invites(out);
            return;
        };
        let cands = g.cands.clone();
        if Some(peer) == self.host_link {
            // Member: the reply.
            let Some(inv) = self.my_invite.clone() else {
                return;
            };
            let my_fp = crate::sdp::dtls_fingerprint(&sdp).unwrap_or_default();
            let host_fp = inv.fingerprint().unwrap_or_default();
            let me = self.cfg.key.public();
            let words = invite::invite_words(
                &inv.room_id,
                &inv.invite_id,
                &host_fp,
                &my_fp,
                &inv.host,
                &me,
            );
            l.manual_words = Some(words);
            let reply = Reply {
                protocol: syncwatch_protocol::PROTOCOL_VERSION,
                invite_id: inv.invite_id,
                member: me,
                display_name: self.cfg.display_name.clone(),
                sdp,
                candidates: cands,
            };
            let text = reply.encode(&self.cfg.key);
            tracing::info!(chars = text.len(), "reply ready");
            out.push(Output::Ui(RoomUi::Reply { text, words }));
            out.push(Output::Ui(RoomUi::Verification {
                member: None,
                words,
            }));
        } else if let Some(id) = l.invite_id {
            // Host: the invite.
            let Some(p) = self.invites.get_mut(&id) else {
                return;
            };
            let inv = Invite {
                protocol: syncwatch_protocol::PROTOCOL_VERSION,
                room_id: self.room_id,
                invite_id: id,
                host: self.cfg.key.public(),
                room_name: self.room_name.clone(),
                password: self.password.is_some(),
                expires_unix: p.expires_unix,
                for_member: p.for_member.map(|(_, i)| i),
                sdp,
                candidates: cands,
            };
            p.local_fp = crate::sdp::dtls_fingerprint(&inv.sdp);
            let text = inv.encode(&self.cfg.key);
            tracing::info!(
                chars = text.len(),
                candidates = inv.candidates.len(),
                "invite ready"
            );
            p.text = Some(text);
            if p.state == InvState::Preparing {
                p.state = InvState::Waiting;
            }
            self.publish_invites(out);
        }
    }

    fn publish_invites(&self, out: &mut Vec<Output>) {
        if !self.peer.is_host() || !self.is_manual() {
            return;
        }
        let v = self
            .invites
            .iter()
            .map(|(id, p)| InviteView {
                id: invite::hex16(id),
                status: p.state.text(),
                text: p
                    .text
                    .clone()
                    .filter(|_| matches!(p.state, InvState::Waiting | InvState::Replied)),
                expires_unix: p.expires_unix,
                member_name: p.reply.as_ref().map(|r| r.display_name.clone()),
                words: p.words,
                for_member: p.for_member.map(|(m, _)| m.0),
            })
            .collect();
        out.push(Output::Ui(RoomUi::Invites(v)));
    }

    fn create_invite(&mut self, clock: Clock, for_member: Option<MemberId>, out: &mut Vec<Output>) {
        let HandshakeMode::ManualInvite {
            invite_ttl_secs, ..
        } = self.cfg.handshake
        else {
            return;
        };
        if !self.peer.is_host() {
            return;
        }
        let open = self
            .invites
            .values()
            .filter(|p| {
                matches!(
                    p.state,
                    InvState::Preparing | InvState::Waiting | InvState::Replied
                )
            })
            .count();
        if open >= MAX_OPEN_INVITES {
            out.push(Output::Ui(RoomUi::InviteError(
                "There are already 12 open invites. Remove some first.".into(),
            )));
            return;
        }
        let bound = match (for_member, &self.peer) {
            (Some(m), Peer::Host(h)) => match h.identity_of(m) {
                Some(i) => Some((m, i)),
                None => {
                    out.push(Output::Ui(RoomUi::InviteError(
                        "That member's seat is no longer kept. Send a normal invite.".into(),
                    )));
                    return;
                }
            },
            _ => None,
        };
        let id = invite::random16();
        let link = self.alloc_link(None);
        if let Some(l) = self.links.get_mut(&link) {
            l.invite_id = Some(id);
            l.gather = Some(Gather {
                sdp: None,
                cands: Vec::new(),
                deadline: clock.mono_us + GATHER_WAIT_US,
                done: false,
            });
        }
        let ttl = invite_ttl_secs.clamp(invite::MIN_TTL_SECS, invite::MAX_TTL_SECS);
        self.invites.insert(
            id,
            PendingInvite {
                link,
                expires_unix: unix(clock) + ttl,
                for_member: bound,
                state: InvState::Preparing,
                text: None,
                local_fp: None,
                reply: None,
                words: None,
            },
        );
        out.push(Output::CreatePeer {
            peer: link,
            role: IceRole::Offerer,
            ice: self.cfg.ice.clone(),
        });
        self.publish_invites(out);
    }

    fn submit_reply(&mut self, clock: Clock, text: &str, out: &mut Vec<Output>) {
        match self.check_reply(clock, text) {
            Ok(()) => self.publish_invites(out),
            Err(e) => {
                tracing::info!(error = ?e, "reply refused");
                out.push(Output::Ui(RoomUi::InviteError(e.to_string())));
                self.publish_invites(out);
            }
        }
    }

    fn check_reply(&mut self, clock: Clock, text: &str) -> Result<(), InviteError> {
        let r = Reply::decode(text)?;
        let me = self.cfg.key.public();
        let room_id = self.room_id;
        let Some(p) = self.invites.get_mut(&r.invite_id) else {
            return Err(InviteError::UnknownInvite);
        };
        match p.state {
            InvState::Waiting => {}
            InvState::Expired => return Err(InviteError::Expired),
            InvState::Preparing | InvState::Failed => return Err(InviteError::UnknownInvite),
            InvState::Replied | InvState::Admitted | InvState::Joined | InvState::Refused => {
                return Err(InviteError::Used);
            }
        }
        if unix(clock) > p.expires_unix {
            p.state = InvState::Expired;
            return Err(InviteError::Expired);
        }
        if let Some((_, want)) = p.for_member
            && want != r.member
        {
            return Err(InviteError::WrongMember);
        }
        let (Some(host_fp), Some(member_fp)) = (p.local_fp.clone(), r.fingerprint()) else {
            return Err(InviteError::BadConnection);
        };
        p.words = Some(invite::invite_words(
            &room_id,
            &r.invite_id,
            &host_fp,
            &member_fp,
            &me,
            &r.member,
        ));
        p.state = InvState::Replied;
        p.reply = Some(r);
        Ok(())
    }

    fn admit(&mut self, id: &str, out: &mut Vec<Output>) {
        let Some(id) = parse_hex16(id) else {
            return;
        };
        let Some(p) = self.invites.get_mut(&id) else {
            return;
        };
        if p.state != InvState::Replied {
            return;
        }
        let Some(r) = p.reply.clone() else {
            return;
        };
        p.state = InvState::Admitted;
        let link = p.link;
        let words = p.words;
        if let Some(l) = self.links.get_mut(&link) {
            l.expect_fp = r.fingerprint();
            l.manual_words = words;
        }
        tracing::info!(id = %short_id(&r.member), "member admitted");
        for data in r.signals() {
            out.push(Output::RemoteSignal { peer: link, data });
        }
        self.publish_invites(out);
    }

    fn refuse_or_remove(&mut self, id: &str, remove: bool, out: &mut Vec<Output>) {
        let Some(id) = parse_hex16(id) else {
            return;
        };
        let Some(p) = self.invites.get_mut(&id) else {
            return;
        };
        let link = p.link;
        let joined = p.state == InvState::Joined;
        if remove {
            self.invites.remove(&id);
        } else if p.state == InvState::Replied {
            p.state = InvState::Refused;
        } else {
            return;
        }
        // A joined member stays connected when its old invite is removed from the list.
        if !joined && self.links.contains_key(&link) {
            self.close_link_now(link, out);
        }
        self.publish_invites(out);
    }

    /// Host: the member admitted through `peer` finished authentication.
    fn invite_joined(&mut self, peer: PeerId, out: &mut Vec<Output>) {
        let Some(id) = self.links.get(&peer).and_then(|l| l.invite_id) else {
            return;
        };
        if let Some(p) = self.invites.get_mut(&id) {
            p.state = InvState::Joined;
            self.publish_invites(out);
        }
    }

    /// Host: a link closed; its invite failed if the member never got in.
    fn invite_link_closed(&mut self, peer: PeerId, out: &mut Vec<Output>) {
        let Some(id) = self.links.get(&peer).and_then(|l| l.invite_id) else {
            return;
        };
        if let Some(p) = self.invites.get_mut(&id)
            && matches!(
                p.state,
                InvState::Admitted | InvState::Waiting | InvState::Preparing
            )
        {
            p.state = InvState::Failed;
            self.publish_invites(out);
        }
    }

    /// Manual invites: gather timeouts, invite expiry, silent links, the admission deadline.
    fn manual_tick(&mut self, clock: Clock, out: &mut Vec<Output>) {
        if !self.is_manual() {
            return;
        }
        let now = clock.mono_us;
        let late: Vec<PeerId> = self
            .links
            .iter()
            .filter(|(_, l)| {
                l.gather
                    .as_ref()
                    .is_some_and(|g| !g.done && now >= g.deadline)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            self.finish_gather(id, out);
        }
        let t = unix(clock);
        let expired: Vec<[u8; 16]> = self
            .invites
            .iter()
            .filter(|(_, p)| {
                matches!(
                    p.state,
                    InvState::Preparing | InvState::Waiting | InvState::Replied
                ) && t > p.expires_unix
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            if let Some(p) = self.invites.get_mut(id) {
                p.state = InvState::Expired;
                let link = p.link;
                if self.links.contains_key(&link) {
                    self.close_link_now(link, out);
                }
            }
        }
        if !expired.is_empty() {
            self.publish_invites(out);
        }
        // Silent links: without signaling there is no ICE restart.
        let lost: Vec<PeerId> = self
            .links
            .iter()
            .filter(|(_, l)| l.lost_since.is_some_and(|s| now - s >= LOST_WAIT_US))
            .map(|(id, _)| *id)
            .collect();
        for id in lost {
            if Some(id) == self.host_link {
                self.lost_host(out);
            } else {
                self.close_link_now(id, out);
            }
        }
        if let Some(d) = self.admit_deadline
            && now >= d
            && !self.joined_once
            && self.ending.is_none()
        {
            self.end_now(EndReason::NotAdmitted, out);
        }
    }

    /// Member, manual invites: the link to the host is gone for good.
    fn lost_host(&mut self, out: &mut Vec<Output>) {
        let seat_keep_secs = match self.cfg.handshake {
            HandshakeMode::ManualInvite { seat_keep_us, .. } => (seat_keep_us / 1_000_000) as u64,
            HandshakeMode::Signaling => 0,
        };
        if self.joined_once {
            self.end_now(EndReason::ConnectionLost { seat_keep_secs }, out);
        } else {
            self.end_now(EndReason::NotAdmitted, out);
        }
    }
}

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

#[derive(Clone)]
pub struct SessionConfig {
    pub key: IdentityKey,
    pub display_name: String,
    pub ice: IceConfig,
    pub max_members: u8,
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
    /// The member typed a password but the room has none (it was ignored). Once per join.
    PasswordIgnored,
    /// Host: something from member was refused (None: a joining link not yet a member).
    Rejected {
        member: Option<MemberId>,
        what: String,
    },
    Core(UiEvent),
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
                let lane = match msg.channel() {
                    Channel::Reliable => Lane::Reliable,
                    Channel::Unreliable => Lane::Unreliable,
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
        out.push(Output::Signaling(ClientMsg::Leave));
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
            ServerMsg::BecameHost { token, .. } => {
                self.token = Some(token);
                self.sig_host = true;
                if self.peer.is_host() {
                    self.flush_pending_signals(clock, out);
                }
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
            | ServerMsg::HostBack
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
                let st = l.steady_state();
                let authed = l.authed();
                self.link_ui(peer, st, out);
                if Some(peer) == self.host_link && authed {
                    self.set_phase(Phase::InRoom, out);
                }
            }
            TransportEvent::RestartNeeded { peer, diag } => {
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
                if self.links.get(&peer).is_some_and(|l| l.close_at.is_none()) {
                    if self.password.is_none() {
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
                self.host_identity = Some(host);
                tracing::info!(host = %short_id(&host), "host authenticated");
                if !pw_room {
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
        self.flush_pending_left(clock, out);
        self.maybe_reconnect_after_transfer(now, out);
        if let Some((t, reason)) = self.ending.clone()
            && now >= t
        {
            self.end_now(reason, out);
        }
    }
}

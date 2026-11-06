//! The host: the single authority of a room (host-star topology).
//!
//! Members send requests; the host validates them (permissions, epochs, limits), applies them
//! and broadcasts ONE authoritative command that everyone, including the requester and the host
//! itself, applies. Track control and moderation are host-only; anything a member sends that
//! only a host may send is rejected and logged as a protocol violation.

use crate::chat::{ChatEngine, Submit, sanitize_name};
use crate::gate::{Gate, Pending};
use crate::local::Local;
use crate::timing::*;
use crate::types::*;
use std::collections::{BTreeMap, HashSet};
use syncwatch_protocol::limits::*;
use syncwatch_protocol::*;

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub identity: Identity,
    pub display_name: String,
    /// Members including the host; never above `limits::MAX_MEMBERS`.
    pub max_members: usize,
    pub gate_timeout_us: i64,
    /// External file ceilings; `None` = "No limit".
    pub max_external_sub: Option<u64>,
    pub max_external_audio: Option<u64>,
}

impl HostConfig {
    pub fn new(identity: Identity, display_name: &str) -> Self {
        Self {
            identity,
            display_name: sanitize_name(display_name),
            max_members: MAX_MEMBERS,
            gate_timeout_us: GATE_TIMEOUT_US,
            max_external_sub: Some(DEFAULT_MAX_EXTERNAL_SUB),
            max_external_audio: Some(DEFAULT_MAX_EXTERNAL_AUDIO),
        }
    }
}

#[derive(Debug, Clone)]
struct MemberEntry {
    identity: Identity,
    name: String,
    perms: Permissions,
    status: Option<MemberStatus>,
    connected: bool,
    /// When the transport dropped; the seat is kept for [`SEAT_RETENTION_US`].
    left_at: Option<i64>,
}

#[derive(Debug)]
pub struct HostMachine {
    cfg: HostConfig,
    pub me: MemberId,
    pub local: Local,
    descriptor: Option<MediaDescriptor>,
    tracks: TrackState,
    members: BTreeMap<MemberId, MemberEntry>,
    default_perms: Permissions,
    bans: Vec<Identity>,
    locked: bool,
    next_member_id: u32,
    next_external_id: u32,
    chat: ChatEngine,
    gate: Gate,
    last_change: Option<(MemberId, u32, i64)>,
    last_heartbeat_us: Option<i64>,
    last_members: Vec<MemberInfo>,
    local_mutes: HashSet<MemberId>,
    handover: Option<HostHandover>,
    /// The succession state last sent, and to whom (sent again only when either changes).
    successor_sent: Option<(MemberId, SuccessionState)>,
    closed: bool,
}

impl HostMachine {
    pub fn new(cfg: HostConfig, now: i64) -> Self {
        let me = MemberId::HOST;
        let mut cfg = cfg;
        cfg.max_members = cfg.max_members.clamp(2, MAX_MEMBERS);
        let gate = Gate::new(cfg.gate_timeout_us);
        Self {
            cfg,
            me,
            local: Local::new(me, now),
            descriptor: None,
            tracks: TrackState::default(),
            members: BTreeMap::new(),
            default_perms: Permissions::default(),
            bans: Vec::new(),
            locked: false,
            next_member_id: 1,
            next_external_id: 1,
            chat: ChatEngine::default(),
            gate,
            last_change: None,
            last_heartbeat_us: None,
            last_members: Vec::new(),
            local_mutes: HashSet::new(),
            handover: None,
            successor_sent: None,
            closed: false,
        }
    }

    /// Becomes host from a handover (the former host's state) keeping our local participant.
    pub fn from_handover(h: HostHandover, local: Local, mut cfg: HostConfig, now: i64) -> Self {
        cfg.identity = h.new_host_identity;
        let me = h.new_host;
        let mut members = BTreeMap::new();
        for m in &h.members {
            if m.id == me {
                continue;
            }
            let identity = h
                .identities
                .iter()
                .find(|(id, _)| *id == m.id)
                .map(|(_, i)| *i)
                .unwrap_or(Identity([0; 32]));
            members.insert(
                m.id,
                MemberEntry {
                    identity,
                    name: m.display_name.clone(),
                    perms: m.permissions,
                    status: None,
                    // After a succession the others still have to reconnect: their seats
                    // are kept for the usual retention time.
                    connected: m.connected,
                    left_at: (!m.connected).then_some(now),
                },
            );
        }
        let muted: Vec<Identity> = h.muted.clone();
        let next_member_id = h.members.iter().map(|m| m.id.0).max().unwrap_or(0) + 1;
        let mut host = Self {
            gate: Gate::new(cfg.gate_timeout_us),
            cfg,
            me,
            local,
            descriptor: h.descriptor,
            tracks: h.tracks,
            members,
            default_perms: h.default_permissions,
            bans: h.bans,
            locked: h.locked,
            next_member_id,
            next_external_id: h.next_external_id,
            chat: ChatEngine::from_handover(h.chat, h.chat_config, h.chat_next_seq, muted),
            last_change: None,
            last_heartbeat_us: None,
            last_members: Vec::new(),
            local_mutes: HashSet::new(),
            handover: None,
            successor_sent: None,
            closed: false,
        };
        host.local.room = h.playback;
        host
    }

    /// Right after becoming host: re-announce room, tracks and members under OUR clock.
    pub fn after_takeover(&mut self, now: i64) -> Vec<Effect> {
        let mut out = Out::default();
        let r = self.local.room;
        let p = RoomPlayback {
            position_ms: r.position_at(now),
            anchor_host_us: if r.paused { now } else { now + self.lead_us() },
            ..r
        };
        self.local.room = RoomPlayback {
            epoch: r.epoch,
            ..p
        };
        self.commit(p, now, &mut out);
        self.broadcast_cmd(Command::Tracks(self.tracks.clone()), &mut out);
        out.ui(UiEvent::BecameHost);
        self.broadcast_members_if_changed(&mut out);
        out.into_vec()
    }

    pub fn take_handover(&mut self) -> Option<HostHandover> {
        self.handover.take()
    }

    pub fn room(&self) -> RoomPlayback {
        self.local.room
    }

    pub fn tracks(&self) -> &TrackState {
        &self.tracks
    }

    pub fn chat_history(&self) -> Vec<ChatMessage> {
        self.chat.history()
    }

    /// The room's current media (None until the host picks a source).
    pub fn descriptor(&self) -> Option<&MediaDescriptor> {
        self.descriptor.as_ref()
    }

    /// One member's chat state (global switch, slow mode, its own mute flag).
    fn chat_state_for(&self, m: MemberId) -> ChatState {
        ChatState {
            config: self.chat.config,
            muted: self
                .members
                .get(&m)
                .is_some_and(|e| self.chat.is_muted(&e.identity)),
        }
    }

    pub fn chat_state(&self) -> ChatState {
        ChatState {
            config: self.chat.config,
            muted: false,
        }
    }

    /// The seat a (re)connecting identity gets back, if it is still kept.
    pub fn seat_of(&self, identity: &Identity) -> Option<MemberId> {
        self.members
            .iter()
            .find(|(_, e)| e.identity == *identity)
            .map(|(id, _)| *id)
    }

    /// A member number no current or kept seat uses (for a newly authenticated member).
    pub fn free_member_id(&self) -> MemberId {
        let mut n = self.next_member_id.max(1);
        while self.members.contains_key(&MemberId(n)) || MemberId(n) == self.me {
            n += 1;
        }
        MemberId(n)
    }

    /// Identity of a seat (connected or kept).
    pub fn identity_of(&self, m: MemberId) -> Option<Identity> {
        self.members.get(&m).map(|e| e.identity)
    }

    /// Seats whose member is currently marked connected.
    pub fn connected_members(&self) -> Vec<MemberId> {
        self.members
            .iter()
            .filter(|(_, e)| e.connected)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn bans(&self) -> &[Identity] {
        &self.bans
    }

    pub fn permissions_of(&self, m: MemberId) -> Option<Permissions> {
        if m == self.me {
            return Some(Permissions::ALL);
        }
        self.members.get(&m).map(|e| e.perms)
    }

    pub fn next_deadline(&self) -> Option<i64> {
        self.local.next_deadline(Some)
    }

    pub fn handle(&mut self, clock: Clock, ev: Event) -> Vec<Effect> {
        let mut out = Out::default();
        if self.closed {
            return out.into_vec();
        }
        let now = clock.mono_us;
        match ev {
            Event::Tick => self.on_tick(now, &mut out),
            Event::Ui(a) => self.on_ui(clock, a, &mut out),
            Event::Net { from, msg } => self.on_net(clock, from, msg, &mut out),
            Event::PeerJoined {
                member,
                identity,
                display_name,
            } => self.on_join(member, identity, &display_name, now, &mut out),
            Event::PeerLeft { member } => {
                if let Some(e) = self.members.get_mut(&member) {
                    e.connected = false;
                    e.left_at = Some(now);
                }
                self.try_resume(now, &mut out);
            }
            Event::Player(obs) => {
                self.local.on_player(obs, Some(now), now, &mut out);
                self.check_buffering(MemberId(u32::MAX), now, &mut out);
                self.try_resume(now, &mut out);
            }
            Event::LocalFile(e) => self.local.on_local_file(e, &mut out),
            Event::Torrent(e) => self.local.on_torrent(e, &mut out),
            Event::ExternalCached(_) => {}
        }
        self.broadcast_members_if_changed(&mut out);
        self.update_successor(&mut out);
        out.into_vec()
    }

    /// The designated successor: the lowest connected member id (the same rank-0 rule the
    /// members use to decide who claims the room after a host crash).
    pub fn designated_successor(&self) -> Option<MemberId> {
        self.members
            .iter()
            .find(|(id, e)| e.connected && **id != self.me)
            .map(|(id, _)| *id)
    }

    /// What the successor needs to keep the room's rules after a host crash.
    pub fn succession_state(&self) -> SuccessionState {
        let mut muted = self.chat.muted();
        muted.sort_by_key(|i| i.0);
        SuccessionState {
            seats: self
                .members
                .iter()
                .filter(|(id, _)| **id != self.me)
                .map(|(id, e)| SuccessorSeat {
                    id: *id,
                    identity: e.identity,
                    display_name: e.name.clone(),
                    permissions: e.perms,
                })
                .collect(),
            default_permissions: self.default_perms,
            muted,
            bans: self.bans.clone(),
            locked: self.locked,
        }
    }

    /// Keeps the designated successor up to date: the state goes out on every change and to
    /// every new successor.
    fn update_successor(&mut self, out: &mut Out) {
        let Some(to) = self.designated_successor() else {
            self.successor_sent = None;
            return;
        };
        let st = self.succession_state();
        if self
            .successor_sent
            .as_ref()
            .is_some_and(|(t, s)| *t == to && *s == st)
        {
            return;
        }
        out.push(Effect::SendTo(
            to,
            Message::Command(Command::Succession(Box::new(st.clone()))),
        ));
        self.successor_sent = Some((to, st));
    }

    // ------------------------------------------------------------------ helpers -----------

    fn broadcast_cmd(&self, c: Command, out: &mut Out) {
        out.push(Effect::Broadcast(Message::Command(c)));
    }

    fn notify(&self, to: MemberId, n: Notice, out: &mut Out) {
        if to == self.me {
            out.ui(UiEvent::Notice(n));
        } else {
            out.push(Effect::SendTo(to, Message::Command(Command::Notice(n))));
        }
    }

    fn lead_us(&self) -> i64 {
        let max_rtt_ms = self
            .members
            .values()
            .filter(|m| m.connected)
            .filter_map(|m| m.status.and_then(|s| s.rtt_ms))
            .max()
            .unwrap_or(0) as i64;
        (max_rtt_ms * 1_000 + 200_000).clamp(MIN_LEAD_US, MAX_LEAD_US)
    }

    /// Commit a new room playback state: broadcast it and apply it locally.
    fn commit(&mut self, mut p: RoomPlayback, now: i64, out: &mut Out) {
        p.epoch = Epoch(self.local.room.epoch.0 + 1);
        p.media_epoch = self.local.room.media_epoch;
        self.broadcast_cmd(Command::Playback(p), out);
        self.local.apply_room(p, Some(now), now, out);
    }

    fn member_infos(&self) -> Vec<MemberInfo> {
        let host_status = self.local.status(None, None);
        let mut v = vec![MemberInfo {
            id: self.me,
            display_name: self.cfg.display_name.clone(),
            is_host: true,
            permissions: Permissions::ALL,
            media: host_status.media,
            buffering: host_status.buffering,
            tracks: host_status.tracks,
            chat_muted: false,
            connected: true,
        }];
        v.extend(
            self.members
                .iter()
                // Disconnected members stay listed (as reconnecting) while their seat is kept.
                .map(|(id, e)| MemberInfo {
                    id: *id,
                    display_name: e.name.clone(),
                    is_host: false,
                    permissions: e.perms,
                    media: e.status.map(|s| s.media).unwrap_or(MediaStatus::NoMedia),
                    buffering: e.status.is_some_and(|s| s.buffering),
                    tracks: e
                        .status
                        .map(|s| s.tracks)
                        .unwrap_or(TrackSyncStatus::Applying),
                    chat_muted: self.chat.is_muted(&e.identity),
                    connected: e.connected,
                }),
        );
        v
    }

    fn broadcast_members_if_changed(&mut self, out: &mut Out) {
        let infos = self.member_infos();
        if infos != self.last_members {
            self.last_members = infos.clone();
            out.ui(UiEvent::Members(infos.clone()));
            self.broadcast_cmd(Command::Members(infos), out);
        }
    }

    fn connected_count(&self) -> usize {
        1 + self.members.values().filter(|m| m.connected).count()
    }

    // ------------------------------------------------------------------ gate --------------

    fn blockers(&self) -> Vec<(MemberId, NotReadyReason)> {
        let room = self.local.room;
        let ignore = self
            .gate
            .pending
            .as_ref()
            .map(|p| p.ignore.clone())
            .unwrap_or_default();
        let mut v = Vec::new();
        if self.descriptor.is_some() {
            let me = self.local.status(None, None);
            if let Some(r) = self.gate.blocks(Some(&me), &room)
                && !ignore.contains(&self.me)
            {
                v.push((self.me, r));
            }
        }
        for (id, e) in &self.members {
            if !e.connected || ignore.contains(id) {
                continue;
            }
            if let Some(r) = self.gate.blocks(e.status.as_ref(), &room) {
                v.push((*id, r));
            }
        }
        v
    }

    fn publish_gate(&self, active: bool, waiting: Vec<(MemberId, NotReadyReason)>, out: &mut Out) {
        let g = GateInfo { active, waiting };
        out.ui(UiEvent::Gate(g.clone()));
        self.broadcast_cmd(Command::ReadyGate(g), out);
    }

    /// Play requested: start now (scheduled with lead) if everyone needed is ready, else wait.
    fn request_play(&mut self, now: i64, out: &mut Out) {
        if self.gate.pending.is_none() {
            self.gate.pending = Some(Pending {
                since_us: now,
                ignore: HashSet::new(),
            });
        }
        self.try_resume(now, out);
    }

    fn try_resume(&mut self, now: i64, out: &mut Out) {
        if self.gate.pending.is_none() {
            return;
        }
        let waiting = self.blockers();
        if !waiting.is_empty() && !self.gate.timed_out(now) {
            self.publish_gate(true, waiting, out);
            return;
        }
        // Whoever still blocks (timeout) or was ignored is excused until ready again.
        let ignored = self
            .gate
            .pending
            .as_ref()
            .map(|p| p.ignore.clone())
            .unwrap_or_default();
        self.gate.excused.extend(waiting.iter().map(|(m, _)| *m));
        self.gate.excused.extend(ignored);
        self.gate.pending = None;
        let r = self.local.room;
        if r.paused {
            let p = RoomPlayback {
                position_ms: r.position_ms,
                anchor_host_us: now + self.lead_us(),
                paused: false,
                ..r
            };
            self.commit(p, now, out);
        }
        self.publish_gate(false, Vec::new(), out);
    }

    /// A member (or the host) that holds the media started buffering while playing: pause
    /// everyone at the current position and resume automatically when ready.
    fn check_buffering(&mut self, _who: MemberId, now: i64, out: &mut Out) {
        let r = self.local.room;
        if r.paused || !r.is_playing_at(now) {
            return;
        }
        let excused = self.gate.excused.clone();
        let buffering = self
            .blockers()
            .iter()
            .any(|(m, why)| *why == NotReadyReason::Buffering && !excused.contains(m));
        if buffering {
            let p = RoomPlayback {
                position_ms: r.position_at(now),
                anchor_host_us: now,
                paused: true,
                ..r
            };
            self.commit(p, now, out);
            self.gate.pending = Some(Pending {
                since_us: now,
                ignore: HashSet::new(),
            });
            let waiting = self.blockers();
            self.publish_gate(true, waiting, out);
        }
    }

    // ------------------------------------------------------------------ tick --------------

    fn on_tick(&mut self, now: i64, out: &mut Out) {
        // A dropped member keeps their seat for 20 s (spec section 1); then it is freed.
        self.members
            .retain(|_, e| e.connected || e.left_at.is_none_or(|t| now - t < SEAT_RETENTION_US));
        self.local.tick(Some(now), now, out);
        if self.gate.pending.is_some() {
            self.try_resume(now, out);
        }
        if self
            .last_heartbeat_us
            .is_none_or(|t| now - t >= HEARTBEAT_US)
        {
            self.last_heartbeat_us = Some(now);
            out.push(Effect::Broadcast(Message::Heartbeat {
                playback: self.local.room,
                host_now_us: now,
            }));
        }
    }

    // ------------------------------------------------------------------ join --------------

    fn on_join(
        &mut self,
        member: MemberId,
        identity: Identity,
        name: &str,
        now: i64,
        out: &mut Out,
    ) {
        let reject = |reason, out: &mut Out| {
            out.push(Effect::SendTo(member, Message::Reject { reason }));
            out.push(Effect::Disconnect(member));
        };
        if self.bans.contains(&identity) {
            return reject(RejectReason::Banned, out);
        }
        // A reconnecting identity gets its seat back.
        let existing = self
            .members
            .iter()
            .find(|(_, e)| e.identity == identity)
            .map(|(id, _)| *id);
        let id = match existing {
            Some(id) => {
                if let Some(e) = self.members.get_mut(&id) {
                    e.connected = true;
                    e.left_at = None;
                    e.status = None;
                }
                id
            }
            None => {
                if self.locked {
                    return reject(RejectReason::Locked, out);
                }
                if self.connected_count() >= self.cfg.max_members {
                    return reject(RejectReason::Full, out);
                }
                self.next_member_id = self.next_member_id.max(member.0 + 1);
                self.members.insert(
                    member,
                    MemberEntry {
                        identity,
                        name: sanitize_name(name),
                        perms: self.default_perms,
                        status: None,
                        connected: true,
                        left_at: None,
                    },
                );
                member
            }
        };
        let snapshot = Snapshot {
            your_id: id,
            host_identity: self.cfg.identity,
            descriptor: self.descriptor.clone(),
            playback: self.local.room,
            tracks: self.tracks.clone(),
            members: self.member_infos(),
            your_permissions: self.members.get(&id).map(|e| e.perms).unwrap_or_default(),
            chat: self.chat.snapshot(),
            chat_state: self.chat_state_for(id),
        };
        out.push(Effect::SendTo(id, Message::Welcome(Box::new(snapshot))));
        let _ = now;
    }

    // ------------------------------------------------------------------ network -----------

    fn on_net(&mut self, clock: Clock, from: Sender, msg: Message, out: &mut Out) {
        let now = clock.mono_us;
        let known = self
            .members
            .get(&from.member)
            .is_some_and(|e| e.connected && e.identity == from.identity);
        if !known {
            out.push(Effect::ProtocolViolation {
                from: from.member,
                what: "message from unknown or disconnected member",
            });
            return;
        }
        let m = from.member;
        match msg {
            Message::TimePing { id, t0_us } => out.push(Effect::SendTo(
                m,
                Message::TimePong {
                    id,
                    t0_us,
                    t1_host_us: now,
                    t2_host_us: now,
                },
            )),
            Message::Request(r) => self.on_request(m, r, now, out),
            Message::Status(s) => {
                let prev = self.members.get(&m).and_then(|e| e.status);
                if let Some(e) = self.members.get_mut(&m) {
                    e.status = Some(s);
                }
                if self.gate.excused.contains(&m)
                    && self.gate.blocks(Some(&s), &self.local.room).is_none()
                {
                    self.gate.excused.remove(&m);
                }
                if s.media == MediaStatus::Declined
                    && prev.is_none_or(|p| p.media != MediaStatus::Declined)
                {
                    out.ui(UiEvent::Notice(Notice::MemberDeclinedMagnet { member: m }));
                }
                if s.buffering {
                    self.check_buffering(m, now, out);
                }
                self.try_resume(now, out);
            }
            Message::ChatSend {
                client_id,
                text,
                claimed_media_ms,
            } => self.submit_chat(clock, m, client_id, &text, claimed_media_ms, out),
            Message::FileRequest {
                external_id,
                offset,
            } => match self.tracks.external(external_id) {
                Some(e) if offset <= e.size => out.push(Effect::StartUpload {
                    to: m,
                    external_id,
                    offset,
                }),
                _ => out.push(Effect::ProtocolViolation {
                    from: m,
                    what: "file request outside the external manifest",
                }),
            },
            Message::FileHave { .. } => {}
            Message::FileCancel { external_id } => {
                out.push(Effect::CancelUpload { to: m, external_id })
            }
            // A deliberate leave frees the seat at once (no 20 s retention).
            Message::Bye => {
                self.members.remove(&m);
            }
            Message::Command(Command::Tracks(_)) => out.push(Effect::ProtocolViolation {
                from: m,
                what: "forged track command from a member",
            }),
            Message::Command(Command::ChatModeration(_)) => out.push(Effect::ProtocolViolation {
                from: m,
                what: "forged moderation command from a member",
            }),
            Message::Command(_) => out.push(Effect::ProtocolViolation {
                from: m,
                what: "host command from a member",
            }),
            Message::FileChunk { .. } => out.push(Effect::ProtocolViolation {
                from: m,
                what: "file chunk from a member",
            }),
            Message::FileWithheld { .. } => out.push(Effect::ProtocolViolation {
                from: m,
                what: "host-only file message from a member",
            }),
            Message::Hello { .. }
            | Message::Welcome(_)
            | Message::Reject { .. }
            | Message::TimePong { .. }
            | Message::Heartbeat { .. }
            | Message::Chat(_)
            | Message::ChatRejected { .. } => out.push(Effect::ProtocolViolation {
                from: m,
                what: "unexpected message from a member",
            }),
        }
    }

    fn perm(&self, who: MemberId, f: impl Fn(&Permissions) -> bool) -> bool {
        self.permissions_of(who).is_some_and(|p| f(&p))
    }

    fn on_request(&mut self, who: MemberId, r: Request, now: i64, out: &mut Out) {
        let deny = |s: &Self, out: &mut Out| {
            s.notify(
                who,
                Notice::RequestDenied {
                    reason: DenyReason::NoPermission,
                },
                out,
            )
        };
        let room = self.local.room;
        match r {
            Request::Play => {
                if !self.perm(who, |p| p.can_play_pause) {
                    return deny(self, out);
                }
                if room.paused {
                    self.request_play(now, out);
                }
            }
            Request::Pause => {
                if !self.perm(who, |p| p.can_play_pause) {
                    return deny(self, out);
                }
                let was_pending = self.gate.pending.take().is_some();
                if !room.paused {
                    let p = RoomPlayback {
                        position_ms: room.position_at(now),
                        anchor_host_us: now,
                        paused: true,
                        ..room
                    };
                    self.commit(p, now, out);
                }
                if was_pending {
                    self.publish_gate(false, Vec::new(), out);
                }
            }
            Request::Seek { position_ms } => {
                if !self.perm(who, |p| p.can_seek) {
                    return deny(self, out);
                }
                let mut pos = position_ms.max(0);
                if let Some(d) = self.local.duration_ms {
                    pos = pos.min(d.max(0));
                }
                let was_playing = !room.paused || self.gate.pending.is_some();
                let p = RoomPlayback {
                    position_ms: pos,
                    anchor_host_us: now,
                    paused: true,
                    ..room
                };
                self.commit(p, now, out);
                if was_playing {
                    // Ready gate after a seek: resume once everyone has seeked.
                    self.gate.pending = Some(Pending {
                        since_us: now,
                        ignore: HashSet::new(),
                    });
                    self.try_resume(now, out);
                }
            }
            Request::SetSpeed { speed } => {
                if !self.perm(who, |p| p.can_change_speed) {
                    return deny(self, out);
                }
                let speed = speed.clamp(250, 4000);
                let at = if room.paused {
                    now
                } else {
                    now + self.lead_us()
                };
                let p = RoomPlayback {
                    position_ms: room.position_at(at),
                    anchor_host_us: at,
                    speed,
                    ..room
                };
                self.commit(p, now, out);
            }
            Request::ChangeMedia {
                request_id,
                descriptor,
            } => {
                if !self.perm(who, |p| p.can_change_media) {
                    return deny(self, out);
                }
                if descriptor.validate().is_err() {
                    return self.notify(
                        who,
                        Notice::RequestDenied {
                            reason: DenyReason::Invalid,
                        },
                        out,
                    );
                }
                self.change_media(who, request_id, Some(descriptor), now, out);
            }
        }
    }

    fn change_media(
        &mut self,
        who: MemberId,
        request_id: u32,
        descriptor: Option<MediaDescriptor>,
        now: i64,
        out: &mut Out,
    ) {
        // Concurrent requests: the later one wins; the earlier requester gets a toast.
        if let Some((prev_who, prev_req, at)) = self.last_change
            && prev_who != who
            && now - at < CONCURRENT_CHANGE_US
        {
            self.notify(
                prev_who,
                Notice::MediaChangeSuperseded {
                    request_id: prev_req,
                    by: who,
                },
                out,
            );
        }
        self.last_change = Some((who, request_id, now));
        let media_epoch = MediaEpoch(self.local.room.media_epoch.0 + 1);
        let playback = RoomPlayback {
            media_epoch,
            epoch: Epoch(self.local.room.epoch.0 + 1),
            position_ms: 0,
            anchor_host_us: now,
            paused: true,
            speed: 1000,
        };
        let tracks = TrackState {
            track_epoch: TrackEpoch(self.tracks.track_epoch.0 + 1),
            ..TrackState::default()
        };
        self.descriptor = descriptor.clone();
        self.tracks = tracks.clone();
        self.gate.pending = None;
        // Statuses refer to the old media now.
        for e in self.members.values_mut() {
            e.status = None;
        }
        self.broadcast_cmd(
            Command::MediaChange(Box::new(MediaChange {
                descriptor: descriptor.clone(),
                playback,
                tracks: tracks.clone(),
                requested_by: who,
            })),
            out,
        );
        self.local
            .on_media_change(descriptor, playback, tracks, Some(who), Some(now), now, out);
    }

    fn submit_chat(
        &mut self,
        clock: Clock,
        who: MemberId,
        client_id: u32,
        text: &str,
        claimed: Option<MediaMs>,
        out: &mut Out,
    ) {
        let now = clock.mono_us;
        let is_host = who == self.me;
        let (name, identity) = if is_host {
            (self.cfg.display_name.clone(), self.cfg.identity)
        } else {
            match self.members.get(&who) {
                Some(e) => (e.name.clone(), e.identity),
                None => return,
            }
        };
        let room_pos = self
            .descriptor
            .as_ref()
            .map(|_| self.local.room.position_at(now));
        let res = self.chat.submit(
            Submit {
                sender: who,
                sender_identity: identity,
                display_name: &name,
                is_host,
                mono_us: now,
                wall_ms: clock.wall_ms,
                media_epoch: self.local.room.media_epoch,
                room_pos_ms: room_pos,
                claimed_ms: claimed,
                client_id,
            },
            text,
        );
        match res {
            Ok(msg) => {
                if !self.local_mutes.contains(&who) {
                    out.ui(UiEvent::ChatMessage(msg.clone()));
                }
                // Everyone sees all chat (muted members and members while chat is off too).
                out.push(Effect::Broadcast(Message::Chat(msg)));
            }
            Err(reason) => {
                if is_host {
                    out.ui(UiEvent::ChatRejected { client_id, reason });
                } else {
                    out.push(Effect::SendTo(
                        who,
                        Message::ChatRejected { client_id, reason },
                    ));
                }
            }
        }
    }

    // ------------------------------------------------------------------ host UI ------------

    fn set_tracks(&mut self, f: impl FnOnce(&mut TrackState), out: &mut Out) {
        let mut t = self.tracks.clone();
        f(&mut t);
        t.track_epoch = TrackEpoch(self.tracks.track_epoch.0 + 1);
        if t.validate().is_err() {
            out.ui(UiEvent::Notice(Notice::RequestDenied {
                reason: DenyReason::Invalid,
            }));
            return;
        }
        self.tracks = t.clone();
        self.broadcast_cmd(Command::Tracks(t.clone()), out);
        self.local.tracks.set_desired(t, out);
    }

    fn on_ui(&mut self, clock: Clock, a: UiAction, out: &mut Out) {
        let now = clock.mono_us;
        let me = self.me;
        match a {
            UiAction::Play => self.on_request(me, Request::Play, now, out),
            UiAction::Pause => self.on_request(me, Request::Pause, now, out),
            UiAction::Seek(p) => self.on_request(me, Request::Seek { position_ms: p }, now, out),
            UiAction::SetSpeed(s) => self.on_request(me, Request::SetSpeed { speed: s }, now, out),
            UiAction::ChangeMedia {
                descriptor,
                own_file,
            } => {
                if descriptor.validate().is_err() {
                    return out.ui(UiEvent::Notice(Notice::RequestDenied {
                        reason: DenyReason::Invalid,
                    }));
                }
                if let (true, MediaDescriptor::LocalFile(l)) = (own_file, &descriptor) {
                    self.local.set_own_pending(Some(l.identity));
                }
                self.change_media(me, 0, Some(descriptor), now, out);
            }
            UiAction::ProceedWithMismatch => self.local.proceed_with_mismatch(out),
            UiAction::AcceptMagnet => self.local.accept_magnet(out),
            UiAction::DeclineMagnet => self.local.decline_magnet(),
            UiAction::SendChat { client_id, text } => {
                self.submit_chat(clock, me, client_id, &text, None, out)
            }
            UiAction::LocalMute { member, muted } => {
                if muted {
                    self.local_mutes.insert(member);
                } else {
                    self.local_mutes.remove(&member);
                }
            }
            UiAction::CancelExternalDownload(_) => {}
            UiAction::SelectAudio(sel) => self.set_tracks(|t| t.audio = sel, out),
            UiAction::SelectSub(sel) => self.set_tracks(|t| t.subtitle = sel, out),
            UiAction::SetSubDelay(ms) => self.set_tracks(|t| t.sub_delay_ms = ms, out),
            UiAction::SetAudioDelay(ms) => self.set_tracks(|t| t.audio_delay_ms = ms, out),
            UiAction::AddExternal {
                kind,
                display_name,
                format,
                size,
                blake3,
            } => {
                let limit = match kind {
                    ExternalKind::Subtitle => self.cfg.max_external_sub,
                    ExternalKind::Audio => self.cfg.max_external_audio,
                };
                if limit.is_some_and(|l| size > l) || size == 0 {
                    return out.ui(UiEvent::Notice(Notice::RequestDenied {
                        reason: DenyReason::Invalid,
                    }));
                }
                let entry = ExternalEntry {
                    external_id: ExternalId(self.next_external_id),
                    kind,
                    display_name,
                    format: format.to_ascii_lowercase(),
                    size,
                    blake3,
                    added_at_epoch: TrackEpoch(self.tracks.track_epoch.0 + 1),
                };
                if entry.validate().is_err() || self.tracks.manifest.len() >= MAX_MANIFEST_ENTRIES {
                    return out.ui(UiEvent::Notice(Notice::RequestDenied {
                        reason: DenyReason::Invalid,
                    }));
                }
                self.next_external_id += 1;
                let id = entry.external_id;
                self.set_tracks(|t| t.manifest.push(entry), out);
                // The host already holds the file.
                self.local.tracks.external_ready(id, out);
            }
            UiAction::SetPermissions {
                member,
                permissions,
            } => {
                if let Some(e) = self.members.get_mut(&member) {
                    e.perms = permissions;
                    out.push(Effect::SendTo(
                        member,
                        Message::Command(Command::Permissions {
                            member,
                            permissions,
                        }),
                    ));
                }
            }
            UiAction::SetDefaultPermissions(p) => self.default_perms = p,
            UiAction::ApplyPreset { preset, custom } => {
                let p = Permissions::for_preset(preset, custom);
                self.default_perms = p;
                let ids: Vec<MemberId> = self.members.keys().copied().collect();
                for id in ids {
                    if let Some(e) = self.members.get_mut(&id) {
                        e.perms = p;
                    }
                    out.push(Effect::SendTo(
                        id,
                        Message::Command(Command::Permissions {
                            member: id,
                            permissions: p,
                        }),
                    ));
                }
            }
            UiAction::Kick(m) | UiAction::Ban(m) => {
                let banned = matches!(a, UiAction::Ban(_));
                if let Some(e) = self.members.remove(&m) {
                    if banned && !self.bans.contains(&e.identity) {
                        self.bans.push(e.identity);
                    }
                    out.push(Effect::SendTo(
                        m,
                        Message::Command(Command::Kicked { banned }),
                    ));
                    out.push(Effect::Disconnect(m));
                }
                self.try_resume(now, out);
            }
            UiAction::Lock(l) => self.locked = l,
            UiAction::MakeHost(m) => self.make_host(m, out),
            UiAction::CloseRoom => {
                self.broadcast_cmd(Command::RoomClosed, out);
                out.ui(UiEvent::RoomClosed);
                self.closed = true;
            }
            UiAction::Moderate(m) => match m {
                Moderation::Mute(who) | Moderation::Unmute(who) => {
                    let muted = matches!(m, Moderation::Mute(_));
                    let Some(identity) = self.members.get(&who).map(|e| e.identity) else {
                        return;
                    };
                    if self.chat.set_muted(identity, muted) {
                        // The affected member learns it at once; everyone sees the flag in the
                        // member list.
                        out.push(Effect::SendTo(
                            who,
                            Message::Command(Command::ChatState(self.chat_state_for(who))),
                        ));
                    }
                }
                Moderation::Delete(id) => {
                    self.chat.moderate(m);
                    out.ui(UiEvent::ChatDeleted(id));
                    self.broadcast_cmd(Command::ChatModeration(m), out);
                }
                Moderation::Clear => {
                    self.chat.moderate(m);
                    out.ui(UiEvent::ChatCleared);
                    self.broadcast_cmd(Command::ChatModeration(m), out);
                }
            },
            UiAction::SetChatConfig(c) => {
                self.chat.config = c;
                out.ui(UiEvent::ChatState(ChatState {
                    config: c,
                    muted: false,
                }));
                // Each member gets its own state (the global switch plus its mute flag).
                let ids: Vec<MemberId> = self.members.keys().copied().collect();
                for m in ids {
                    out.push(Effect::SendTo(
                        m,
                        Message::Command(Command::ChatState(self.chat_state_for(m))),
                    ));
                }
            }
            UiAction::SetWaitForEveryone(w) => {
                self.gate.wait_for_everyone = w;
                self.try_resume(now, out);
            }
            UiAction::ResumeWithoutWaiting => {
                let blockers: Vec<MemberId> = self.blockers().into_iter().map(|(m, _)| m).collect();
                if let Some(p) = self.gate.pending.as_mut() {
                    p.ignore.extend(blockers);
                }
                self.try_resume(now, out);
            }
            // Member-only actions.
            UiAction::TakeOverAsSuccessor | UiAction::ExpectNewHost => {}
        }
    }

    fn make_host(&mut self, m: MemberId, out: &mut Out) {
        let Some(target) = self.members.get(&m).filter(|e| e.connected) else {
            return;
        };
        let mut members = self.member_infos();
        // The former host becomes an ordinary member with the default permissions.
        for info in &mut members {
            info.is_host = info.id == m;
            if info.id == self.me {
                info.permissions = self.default_perms;
            }
            if info.id == m {
                info.permissions = Permissions::ALL;
            }
        }
        let mut identities: Vec<(MemberId, Identity)> = self
            .members
            .iter()
            .map(|(id, e)| (*id, e.identity))
            .collect();
        identities.push((self.me, self.cfg.identity));
        let h = HostHandover {
            new_host: m,
            new_host_identity: target.identity,
            descriptor: self.descriptor.clone(),
            playback: self.local.room,
            tracks: self.tracks.clone(),
            members,
            identities,
            default_permissions: self.default_perms,
            chat: self.chat.history(),
            chat_config: self.chat.config,
            muted: self.chat.muted(),
            chat_next_seq: self.chat.next_seq(),
            bans: self.bans.clone(),
            locked: self.locked,
            next_external_id: self.next_external_id,
        };
        self.broadcast_cmd(Command::HostTransfer(Box::new(h.clone())), out);
        out.ui(UiEvent::HostChanged { new_host: m });
        self.handover = Some(h);
    }
}

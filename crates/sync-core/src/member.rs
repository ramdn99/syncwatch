//! A member: forwards the user's requests to the host and applies the host's authoritative
//! commands. Commands, track changes and moderation are accepted ONLY from the current host's
//! identity; anything else is a protocol violation.

use crate::clock_sync::ClockSync;
use crate::local::Local;
use crate::timing::*;
use crate::transfer::{ChunkError, Downloads};
use crate::types::*;
use std::collections::{HashMap, HashSet};
use syncwatch_protocol::*;

#[derive(Debug)]
pub struct MemberMachine {
    pub identity: Identity,
    pub display_name: String,
    pub me: Option<MemberId>,
    pub host_identity: Option<Identity>,
    pub local: Local,
    pub clock: ClockSync,
    pub perms: Permissions,
    pub members: Vec<MemberInfo>,
    pub chat_config: ChatConfig,
    /// My chat state as the host last told me (authoritative for my UI).
    pub chat_state: ChatState,
    /// The room rules the host keeps us updated with while we are its designated successor.
    pub succession: Option<SuccessionState>,
    downloads: Downloads,
    local_mutes: HashSet<MemberId>,
    next_ping_us: i64,
    pings_sent: u32,
    ping_id: u32,
    outstanding: HashMap<u32, i64>,
    next_request_id: u32,
    last_status: Option<MemberStatus>,
    handover: Option<HostHandover>,
    /// Highest chat id/seq seen (a successor host continues above it, so ids never collide).
    last_chat_seq: u64,
    pub closed: bool,
}

impl MemberMachine {
    pub fn new(identity: Identity, display_name: &str) -> Self {
        Self {
            identity,
            display_name: crate::chat::sanitize_name(display_name),
            me: None,
            host_identity: None,
            local: Local::new(MemberId(u32::MAX), 0),
            clock: ClockSync::default(),
            perms: Permissions::default(),
            members: Vec::new(),
            chat_config: ChatConfig::default(),
            chat_state: ChatState::default(),
            succession: None,
            downloads: Downloads::default(),
            local_mutes: HashSet::new(),
            next_ping_us: i64::MIN,
            pings_sent: 0,
            ping_id: 0,
            outstanding: HashMap::new(),
            next_request_id: 1,
            last_status: None,
            handover: None,
            last_chat_seq: 0,
            closed: false,
        }
    }

    /// Host succession: the room state as this member knows it, to continue as the host.
    /// The room rules (seats with identities and custom permissions, default permissions,
    /// mutes, bans, lock) come from the [`SuccessionState`] the host kept us updated with;
    /// the other seats wait for their owners to reconnect. Not carried: the chat history
    /// (members keep what they already show). See ADR 0007.
    fn succession_handover(&self) -> Option<HostHandover> {
        let me = self.me?;
        let mut mine = self.members.iter().find(|i| i.id == me).cloned()?;
        mine.is_host = true;
        mine.permissions = Permissions::ALL;
        let rules = self.succession.clone().unwrap_or_default();
        let old_host = self.members.iter().find(|i| i.is_host).map(|i| i.id);
        let mut members = vec![mine];
        let mut identities = Vec::new();
        for s in rules
            .seats
            .iter()
            .filter(|s| s.id != me && Some(s.id) != old_host)
        {
            let known = self.members.iter().find(|i| i.id == s.id);
            members.push(MemberInfo {
                id: s.id,
                display_name: s.display_name.clone(),
                is_host: false,
                permissions: s.permissions,
                media: known.map(|i| i.media).unwrap_or(MediaStatus::NoMedia),
                buffering: false,
                tracks: known.map(|i| i.tracks).unwrap_or(TrackSyncStatus::InSync),
                chat_muted: rules.muted.contains(&s.identity),
                connected: false,
                torrent: known.and_then(|i| i.torrent),
            });
            identities.push((s.id, s.identity));
        }
        let next_external_id = self
            .local
            .tracks
            .desired
            .manifest
            .iter()
            .map(|e| e.external_id.0 + 1)
            .max()
            .unwrap_or(1);
        Some(HostHandover {
            new_host: me,
            new_host_identity: self.identity,
            descriptor: self.local.descriptor.clone(),
            playback: self.local.room,
            tracks: self.local.tracks.desired.clone(),
            members,
            identities,
            default_permissions: if self.succession.is_some() {
                rules.default_permissions
            } else {
                Permissions::default()
            },
            chat: Vec::new(),
            chat_config: self.chat_config,
            chat_next_seq: self.last_chat_seq + 1,
            muted: rules.muted,
            bans: rules.bans,
            locked: rules.locked,
            next_external_id,
        })
    }

    /// The former host continues as a member of the new host.
    pub fn from_former_host(
        identity: Identity,
        display_name: &str,
        local: Local,
        h: &HostHandover,
    ) -> Self {
        let mut m = Self::new(identity, display_name);
        m.me = Some(local.me);
        m.host_identity = Some(h.new_host_identity);
        m.perms = h
            .members
            .iter()
            .find(|i| i.id == local.me)
            .map(|i| i.permissions)
            .unwrap_or_default();
        m.members = h.members.clone();
        m.chat_config = h.chat_config;
        m.local = local;
        m.next_ping_us = i64::MIN;
        m
    }

    /// First message on a new connection (authentication is added in Milestone 5).
    pub fn hello(&self) -> Message {
        Message::Hello {
            protocol: PROTOCOL_VERSION,
            display_name: self.display_name.clone(),
            identity: self.identity,
        }
    }

    pub fn take_handover(&mut self) -> Option<HostHandover> {
        self.handover.take()
    }

    fn host_now(&self, mono: i64) -> Option<i64> {
        self.clock.host_now(mono)
    }

    pub fn next_deadline(&self) -> Option<i64> {
        self.local.next_deadline(|h| self.clock.local_for_host(h))
    }

    pub fn handle(&mut self, clock: Clock, ev: Event) -> Vec<Effect> {
        let mut out = Out::default();
        if self.closed {
            return out.into_vec();
        }
        let mono = clock.mono_us;
        match ev {
            Event::Tick => self.on_tick(mono, &mut out),
            Event::Ui(a) => self.on_ui(mono, a, &mut out),
            Event::Net { from, msg } => self.on_net(mono, from, msg, &mut out),
            Event::Player(obs) => {
                let hn = self.host_now(mono);
                self.local.on_player(obs, hn, mono, &mut out);
            }
            Event::LocalFile(e) => self.local.on_local_file(e, &mut out),
            Event::Torrent(e) => self.local.on_torrent(e, &mut out),
            Event::ExternalCached(id) => {
                let manifest = self.local.tracks.desired.manifest.clone();
                self.downloads
                    .on_cached(id, &manifest, &mut self.local.tracks, &mut out);
            }
            Event::PeerJoined { .. } | Event::PeerLeft { .. } => {}
        }
        self.report_status(&mut out);
        out.into_vec()
    }

    fn report_status(&mut self, out: &mut Out) {
        if self.me.is_none() || self.closed {
            return;
        }
        let track_override = self.downloads.progress_permille().and_then(|p| {
            (self.local.tracks.status == TrackSyncStatus::Applying)
                .then_some(TrackSyncStatus::Downloading { permille: p })
        });
        let rtt = self.clock.rtt_us().map(|r| (r / 1000) as u32);
        let s = self.local.status(rtt, track_override);
        // Don't spam the host with every RTT wiggle: compare without rtt.
        let changed = self.last_status.is_none_or(|l| {
            MemberStatus { rtt_ms: None, ..l } != MemberStatus { rtt_ms: None, ..s }
        });
        if changed {
            self.last_status = Some(s);
            out.push(Effect::SendToHost(Message::Status(s)));
        }
    }

    fn on_tick(&mut self, mono: i64, out: &mut Out) {
        if self.me.is_some() && mono >= self.next_ping_us {
            self.ping_id = self.ping_id.wrapping_add(1);
            self.outstanding.insert(self.ping_id, mono);
            if self.outstanding.len() > 32 {
                let oldest = self.outstanding.keys().min().copied();
                if let Some(k) = oldest {
                    self.outstanding.remove(&k);
                }
            }
            out.push(Effect::SendToHost(Message::TimePing {
                id: self.ping_id,
                t0_us: mono,
            }));
            self.pings_sent += 1;
            self.next_ping_us = mono
                + if self.pings_sent < PING_BURST {
                    PING_BURST_US
                } else {
                    PING_US
                };
        }
        let hn = self.host_now(mono);
        self.local.tick(hn, mono, out);
    }

    fn on_ui(&mut self, mono: i64, a: UiAction, out: &mut Out) {
        let deny = |out: &mut Out| {
            out.ui(UiEvent::Notice(Notice::RequestDenied {
                reason: DenyReason::NoPermission,
            }))
        };
        let p = self.perms;
        let req = |r: Request, allowed: bool, out: &mut Out| {
            if allowed {
                out.push(Effect::SendToHost(Message::Request(r)));
            } else {
                deny(out);
            }
        };
        match a {
            UiAction::Play => req(Request::Play, p.can_play_pause, out),
            UiAction::Pause => req(Request::Pause, p.can_play_pause, out),
            UiAction::Seek(pos) => req(Request::Seek { position_ms: pos }, p.can_seek, out),
            UiAction::SetSpeed(s) => req(Request::SetSpeed { speed: s }, p.can_change_speed, out),
            UiAction::ChangeMedia {
                descriptor,
                own_file,
            } => {
                if !p.can_change_media {
                    return deny(out);
                }
                if let (true, MediaDescriptor::LocalFile(l)) = (own_file, &descriptor) {
                    self.local.set_own_pending(Some(l.identity));
                }
                let request_id = self.next_request_id;
                self.next_request_id += 1;
                out.push(Effect::SendToHost(Message::Request(Request::ChangeMedia {
                    request_id,
                    descriptor,
                })));
            }
            UiAction::ProceedWithMismatch => self.local.proceed_with_mismatch(out),
            UiAction::AcceptMagnet => self.local.accept_magnet(out),
            UiAction::DeclineMagnet => self.local.decline_magnet(),
            UiAction::SendChat { client_id, text } => {
                let claimed = self.local.position_estimate(mono);
                out.push(Effect::SendToHost(Message::ChatSend {
                    client_id,
                    text,
                    claimed_media_ms: claimed,
                }));
            }
            UiAction::LocalMute { member, muted } => {
                if muted {
                    self.local_mutes.insert(member);
                } else {
                    self.local_mutes.remove(&member);
                }
            }
            UiAction::CancelExternalDownload(id) => {
                let manifest = self.local.tracks.desired.manifest.clone();
                self.downloads
                    .cancel(id, &manifest, &mut self.local.tracks, out);
            }
            // Host-only actions: members have no way to perform them.
            UiAction::SelectAudio(_)
            | UiAction::SelectSub(_)
            | UiAction::SetSubDelay(_)
            | UiAction::SetAudioDelay(_)
            | UiAction::AddExternal { .. }
            | UiAction::SetPermissions { .. }
            | UiAction::SetDefaultPermissions(_)
            | UiAction::ApplyPreset { .. }
            | UiAction::Kick(_)
            | UiAction::Ban(_)
            | UiAction::Lock(_)
            | UiAction::MakeHost(_)
            | UiAction::CloseRoom
            | UiAction::Moderate(_)
            | UiAction::SetChatConfig(_)
            | UiAction::SetWaitForEveryone(_)
            | UiAction::ResumeWithoutWaiting => deny(out),
            UiAction::TakeOverAsSuccessor => {
                if let Some(h) = self.succession_handover() {
                    out.ui(UiEvent::BecameHost);
                    self.handover = Some(h);
                }
            }
            UiAction::ExpectNewHost => {
                // A different host with its own clock: forget the old pins and resync.
                self.host_identity = None;
                self.clock = ClockSync::default();
                self.outstanding.clear();
                self.pings_sent = 0;
                self.next_ping_us = i64::MIN;
            }
        }
    }

    fn on_net(&mut self, mono: i64, from: Sender, msg: Message, out: &mut Out) {
        // Before Welcome the only acceptable messages are Welcome / Reject.
        let from_host = self.host_identity == Some(from.identity);
        match msg {
            Message::Welcome(s) => {
                // After a host succession (host_identity cleared) a new host may welcome us.
                if self.me.is_some() && self.host_identity.is_some() && !from_host {
                    return out.push(Effect::ProtocolViolation {
                        from: from.member,
                        what: "welcome from a non-host",
                    });
                }
                if s.host_identity != from.identity {
                    return out.push(Effect::ProtocolViolation {
                        from: from.member,
                        what: "welcome identity mismatch",
                    });
                }
                self.on_welcome(*s, mono, out);
            }
            Message::Reject { reason } => {
                if self.me.is_none() || from_host {
                    out.ui(UiEvent::Notice(Notice::RequestDenied {
                        reason: DenyReason::Invalid,
                    }));
                    out.ui(UiEvent::Kicked {
                        banned: reason == RejectReason::Banned,
                    });
                    self.closed = true;
                }
            }
            // Before Welcome nothing else is meaningful. An unreliable heartbeat can legitimately
            // overtake the reliable Welcome (no ordering across channels): ignore quietly.
            _ if self.me.is_none() => {}
            _ if !from_host => out.push(Effect::ProtocolViolation {
                from: from.member,
                what: "message not from the room host",
            }),
            Message::TimePong {
                id,
                t0_us,
                t1_host_us,
                t2_host_us,
            } => {
                if self.outstanding.remove(&id) == Some(t0_us) {
                    let had = self.clock.is_precise();
                    self.clock.add_pong(t0_us, t1_host_us, t2_host_us, mono);
                    if !had {
                        let hn = self.host_now(mono);
                        self.local.tick(hn, mono, out);
                    }
                }
            }
            Message::Heartbeat {
                playback,
                host_now_us,
            } => {
                self.clock.add_coarse(host_now_us, mono);
                let hn = self.host_now(mono);
                self.local.on_heartbeat(playback, hn, mono, out);
            }
            Message::Command(c) => self.on_command(mono, c, out),
            Message::Chat(m) => {
                self.last_chat_seq = self.last_chat_seq.max(m.seq).max(m.id.0);
                if !self.local_mutes.contains(&m.sender) {
                    out.ui(UiEvent::ChatMessage(m));
                }
            }
            Message::ChatRejected { client_id, reason } => {
                // A rejection also corrects my chat state (normal disabled state, not an error).
                let corrected = match reason {
                    ChatRejectReason::Muted => Some(ChatState {
                        muted: true,
                        ..self.chat_state
                    }),
                    ChatRejectReason::ChatDisabled => Some(ChatState {
                        config: ChatConfig {
                            enabled: false,
                            ..self.chat_state.config
                        },
                        ..self.chat_state
                    }),
                    _ => None,
                };
                if let Some(s) = corrected.filter(|s| *s != self.chat_state) {
                    self.chat_state = s;
                    out.ui(UiEvent::ChatState(s));
                }
                out.ui(UiEvent::ChatRejected { client_id, reason })
            }
            Message::FileChunk {
                external_id,
                offset,
                data,
            } => {
                let manifest = self.local.tracks.desired.manifest.clone();
                if let Err(e) = self.downloads.on_chunk(
                    external_id,
                    offset,
                    data,
                    &manifest,
                    &mut self.local.tracks,
                    out,
                ) {
                    out.push(Effect::ProtocolViolation {
                        from: from.member,
                        what: match e {
                            ChunkError::NotRequested => "file chunk outside the external manifest",
                            ChunkError::OutOfBounds => "file chunk out of bounds",
                        },
                    });
                }
            }
            Message::FileWithheld { external_id } => {
                let manifest = self.local.tracks.desired.manifest.clone();
                self.downloads
                    .withheld(external_id, &manifest, &mut self.local.tracks, out);
            }
            Message::Hello { .. }
            | Message::Bye
            | Message::TimePing { .. }
            | Message::Request(_)
            | Message::Status(_)
            | Message::ChatSend { .. }
            | Message::FileRequest { .. }
            | Message::FileHave { .. }
            | Message::FileCancel { .. } => out.push(Effect::ProtocolViolation {
                from: from.member,
                what: "member-only message from the host",
            }),
        }
    }

    fn on_welcome(&mut self, s: Snapshot, mono: i64, out: &mut Out) {
        // Same media = a reconnect (also to a successor host, which may give us a new seat):
        // keep the loaded file and the downloads.
        let reconnect = self.me.is_some() && self.local.descriptor == s.descriptor;
        self.me = Some(s.your_id);
        self.local.me = s.your_id;
        self.host_identity = Some(s.host_identity);
        self.perms = s.your_permissions;
        self.members = s.members.clone();
        self.chat_config = s.chat_state.config;
        self.chat_state = s.chat_state;
        self.last_status = None;
        self.next_ping_us = mono;
        self.pings_sent = 0;
        out.ui(UiEvent::Joined { me: s.your_id });
        out.ui(UiEvent::Members(s.members));
        out.ui(UiEvent::Permissions(s.your_permissions));
        out.ui(UiEvent::ChatState(s.chat_state));
        for m in s.chat {
            self.last_chat_seq = self.last_chat_seq.max(m.seq).max(m.id.0);
            if !self.local_mutes.contains(&m.sender) {
                out.ui(UiEvent::ChatMessage(m));
            }
        }
        let hn = self.host_now(mono);
        if reconnect {
            // Same media: keep the file; take the fresh state and resume any download.
            self.local.tracks.set_desired(s.tracks.clone(), out);
            self.local.apply_room(s.playback, hn, mono, out);
            self.downloads.resume(out);
        } else {
            // Late joiner: start paused ("Joining..."), then load/verify and follow.
            out.ui(UiEvent::Joining);
            self.downloads.reset(out);
            self.local.on_media_change(
                s.descriptor,
                s.playback,
                s.tracks.clone(),
                None,
                hn,
                mono,
                out,
            );
        }
        self.downloads
            .on_manifest(&s.tracks.manifest, &mut self.local.tracks, out);
    }

    fn on_command(&mut self, mono: i64, c: Command, out: &mut Out) {
        let hn = self.host_now(mono);
        match c {
            Command::Playback(p) => self.local.apply_room(p, hn, mono, out),
            Command::MediaChange(mc) => {
                let MediaChange {
                    descriptor,
                    playback,
                    tracks,
                    requested_by,
                } = *mc;
                self.downloads.reset(out);
                self.local.on_media_change(
                    descriptor,
                    playback,
                    tracks.clone(),
                    Some(requested_by),
                    hn,
                    mono,
                    out,
                );
                self.downloads
                    .on_manifest(&tracks.manifest, &mut self.local.tracks, out);
            }
            Command::Tracks(t) => {
                if t.track_epoch >= self.local.tracks.desired.track_epoch {
                    let manifest = t.manifest.clone();
                    self.local.tracks.set_desired(t, out);
                    self.downloads
                        .on_manifest(&manifest, &mut self.local.tracks, out);
                }
            }
            Command::Permissions {
                member,
                permissions,
            } => {
                if Some(member) == self.me {
                    self.perms = permissions;
                    out.ui(UiEvent::Permissions(permissions));
                }
            }
            Command::Members(v) => {
                self.members = v.clone();
                out.ui(UiEvent::Members(v));
            }
            Command::ChatModeration(m) => match m {
                Moderation::Delete(id) => out.ui(UiEvent::ChatDeleted(id)),
                Moderation::Clear => out.ui(UiEvent::ChatCleared),
                Moderation::Mute(_) | Moderation::Unmute(_) => {}
            },
            Command::ChatState(s) => {
                self.chat_config = s.config;
                self.chat_state = s;
                out.ui(UiEvent::ChatState(s));
            }
            Command::ReadyGate(g) => out.ui(UiEvent::Gate(g)),
            Command::Notice(n) => out.ui(UiEvent::Notice(n)),
            Command::Kicked { banned } => {
                out.ui(UiEvent::Kicked { banned });
                out.player(PlayerCmd::Stop);
                self.closed = true;
            }
            Command::HostTransfer(h) => {
                if Some(h.new_host) == self.me {
                    out.ui(UiEvent::BecameHost);
                    self.handover = Some(*h);
                } else {
                    self.host_identity = Some(h.new_host_identity);
                    self.members = h.members.clone();
                    // Different host = different clock: start over with a fresh ping burst.
                    self.clock = ClockSync::default();
                    self.outstanding.clear();
                    self.pings_sent = 0;
                    self.next_ping_us = mono;
                    out.ui(UiEvent::HostChanged {
                        new_host: h.new_host,
                    });
                }
            }
            Command::Succession(s) => self.succession = Some(*s),
            Command::RoomClosed => {
                out.ui(UiEvent::RoomClosed);
                out.player(PlayerCmd::Stop);
                self.closed = true;
            }
        }
    }
}

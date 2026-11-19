#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Room sessions end to end, in process: a fake signaling server and fake transport links
//! (offer/answer pairing, frames, closes, injected interruptions). Spec sections 6, 9 and 12.

use std::collections::{BTreeMap, HashMap, VecDeque};
use syncwatch_net::auth::IdentityKey;
use syncwatch_net::session::*;
use syncwatch_net::*;
use syncwatch_protocol::signaling::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::{Clock, UiAction, UiEvent};

const TICK_US: i64 = 50_000;

enum SigRole {
    None,
    Host(String),
    Member(String, SigPeer),
}

struct FakeRoom {
    host: Option<usize>,
    members: BTreeMap<SigPeer, usize>,
    next: SigPeer,
}

struct LinkSim {
    fp: String,
    remote: Option<(usize, PeerId)>,
}

enum Ev {
    Sig(usize, ServerMsg),
    Tr(usize, TransportEvent),
}

struct Node {
    s: RoomSession,
    ui: Vec<RoomUi>,
    ended: Option<EndReason>,
    role: SigRole,
    /// The process crashed: no more input, no clean leave.
    dead: bool,
}

struct World {
    now: i64,
    nodes: Vec<Node>,
    rooms: HashMap<String, FakeRoom>,
    links: HashMap<(usize, PeerId), LinkSim>,
    queue: VecDeque<Ev>,
    link_seq: u32,
    /// Fingerprint the HOST side will see for its member (MITM simulation).
    mitm: bool,
    next_room: u32,
    /// Room frames SENT by this node carry the previous protocol version (an older build).
    old_version: Option<usize>,
    /// Manual invites: offers and answers are real-looking SDP with an a=fingerprint line,
    /// followed by one candidate and end-of-candidates (as the bridge reports them).
    sdp_mode: bool,
}

fn cfg(name: &str, seed: u8) -> SessionConfig {
    SessionConfig {
        key: IdentityKey::from_secret([seed; 32]),
        display_name: name.into(),
        ice: IceConfig::default(),
        max_members: 12,
        gate_timeout_us: None,
        handshake: syncwatch_net::handshake::HandshakeMode::Signaling,
    }
}

impl World {
    fn new() -> Self {
        Self {
            now: 1_000_000,
            nodes: Vec::new(),
            rooms: HashMap::new(),
            links: HashMap::new(),
            queue: VecDeque::new(),
            link_seq: 0,
            mitm: false,
            next_room: 0,
            old_version: None,
            sdp_mode: false,
        }
    }

    fn clock(&self) -> Clock {
        Clock {
            mono_us: self.now,
            wall_ms: 1_700_000_000_000 + self.now / 1000,
        }
    }

    fn host(&mut self, name: &str, seed: u8, password: Option<&str>) -> usize {
        let (s, outs) =
            RoomSession::create(cfg(name, seed), password.map(str::to_owned), self.clock());
        self.push(s, outs)
    }

    fn join(&mut self, name: &str, seed: u8, password: Option<&str>) -> usize {
        let room = self.rooms.keys().next().cloned().unwrap();
        let (s, outs) = RoomSession::join(
            cfg(name, seed),
            room,
            password.map(str::to_owned),
            self.clock(),
        );
        let i = self.push(s, outs);
        self.settle();
        i
    }

    fn push(&mut self, s: RoomSession, outs: Vec<Output>) -> usize {
        self.nodes.push(Node {
            s,
            ui: Vec::new(),
            ended: None,
            role: SigRole::None,
            dead: false,
        });
        let i = self.nodes.len() - 1;
        self.exec(i, outs);
        self.settle();
        i
    }

    fn input(&mut self, i: usize, input: Input) {
        if self.nodes[i].dead {
            return;
        }
        let clock = self.clock();
        let outs = self.nodes[i].s.handle(clock, input);
        self.exec(i, outs);
    }

    fn act(&mut self, i: usize, a: Action) {
        self.input(i, Input::Action(a));
        self.settle();
    }

    fn ui_action(&mut self, i: usize, a: UiAction) {
        self.act(i, Action::Ui(a));
    }

    /// Delivers queued events until nothing is left.
    fn settle(&mut self) {
        let mut guard = 0;
        while let Some(ev) = self.queue.pop_front() {
            guard += 1;
            assert!(guard < 100_000, "event storm");
            match ev {
                Ev::Sig(i, m) => self.input(i, Input::Signaling(m)),
                Ev::Tr(i, e) => self.input(i, Input::Transport(e)),
            }
        }
    }

    /// Advances time, ticking every session every 50 ms.
    fn run_for(&mut self, us: i64) {
        let end = self.now + us;
        while self.now < end {
            self.now += TICK_US;
            for i in 0..self.nodes.len() {
                self.input(i, Input::Tick);
            }
            self.settle();
        }
    }

    fn exec(&mut self, i: usize, outs: Vec<Output>) {
        for o in outs {
            match o {
                Output::Signaling(m) => self.signaling(i, m),
                Output::CreatePeer { peer, role, .. } => {
                    self.close_sim(i, peer);
                    self.link_seq += 1;
                    let fp = format!("SHA-256 {:02X}:{:02X}:{:02X}", i, peer, self.link_seq);
                    self.links.insert(
                        (i, peer),
                        LinkSim {
                            fp: fp.clone(),
                            remote: None,
                        },
                    );
                    if role == IceRole::Offerer {
                        let sdp = self.sdp_of(&fp);
                        self.queue.push_back(Ev::Tr(
                            i,
                            TransportEvent::LocalSignal {
                                peer,
                                data: SignalData::Offer { sdp },
                            },
                        ));
                        self.gathered(i, peer);
                    }
                }
                Output::RemoteSignal { peer, data } => match data {
                    SignalData::Offer { .. } => {
                        let Some(l) = self.links.get(&(i, peer)) else {
                            continue;
                        };
                        let sdp = self.sdp_of(&l.fp.clone());
                        self.queue.push_back(Ev::Tr(
                            i,
                            TransportEvent::LocalSignal {
                                peer,
                                data: SignalData::Answer { sdp },
                            },
                        ));
                        self.gathered(i, peer);
                    }
                    SignalData::Answer { sdp } => {
                        let sdp = syncwatch_net::sdp::dtls_fingerprint(&sdp).unwrap_or(sdp);
                        // Pair with the answerer that owns this fingerprint.
                        let other = self
                            .links
                            .iter()
                            .find(|(_, l)| l.fp == sdp)
                            .map(|(k, _)| *k);
                        let Some((j, q)) = other else { continue };
                        let my_fp = self.links[&(i, peer)].fp.clone();
                        self.links.get_mut(&(i, peer)).unwrap().remote = Some((j, q));
                        self.links.get_mut(&(j, q)).unwrap().remote = Some((i, peer));
                        let host_sees = if self.mitm {
                            "SHA-256 66:66:66".to_owned()
                        } else {
                            my_fp.clone()
                        };
                        self.queue.push_back(Ev::Tr(
                            i,
                            TransportEvent::Open {
                                peer,
                                info: PeerInfo {
                                    local_fingerprint: Some(my_fp),
                                    remote_fingerprint: Some(sdp.clone()),
                                    relayed: false,
                                },
                            },
                        ));
                        self.queue.push_back(Ev::Tr(
                            j,
                            TransportEvent::Open {
                                peer: q,
                                info: PeerInfo {
                                    local_fingerprint: Some(sdp),
                                    remote_fingerprint: Some(host_sees),
                                    relayed: false,
                                },
                            },
                        ));
                    }
                    _ => {}
                },
                Output::Send {
                    peer,
                    lane,
                    mut data,
                } => {
                    // postcard: the envelope starts with the version as a one-byte varint.
                    if self.old_version == Some(i) && data.first() == Some(&FRAME_ROOM) {
                        data[1] = (PROTOCOL_VERSION - 1) as u8;
                    }
                    if let Some((j, q)) = self.links.get(&(i, peer)).and_then(|l| l.remote) {
                        self.queue.push_back(Ev::Tr(
                            j,
                            TransportEvent::Frame {
                                peer: q,
                                lane,
                                data,
                            },
                        ));
                    }
                }
                Output::ClosePeer(peer) => self.close_sim(i, peer),
                Output::Ui(u) => self.nodes[i].ui.push(u),
                Output::Ended(r) => {
                    self.nodes[i].ended = Some(r);
                }
                Output::Player(_) | Output::Core(_) => {}
            }
        }
    }

    fn sdp_of(&self, fp: &str) -> String {
        if self.sdp_mode {
            format!("v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\na=fingerprint:{fp}\r\n")
        } else {
            fp.to_owned()
        }
    }

    /// Manual invites: one host candidate and end-of-candidates after the description.
    fn gathered(&mut self, i: usize, peer: PeerId) {
        if !self.sdp_mode {
            return;
        }
        for data in [
            SignalData::Candidate {
                candidate: "candidate:1 1 udp 2122260223 192.0.2.1 50000 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
            },
            SignalData::EndOfCandidates,
        ] {
            self.queue
                .push_back(Ev::Tr(i, TransportEvent::LocalSignal { peer, data }));
        }
    }

    fn close_sim(&mut self, i: usize, peer: PeerId) {
        if let Some(l) = self.links.remove(&(i, peer))
            && let Some((j, q)) = l.remote
            && self
                .links
                .get(&(j, q))
                .is_some_and(|o| o.remote == Some((i, peer)))
        {
            self.links.remove(&(j, q));
            self.queue.push_back(Ev::Tr(
                j,
                TransportEvent::Closed {
                    peer: q,
                    reason: "remote closed".into(),
                },
            ));
        }
    }

    fn reply(&mut self, i: usize, m: ServerMsg) {
        self.queue.push_back(Ev::Sig(i, m));
    }

    fn signaling(&mut self, i: usize, m: ClientMsg) {
        match m {
            ClientMsg::Create { .. } => {
                self.next_room += 1;
                let room = format!("ROOM{:04}", self.next_room)
                    .replace('0', "A")
                    .replace('1', "B");
                self.rooms.insert(
                    room.clone(),
                    FakeRoom {
                        host: Some(i),
                        members: BTreeMap::new(),
                        next: 1,
                    },
                );
                self.nodes[i].role = SigRole::Host(room.clone());
                self.reply(
                    i,
                    ServerMsg::Created {
                        room,
                        token: "t".into(),
                    },
                );
            }
            ClientMsg::Join { room } => {
                let Some(r) = self.rooms.get_mut(&room) else {
                    return self.reply(
                        i,
                        ServerMsg::Error {
                            code: SigError::NoSuchRoom,
                        },
                    );
                };
                let p = r.next;
                r.next += 1;
                r.members.insert(p, i);
                let h = r.host;
                self.nodes[i].role = SigRole::Member(room.clone(), p);
                if let Some(h) = h {
                    self.reply(h, ServerMsg::PeerJoined { peer: p });
                }
                self.reply(i, ServerMsg::Joined { room, peer: p });
            }
            ClientMsg::Signal { to, data } => match &self.nodes[i].role {
                SigRole::Host(room) => {
                    if let Some(&j) = to.and_then(|p| self.rooms[room].members.get(&p)) {
                        self.reply(j, ServerMsg::Signal { from: None, data });
                    }
                }
                SigRole::Member(room, p) => {
                    let p = *p;
                    if let Some(h) = self.rooms[room].host {
                        self.reply(
                            h,
                            ServerMsg::Signal {
                                from: Some(p),
                                data,
                            },
                        );
                    }
                }
                SigRole::None => {}
            },
            ClientMsg::TransferHost { to } => {
                let SigRole::Host(room) = &self.nodes[i].role else {
                    return;
                };
                let room = room.clone();
                let r = self.rooms.get_mut(&room).unwrap();
                let Some(new_host) = r.members.remove(&to) else {
                    return;
                };
                let me = r.next;
                r.next += 1;
                r.members.insert(me, i);
                r.host = Some(new_host);
                let peers: Vec<SigPeer> = r.members.keys().copied().collect();
                let others: Vec<usize> = r
                    .members
                    .iter()
                    .filter(|(p, _)| **p != me)
                    .map(|(_, n)| *n)
                    .collect();
                self.nodes[new_host].role = SigRole::Host(room.clone());
                self.nodes[i].role = SigRole::Member(room, me);
                self.reply(
                    new_host,
                    ServerMsg::BecameHost {
                        token: "t2".into(),
                        peers,
                    },
                );
                for o in others {
                    self.reply(o, ServerMsg::HostChanged { you: None });
                }
                self.reply(i, ServerMsg::HostChanged { you: Some(me) });
            }
            ClientMsg::Leave => match std::mem::replace(&mut self.nodes[i].role, SigRole::None) {
                SigRole::Host(room) => {
                    if let Some(r) = self.rooms.remove(&room) {
                        for (_, j) in r.members {
                            self.reply(j, ServerMsg::RoomClosed);
                        }
                    }
                }
                SigRole::Member(room, p) => {
                    if let Some(r) = self.rooms.get_mut(&room) {
                        r.members.remove(&p);
                        if let Some(h) = r.host {
                            self.reply(h, ServerMsg::PeerLeft { peer: p });
                        }
                    }
                }
                SigRole::None => {}
            },
            ClientMsg::ClaimHost => {
                let SigRole::Member(room, me) = &self.nodes[i].role else {
                    return;
                };
                let (room, me) = (room.clone(), *me);
                let r = self.rooms.get_mut(&room).unwrap();
                if r.host.is_some() {
                    return self.reply(
                        i,
                        ServerMsg::Error {
                            code: SigError::NotHost,
                        },
                    );
                }
                r.members.remove(&me);
                r.host = Some(i);
                let peers: Vec<SigPeer> = r.members.keys().copied().collect();
                let others: Vec<usize> = r.members.values().copied().collect();
                self.nodes[i].role = SigRole::Host(room);
                self.reply(
                    i,
                    ServerMsg::BecameHost {
                        token: "t3".into(),
                        peers,
                    },
                );
                for o in others {
                    self.reply(o, ServerMsg::HostChanged { you: None });
                }
            }
            ClientMsg::Hello { .. } | ClientMsg::Resume { .. } | ClientMsg::Ping => {}
        }
    }

    /// The process of node i dies: its links break, signaling notices the closed socket
    /// (HostGone for a host), and it never says goodbye.
    fn crash(&mut self, i: usize) {
        self.nodes[i].dead = true;
        let mine: Vec<PeerId> = self
            .links
            .keys()
            .filter(|(n, _)| *n == i)
            .map(|(_, p)| *p)
            .collect();
        for p in mine {
            self.close_sim(i, p);
        }
        if let SigRole::Host(room) = std::mem::replace(&mut self.nodes[i].role, SigRole::None)
            && let Some(r) = self.rooms.get_mut(&room)
        {
            r.host = None;
            let members: Vec<usize> = r.members.values().copied().collect();
            for m in members {
                self.reply(m, ServerMsg::HostGone { grace_s: 30 });
            }
        }
        self.settle();
    }

    fn phases(&self, i: usize) -> Vec<Phase> {
        self.nodes[i]
            .ui
            .iter()
            .filter_map(|u| match u {
                RoomUi::Phase(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    fn words(&self, i: usize) -> Vec<(Option<MemberId>, [&'static str; 4])> {
        self.nodes[i]
            .ui
            .iter()
            .filter_map(|u| match u {
                RoomUi::Verification { member, words } => Some((*member, *words)),
                _ => None,
            })
            .collect()
    }

    fn members(&self, i: usize) -> Vec<MemberInfo> {
        self.nodes[i]
            .ui
            .iter()
            .rev()
            .find_map(|u| match u {
                RoomUi::Core(UiEvent::Members(v)) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn chats(&self, i: usize) -> Vec<String> {
        self.nodes[i]
            .ui
            .iter()
            .filter_map(|u| match u {
                RoomUi::Core(UiEvent::ChatMessage(c)) => Some(c.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn need_password(&self, i: usize) -> Vec<(bool, u32)> {
        self.nodes[i]
            .ui
            .iter()
            .filter_map(|u| match u {
                RoomUi::NeedPassword {
                    wrong,
                    retry_after_ms,
                } => Some((*wrong, *retry_after_ms)),
                _ => None,
            })
            .collect()
    }

    fn host_link_of(&self, i: usize) -> PeerId {
        *self
            .links
            .keys()
            .filter(|(n, _)| *n == i)
            .map(|(_, p)| p)
            .max()
            .unwrap()
    }

    fn in_room(&self, i: usize) -> bool {
        self.nodes[i].s.phase() == Phase::InRoom && self.nodes[i].ended.is_none()
    }
}

fn chat(w: &mut World, i: usize, text: &str) {
    w.ui_action(
        i,
        UiAction::SendChat {
            client_id: w.now as u32,
            text: text.into(),
        },
    );
    w.run_for(200_000);
}

/// Protocol v3 (Milestone 8): an older member joining a newer host is told the versions differ,
/// instead of waiting forever (the envelope version check used to drop the Hello and the
/// Reject alike).
#[test]
fn an_older_member_is_told_the_versions_differ() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let next = w.nodes.len();
    w.old_version = Some(next);
    let m = w.join("Old", 2, None);
    assert_eq!(m, next);
    w.run_for(2_000_000);
    assert_eq!(
        w.nodes[m].ended,
        Some(EndReason::Refused {
            reason: "version".into()
        })
    );
    assert!(!w.in_room(m));
    assert!(w.in_room(h));
}

/// The other way round: a member joining a host that runs another version ends with the same
/// clear reason.
#[test]
fn a_member_of_another_version_than_the_host_is_told_so() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    w.old_version = Some(h);
    let m = w.join("Mo", 2, None);
    w.run_for(2_000_000);
    assert_eq!(
        w.nodes[m].ended,
        Some(EndReason::Refused {
            reason: "version".into()
        })
    );
}

#[test]
fn passwordless_join_shows_the_same_four_words_on_both_sides() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(1_000_000);
    assert!(w.in_room(h) && w.in_room(m), "{:?}", w.phases(m));
    let (_, mw) = w.words(m)[0];
    let (who, hw) = w.words(h)[0];
    assert_eq!(mw, hw);
    assert!(who.is_some());
    let names: Vec<String> = w
        .members(m)
        .iter()
        .map(|i| i.display_name.clone())
        .collect();
    assert!(
        names.contains(&"Hana".to_owned()) && names.contains(&"Mo".to_owned()),
        "{names:?}"
    );
    chat(&mut w, m, "hello host");
    assert_eq!(w.chats(h), vec!["hello host"]);
    assert_eq!(w.chats(m), vec!["hello host"], "echo to the sender");
}

#[test]
fn password_room_joins_with_the_right_password_and_shows_no_words() {
    let mut w = World::new();
    let h = w.host("Hana", 1, Some("s3cret"));
    let m = w.join("Mo", 2, Some("s3cret"));
    w.run_for(500_000);
    assert!(w.in_room(m));
    assert!(w.words(m).is_empty() && w.words(h).is_empty());
}

#[test]
fn missing_then_wrong_then_right_password() {
    let mut w = World::new();
    let _h = w.host("Hana", 1, Some("s3cret"));
    let m = w.join("Mo", 2, None);
    assert_eq!(w.need_password(m), vec![(false, 0)], "asked for a password");
    w.act(m, Action::SubmitPassword("nope".into()));
    assert_eq!(
        w.need_password(m).last(),
        Some(&(true, 1000)),
        "wrong, retry after 1 s"
    );
    // Too early: refused (throttled), not counted as a guess.
    w.act(m, Action::SubmitPassword("s3cret".into()));
    assert!(!w.in_room(m));
    assert!(matches!(w.need_password(m).last(), Some((false, r)) if *r > 0));
    w.run_for(1_200_000);
    w.act(m, Action::SubmitPassword("s3cret".into()));
    w.run_for(300_000);
    assert!(w.in_room(m), "{:?}", w.phases(m));
}

#[test]
fn man_in_the_middle_is_detected_and_the_member_never_enters() {
    let mut w = World::new();
    w.mitm = true;
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(2_000_000);
    assert!(!w.in_room(m));
    assert!(w.nodes[m].ended.is_some(), "the member gives up");
    assert!(w.members(h).iter().all(|i| i.display_name != "Mo"));
}

#[test]
fn kick_and_ban() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(500_000);
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    w.ui_action(h, UiAction::Ban(mid));
    w.run_for(1_000_000);
    assert_eq!(w.nodes[m].ended, Some(EndReason::Kicked { banned: true }));
    // Same identity again: refused.
    let again = w.join("Mo", 2, None);
    w.run_for(1_000_000);
    assert!(!w.in_room(again));
    assert_eq!(
        w.nodes[again].ended,
        Some(EndReason::Refused {
            reason: "banned".into()
        })
    );
    // A different identity may still join.
    let other = w.join("Zed", 3, None);
    w.run_for(500_000);
    assert!(w.in_room(other));
}

#[test]
fn locked_room_refuses_new_members_but_not_the_seated_ones() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    w.ui_action(h, UiAction::Lock(true));
    let late = w.join("Late", 4, None);
    w.run_for(1_000_000);
    assert_eq!(
        w.nodes[late].ended,
        Some(EndReason::Refused {
            reason: "locked".into()
        })
    );
    assert!(w.in_room(m));
}

#[test]
fn a_muted_member_is_refused_by_the_host_and_stays_muted_after_reconnecting() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    w.ui_action(h, UiAction::Moderate(Moderation::Mute(mid)));
    w.run_for(300_000);
    let my_state = |w: &World| {
        w.nodes[m].ui.iter().rev().find_map(|u| match u {
            RoomUi::Core(UiEvent::ChatState(s)) => Some(*s),
            _ => None,
        })
    };
    assert!(
        my_state(&w).is_some_and(|s| s.muted),
        "pushed to the member at once"
    );
    chat(&mut w, m, "should not arrive");
    assert!(w.chats(h).is_empty());
    assert!(w.nodes[m].ui.iter().any(|u| matches!(
        u,
        RoomUi::Core(UiEvent::ChatRejected {
            reason: ChatRejectReason::Muted,
            ..
        })
    )));
    // The link drops and comes back: the reconnect snapshot still says muted.
    let link = w.host_link_of(m);
    w.input(
        m,
        Input::Transport(TransportEvent::Closed {
            peer: link,
            reason: "test".into(),
        }),
    );
    w.settle();
    w.run_for(1_000_000);
    assert!(w.in_room(m));
    assert!(
        my_state(&w).is_some_and(|s| s.muted),
        "still muted after the reconnect"
    );
    w.ui_action(h, UiAction::Moderate(Moderation::Unmute(mid)));
    w.run_for(300_000);
    assert!(my_state(&w).is_some_and(|s| !s.muted));
    chat(&mut w, m, "unmuted");
    assert_eq!(w.chats(h), vec!["unmuted"]);
}
#[test]
fn reconnecting_banner_only_for_real_interruptions_and_it_clears() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    // A long idle period: no banner anywhere.
    w.run_for(60_000_000);
    assert!(!w.phases(m).contains(&Phase::Reconnecting));
    assert!(!w.nodes[h].ui.iter().any(|u| matches!(
        u,
        RoomUi::Link {
            state: LinkState::Reconnecting,
            ..
        }
    )));
    // Traffic really stops: banner; it recovers: banner clears.
    let link = w.host_link_of(m);
    w.input(
        m,
        Input::Transport(TransportEvent::Interrupted {
            peer: link,
            diag: Diagnostics {
                silence_ms: 4000,
                frames_rx: 10,
                frames_tx: 10,
                ice_state: "connected".into(),
                connection_state: "connected".into(),
                cause: "no inbound traffic",
            },
        }),
    );
    assert_eq!(w.nodes[m].s.phase(), Phase::Reconnecting);
    w.input(
        m,
        Input::Transport(TransportEvent::Recovered { peer: link }),
    );
    assert_eq!(w.nodes[m].s.phase(), Phase::InRoom);
}

#[test]
fn restarted_link_reauthenticates_and_keeps_the_seat() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    let before = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    let link = w.host_link_of(m);
    w.input(
        m,
        Input::Transport(TransportEvent::RestartNeeded {
            peer: link,
            diag: Diagnostics {
                silence_ms: 10_000,
                frames_rx: 1,
                frames_tx: 1,
                ice_state: "disconnected".into(),
                connection_state: "disconnected".into(),
                cause: "silent link",
            },
        }),
    );
    w.settle();
    w.run_for(1_000_000);
    assert!(w.in_room(m), "{:?}", w.phases(m));
    let after: Vec<MemberInfo> = w.members(h).into_iter().filter(|i| !i.is_host).collect();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before, "same seat after the restart");
    assert!(after[0].connected);
    chat(&mut w, m, "still here");
    assert_eq!(w.chats(h), vec!["still here"]);
}

#[test]
fn host_transfer_over_the_network() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let a = w.join("Ana", 2, None);
    let b = w.join("Ben", 3, None);
    w.run_for(500_000);
    chat(&mut w, b, "before");
    let ana = w
        .members(h)
        .into_iter()
        .find(|i| i.display_name == "Ana")
        .unwrap()
        .id;
    w.ui_action(h, UiAction::MakeHost(ana));
    w.run_for(3_000_000);
    assert!(w.nodes[a].s.is_host(), "Ana is the host now");
    assert!(!w.nodes[h].s.is_host(), "Hana is a member now");
    assert!(w.in_room(b), "Ben reconnected to Ana: {:?}", w.phases(b));
    assert!(w.in_room(h));
    let hosts: Vec<String> = w
        .members(b)
        .into_iter()
        .filter(|i| i.is_host)
        .map(|i| i.display_name)
        .collect();
    assert_eq!(hosts, vec!["Ana".to_owned()]);
    chat(&mut w, b, "after");
    assert!(w.chats(a).contains(&"after".to_owned()), "{:?}", w.chats(a));
    assert!(
        w.chats(h).contains(&"after".to_owned()),
        "the old host still gets chat"
    );
    // History moved with the host role.
    assert!(w.chats(a).contains(&"before".to_owned()));
}

#[test]
fn password_moves_with_the_host_role() {
    let mut w = World::new();
    let h = w.host("Hana", 1, Some("pw"));
    let a = w.join("Ana", 2, Some("pw"));
    w.run_for(300_000);
    let ana = w
        .members(h)
        .into_iter()
        .find(|i| i.display_name == "Ana")
        .unwrap()
        .id;
    w.ui_action(h, UiAction::MakeHost(ana));
    w.run_for(2_000_000);
    assert!(w.nodes[a].s.is_host());
    // A newcomer must now know the password at Ana's.
    let wrong = w.join("Eve", 5, Some("guess"));
    assert!(w.need_password(wrong).iter().any(|(wr, _)| *wr));
    let ok = w.join("Cid", 6, Some("pw"));
    w.run_for(300_000);
    assert!(w.in_room(ok));
}

#[test]
fn host_closing_the_room_ends_every_session() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    w.act(h, Action::Leave);
    w.run_for(1_000_000);
    assert_eq!(w.nodes[h].ended, Some(EndReason::Left));
    assert_eq!(w.nodes[m].ended, Some(EndReason::RoomClosed));
}

#[test]
fn member_leaving_frees_the_seat() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    w.act(m, Action::Leave);
    w.run_for(1_000_000);
    assert_eq!(w.nodes[m].ended, Some(EndReason::Left));
    assert!(w.members(h).iter().all(|i| i.is_host), "{:?}", w.members(h));
}

#[test]
fn joining_a_local_file_room_shows_choose_my_file_once_without_any_permission() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    // Members get no control at all ("Only me"), so no can_change_media.
    w.ui_action(
        h,
        UiAction::ApplyPreset {
            preset: Preset::OnlyMe,
            custom: Permissions::NONE,
        },
    );
    w.ui_action(
        h,
        UiAction::ChangeMedia {
            descriptor: MediaDescriptor::LocalFile(LocalMedia {
                name: "movie.mkv".into(),
                identity: FileIdentity {
                    sample_blake3: [3; 32],
                    size: 1_234_567,
                    duration_ms: Some(1_700_000),
                },
            }),
            own_file: true,
        },
    );
    let m = w.join("Mo", 2, None);
    w.run_for(1_000_000);
    let panels = |w: &World| {
        w.nodes[m]
            .ui
            .iter()
            .filter(|u| matches!(u, RoomUi::Core(UiEvent::ShowMissingFilePanel { .. })))
            .count()
    };
    assert_eq!(panels(&w), 1, "shown automatically once on joining");
    assert!(w.nodes[m].ui.iter().any(|u| matches!(
        u,
        RoomUi::Core(UiEvent::ShowMissingFilePanel { name, size: 1_234_567, .. }) if name == "movie.mkv"
    )));
    // A reconnect brings a fresh snapshot but not the panel again.
    let link = w.host_link_of(m);
    w.input(
        m,
        Input::Transport(TransportEvent::Closed {
            peer: link,
            reason: "test".into(),
        }),
    );
    w.settle();
    w.run_for(1_000_000);
    assert!(w.in_room(m));
    assert_eq!(panels(&w), 1, "not shown again after a reconnect");
}

#[test]
fn a_typed_password_for_an_open_room_is_ignored_with_a_notice_once() {
    let mut w = World::new();
    let _h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, Some("random"));
    w.run_for(500_000);
    assert!(w.in_room(m), "joining still works");
    let notices = |w: &World| {
        w.nodes[m]
            .ui
            .iter()
            .filter(|u| matches!(u, RoomUi::PasswordIgnored))
            .count()
    };
    assert_eq!(notices(&w), 1);
    // A reconnect does not repeat it.
    let link = w.host_link_of(m);
    w.input(
        m,
        Input::Transport(TransportEvent::Closed {
            peer: link,
            reason: "test".into(),
        }),
    );
    w.settle();
    w.run_for(1_000_000);
    assert!(w.in_room(m));
    assert_eq!(notices(&w), 1);
    // No notice when no password was typed.
    let quiet = w.join("Q", 7, None);
    w.run_for(300_000);
    assert_eq!(
        w.nodes[quiet]
            .ui
            .iter()
            .filter(|u| matches!(u, RoomUi::PasswordIgnored))
            .count(),
        0
    );
}

#[test]
fn members_see_their_own_permissions_live() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    let mine = |w: &World| {
        w.nodes[m].ui.iter().rev().find_map(|u| match u {
            RoomUi::Core(UiEvent::Permissions(p)) => Some(*p),
            _ => None,
        })
    };
    assert_eq!(
        mine(&w),
        Some(Permissions::ALL),
        "initial permissions arrive with the welcome"
    );
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    let mut p = Permissions::ALL;
    p.can_seek = false;
    p.can_change_speed = false;
    w.ui_action(
        h,
        UiAction::SetPermissions {
            member: mid,
            permissions: p,
        },
    );
    w.run_for(200_000);
    assert_eq!(mine(&w), Some(p), "live update");
    w.ui_action(
        h,
        UiAction::ApplyPreset {
            preset: Preset::OnlyMe,
            custom: Permissions::NONE,
        },
    );
    w.run_for(200_000);
    assert_eq!(mine(&w), Some(Permissions::NONE));
}

#[test]
fn the_host_records_rejections_per_member_for_diagnostics() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    w.ui_action(
        h,
        UiAction::SetChatConfig(ChatConfig {
            enabled: false,
            slow_mode_secs: 0,
        }),
    );
    w.run_for(200_000);
    chat(&mut w, m, "nope");
    let rec: Vec<(Option<MemberId>, String)> = w.nodes[h]
        .ui
        .iter()
        .filter_map(|u| match u {
            RoomUi::Rejected { member, what } => Some((*member, what.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        rec,
        vec![(Some(mid), "chat message refused: ChatDisabled".to_owned())]
    );
}

#[test]
fn a_new_room_after_closing_one_starts_clean_for_host_and_member() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    w.ui_action(
        h,
        UiAction::ChangeMedia {
            descriptor: MediaDescriptor::LocalFile(LocalMedia {
                name: "old.mkv".into(),
                identity: FileIdentity {
                    sample_blake3: [4; 32],
                    size: 99,
                    duration_ms: Some(1000),
                },
            }),
            own_file: true,
        },
    );
    let m = w.join("Mo", 2, None);
    w.run_for(500_000);
    chat(&mut w, m, "from room one");
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    let mut p = Permissions::ALL;
    p.can_seek = false;
    w.ui_action(
        h,
        UiAction::SetPermissions {
            member: mid,
            permissions: p,
        },
    );
    w.ui_action(h, UiAction::Lock(true));
    w.run_for(300_000);
    w.act(h, Action::Leave);
    w.run_for(1_000_000);
    assert_eq!(w.nodes[m].ended, Some(EndReason::RoomClosed));

    // Same people, same app identities, a NEW room.
    let h2 = w.host("Hana", 1, None);
    let m2 = w.join("Mo", 2, None);
    w.run_for(1_000_000);
    assert!(w.in_room(m2), "the old lock did not carry over");
    let ui = &w.nodes[m2].ui;
    assert!(
        !ui.iter()
            .any(|u| matches!(u, RoomUi::Core(UiEvent::ShowMissingFilePanel { .. }))),
        "no media: no choose-my-file panel"
    );
    assert!(
        w.chats(m2).is_empty() && w.chats(h2).is_empty(),
        "no chat history"
    );
    let members = w.members(m2);
    assert!(
        members.iter().all(|i| i.media == MediaStatus::NoMedia),
        "{members:?}"
    );
    let mine = ui.iter().rev().find_map(|u| match u {
        RoomUi::Core(UiEvent::Permissions(p)) => Some(*p),
        _ => None,
    });
    assert_eq!(
        mine,
        Some(Permissions::ALL),
        "default permissions, not the old ones"
    );
    let syncwatch_sync_core::Peer::Host(host) = w.nodes[h2].s.peer() else {
        panic!()
    };
    assert!(host.descriptor().is_none());
    assert!(host.chat_history().is_empty());
}

#[test]
fn host_succession_after_a_host_crash() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    w.ui_action(
        h,
        UiAction::ChangeMedia {
            descriptor: MediaDescriptor::LocalFile(LocalMedia {
                name: "movie.mkv".into(),
                identity: FileIdentity {
                    sample_blake3: [5; 32],
                    size: 77,
                    duration_ms: Some(3_600_000),
                },
            }),
            own_file: true,
        },
    );
    let a = w.join("Ana", 2, None);
    let b = w.join("Ben", 3, None);
    w.run_for(500_000);
    chat(&mut w, b, "before the crash");
    let panels_before = w.nodes[b]
        .ui
        .iter()
        .filter(|u| matches!(u, RoomUi::Core(UiEvent::ShowMissingFilePanel { .. })))
        .count();

    w.crash(h);
    // Nobody claims while the host might still come back...
    w.run_for(5_000_000);
    assert!(!w.nodes[a].s.is_host() && !w.nodes[b].s.is_host());
    // ...then the lowest-ranked connected member (Ana) takes over and Ben reconnects to her.
    w.run_for(20_000_000);
    assert!(w.nodes[a].s.is_host(), "Ana took over");
    assert!(!w.nodes[b].s.is_host());
    assert!(w.in_room(a) && w.in_room(b), "{:?}", w.phases(b));
    let hosts: Vec<String> = w
        .members(b)
        .into_iter()
        .filter(|i| i.is_host)
        .map(|i| i.display_name)
        .collect();
    assert_eq!(hosts, vec!["Ana".to_owned()]);
    assert!(
        w.members(b).iter().all(|i| i.display_name != "Hana"),
        "the crashed host is gone"
    );
    // The room continues: same media, no second choose-my-file panel, chat works both ways.
    let syncwatch_sync_core::Peer::Host(host) = w.nodes[a].s.peer() else {
        panic!()
    };
    assert!(
        matches!(host.descriptor(), Some(MediaDescriptor::LocalFile(l)) if l.name == "movie.mkv")
    );
    let panels_after = w.nodes[b]
        .ui
        .iter()
        .filter(|u| matches!(u, RoomUi::Core(UiEvent::ShowMissingFilePanel { .. })))
        .count();
    assert_eq!(panels_after, panels_before, "no new choose-my-file panel");
    chat(&mut w, b, "after the crash");
    assert!(w.chats(a).contains(&"after the crash".to_owned()));
    chat(&mut w, a, "new host here");
    assert!(w.chats(b).contains(&"new host here".to_owned()));
    // New chat ids do not collide with the old ones (UI keys messages by id).
    let ids: Vec<u64> = w.nodes[b]
        .ui
        .iter()
        .filter_map(|u| match u {
            RoomUi::Core(UiEvent::ChatMessage(c)) => Some(c.id.0),
            _ => None,
        })
        .collect();
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), ids.len(), "chat ids unique: {ids:?}");
}

#[test]
fn no_succession_when_only_signaling_dropped_for_the_host() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let a = w.join("Ana", 2, None);
    w.run_for(300_000);
    // The host's signaling socket drops, but the data channel to Ana is fine.
    w.reply(a, ServerMsg::HostGone { grace_s: 30 });
    w.settle();
    w.run_for(30_000_000);
    assert!(
        !w.nodes[a].s.is_host(),
        "a healthy link to the host prevents a takeover"
    );
    assert!(w.nodes[h].s.is_host());
    chat(&mut w, a, "still the same host");
    assert!(w.chats(h).contains(&"still the same host".to_owned()));
}

/// Part B (M7): a host crash must not undo the room's rules. The host keeps its designated
/// successor updated (seats with custom permissions, mutes, bans, lock); the successor
/// enforces them as the new host. In particular a banned member cannot rejoin.
#[test]
fn succession_keeps_bans_lock_custom_permissions_and_mutes() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let a = w.join("Ana", 2, None);
    let b = w.join("Ben", 3, None);
    let c = w.join("Cy", 4, None);
    w.run_for(500_000);
    let id_of = |w: &World, name: &str| {
        w.members(h)
            .into_iter()
            .find(|i| i.display_name == name)
            .unwrap()
            .id
    };
    let (bid, cid) = (id_of(&w, "Ben"), id_of(&w, "Cy"));
    let mut custom = Permissions::NONE;
    custom.can_seek = true;
    w.ui_action(
        h,
        UiAction::SetPermissions {
            member: bid,
            permissions: custom,
        },
    );
    w.ui_action(h, UiAction::Moderate(Moderation::Mute(bid)));
    w.ui_action(h, UiAction::Ban(cid));
    w.ui_action(h, UiAction::Lock(true));
    w.run_for(500_000);
    assert_eq!(w.nodes[c].ended, Some(EndReason::Kicked { banned: true }));

    w.crash(h);
    w.run_for(25_000_000);
    assert!(w.nodes[a].s.is_host(), "Ana (rank 0) took over");
    assert!(w.in_room(b), "Ben reconnected to Ana: {:?}", w.phases(b));
    {
        let syncwatch_sync_core::Peer::Host(host) = w.nodes[a].s.peer() else {
            panic!()
        };
        assert!(host.is_locked(), "the lock survived");
        assert_eq!(host.bans().len(), 1, "the ban list survived");
    }
    // Ben kept his seat, his custom permissions and his mute.
    let ben = w
        .members(b)
        .into_iter()
        .find(|i| i.display_name == "Ben")
        .unwrap();
    assert_eq!(ben.id, bid, "same seat");
    assert_eq!(ben.permissions, custom, "custom permissions kept");
    assert!(ben.chat_muted);
    let muted_state = w.nodes[b].ui.iter().rev().find_map(|u| match u {
        RoomUi::Core(UiEvent::ChatState(s)) => Some(s.muted),
        _ => None,
    });
    assert_eq!(muted_state, Some(true), "Ben's window still shows muted");
    chat(&mut w, b, "still muted?");
    assert!(!w.chats(a).contains(&"still muted?".to_owned()));
    // The banned identity cannot come back after the crash, a new one cannot join the
    // locked room.
    let again = w.join("Cy", 4, None);
    w.run_for(2_000_000);
    assert_eq!(
        w.nodes[again].ended,
        Some(EndReason::Refused {
            reason: "banned".into()
        })
    );
    let new = w.join("Neo", 9, None);
    w.run_for(2_000_000);
    assert_eq!(
        w.nodes[new].ended,
        Some(EndReason::Refused {
            reason: "locked".into()
        })
    );
}

/// The successor role moves when the designated successor leaves; the new one is updated.
#[test]
fn the_next_successor_is_updated_when_the_first_one_leaves() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let a = w.join("Ana", 2, None);
    let b = w.join("Ben", 3, None);
    let c = w.join("Cy", 4, None);
    w.run_for(500_000);
    let cid = w
        .members(h)
        .into_iter()
        .find(|i| i.display_name == "Cy")
        .unwrap()
        .id;
    w.ui_action(h, UiAction::Ban(cid));
    w.run_for(500_000);
    let _ = c;
    w.crash(a);
    w.run_for(2_000_000);
    w.crash(h);
    w.run_for(30_000_000);
    assert!(w.nodes[b].s.is_host(), "Ben took over");
    let again = w.join("Cy", 4, None);
    w.run_for(2_000_000);
    assert_eq!(
        w.nodes[again].ended,
        Some(EndReason::Refused {
            reason: "banned".into()
        })
    );
}

// ---------------------------------------------------------------------- manual invites ----
// ADR 0014: copy-paste invites and replies, no signaling server. The host is the offerer of
// each invite link; the reply is applied only after Admit.

use syncwatch_net::handshake::HandshakeMode;
use syncwatch_net::invite::{Invite, InviteError};

fn manual_cfg(name: &str, seed: u8) -> SessionConfig {
    let mut c = cfg(name, seed);
    c.handshake = HandshakeMode::manual_default();
    c
}

impl World {
    fn now_unix(&self) -> u64 {
        (self.clock().wall_ms / 1000) as u64
    }

    fn host_manual(&mut self, name: &str, seed: u8, password: Option<&str>) -> usize {
        self.sdp_mode = true;
        let (s, outs) = RoomSession::create_manual(
            manual_cfg(name, seed),
            password.map(str::to_owned),
            "Movie night",
            self.clock(),
        );
        self.push(s, outs)
    }

    /// The host makes an invite; returns its id and text.
    fn make_invite(&mut self, h: usize, for_member: Option<MemberId>) -> (String, String) {
        self.act(h, Action::CreateInvite { for_member });
        self.run_for(100_000);
        let v = self.invites(h);
        let last = v.iter().find(|i| i.status == "waiting" && i.text.is_some());
        let inv = last.expect("an invite ready");
        (inv.id.clone(), inv.text.clone().unwrap())
    }

    fn invites(&self, i: usize) -> Vec<InviteView> {
        self.nodes[i]
            .ui
            .iter()
            .rev()
            .find_map(|u| match u {
                RoomUi::Invites(v) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn invite_errors(&self, i: usize) -> Vec<String> {
        self.nodes[i]
            .ui
            .iter()
            .filter_map(|u| match u {
                RoomUi::InviteError(e) => Some(e.clone()),
                _ => None,
            })
            .collect()
    }

    /// A member pastes the invite; returns the node and the reply text.
    fn answer(
        &mut self,
        name: &str,
        seed: u8,
        text: &str,
        password: Option<&str>,
    ) -> (usize, String) {
        let inv = Invite::decode(text, self.now_unix()).expect("a valid invite");
        let (s, outs) = RoomSession::join_invite(
            manual_cfg(name, seed),
            inv,
            password.map(str::to_owned),
            self.clock(),
        );
        let m = self.push(s, outs);
        self.run_for(100_000);
        let reply = self.nodes[m]
            .ui
            .iter()
            .find_map(|u| match u {
                RoomUi::Reply { text, .. } => Some(text.clone()),
                _ => None,
            })
            .expect("a reply");
        (m, reply)
    }

    fn reply_words(&self, m: usize) -> [&'static str; 4] {
        self.nodes[m]
            .ui
            .iter()
            .find_map(|u| match u {
                RoomUi::Reply { words, .. } => Some(*words),
                _ => None,
            })
            .unwrap()
    }

    fn admit_by_reply(&mut self, h: usize, id: &str, reply: &str) {
        self.act(h, Action::SubmitReply(reply.to_owned()));
        self.act(h, Action::Admit(id.to_owned()));
        self.run_for(500_000);
    }

    /// Links that have an open connection (both sides know each other).
    fn connected(&self, i: usize) -> usize {
        self.links
            .iter()
            .filter(|((n, _), l)| *n == i && l.remote.is_some())
            .count()
    }
}

#[test]
fn invite_reply_admit_then_the_room_works() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    assert!(w.in_room(h), "the host is in its room at once (no server)");
    let (id, text) = w.make_invite(h, None);
    assert!(text.starts_with("SWINV1-"), "{text}");
    let (m, reply) = w.answer("Mo", 2, &text, None);
    assert!(reply.starts_with("SWREP1-"));
    // Before the host acts nothing is connected.
    assert_eq!(w.connected(m), 0);
    assert!(!w.in_room(m));
    w.act(h, Action::SubmitReply(reply.clone()));
    let v = w.invites(h);
    let pending = v.iter().find(|i| i.id == id).unwrap();
    assert_eq!(pending.status, "replied");
    assert_eq!(pending.member_name.as_deref(), Some("Mo"));
    // The host sees the same four words the member sees.
    assert_eq!(pending.words, Some(w.reply_words(m)));
    // Still not connected: the reply is applied only after Admit.
    assert_eq!(w.connected(h), 0);
    w.act(h, Action::Admit(id.clone()));
    w.run_for(500_000);
    assert!(w.in_room(m), "{:?}", w.phases(m));
    assert_eq!(
        w.invites(h).iter().find(|i| i.id == id).unwrap().status,
        "joined"
    );
    // After joining, both sides show the invite words (not a second, different set).
    assert_eq!(w.words(m).last().unwrap().1, w.reply_words(m));
    assert_eq!(w.words(h).last().unwrap().1, w.reply_words(m));
    chat(&mut w, m, "hello host");
    assert_eq!(w.chats(h), vec!["hello host"]);
    assert_eq!(w.chats(m), vec!["hello host"]);
}

#[test]
fn an_unadmitted_reply_never_gets_room_data() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    let (id, text) = w.make_invite(h, None);
    let (m, reply) = w.answer("Eve", 3, &text, None);
    w.act(h, Action::SubmitReply(reply));
    w.act(h, Action::Refuse(id.clone()));
    w.run_for(1_000_000);
    assert_eq!(
        w.invites(h).iter().find(|i| i.id == id).unwrap().status,
        "refused"
    );
    assert_eq!(w.connected(m), 0, "never connected");
    assert!(
        w.members(m).is_empty(),
        "no room state reached the refused member"
    );
    assert!(w.chats(m).is_empty());
    // A replied-but-never-admitted member gives up after the invite's lifetime.
    w.run_for(13 * 60 * 1_000_000);
    assert_eq!(w.nodes[m].ended, Some(EndReason::NotAdmitted));
}

#[test]
fn reused_expired_tampered_and_unknown_replies_are_refused() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    let (id, text) = w.make_invite(h, None);
    let (_m, reply) = w.answer("Mo", 2, &text, None);
    // Tampered: one character changed.
    let mut b: Vec<char> = reply.chars().collect();
    let k = b.len() / 2;
    b[k] = if b[k] == 'x' { 'y' } else { 'x' };
    w.act(h, Action::SubmitReply(b.into_iter().collect()));
    assert_eq!(
        w.invite_errors(h).last().unwrap(),
        &InviteError::Damaged.to_string()
    );
    // The real reply works once...
    w.act(h, Action::SubmitReply(reply.clone()));
    assert_eq!(
        w.invites(h).iter().find(|i| i.id == id).unwrap().status,
        "replied"
    );
    // ...and a second reply to the same invite is refused (single use).
    let (_m2, reply2) = w.answer("Eve", 3, &text, None);
    w.act(h, Action::SubmitReply(reply2));
    assert_eq!(
        w.invite_errors(h).last().unwrap(),
        &InviteError::Used.to_string()
    );
    // An invite pasted where a reply belongs.
    w.act(h, Action::SubmitReply(text.clone()));
    assert_eq!(
        w.invite_errors(h).last().unwrap(),
        &InviteError::InviteNotReply.to_string()
    );
    // Expired: a second invite, 11 minutes later.
    let (id2, text2) = w.make_invite(h, None);
    let (_m3, reply3) = w.answer("Late", 4, &text2, None);
    w.run_for(11 * 60 * 1_000_000);
    assert_eq!(
        w.invites(h).iter().find(|i| i.id == id2).unwrap().status,
        "expired"
    );
    w.act(h, Action::SubmitReply(reply3));
    assert_eq!(
        w.invite_errors(h).last().unwrap(),
        &InviteError::Expired.to_string()
    );
    // The member side refuses an expired invite too.
    assert_eq!(
        Invite::decode(&text2, w.now_unix()),
        Err(InviteError::Expired)
    );
    // A reply for an invite this host never made.
    let mut w2 = World::new();
    let other = w2.host_manual("Other", 9, None);
    w2.act(other, Action::SubmitReply(reply));
    assert_eq!(
        w2.invite_errors(other).last().unwrap(),
        &InviteError::UnknownInvite.to_string()
    );
}

#[test]
fn invites_and_replies_carry_no_relay_credentials() {
    let mut w = World::new();
    w.sdp_mode = true;
    let mut c = manual_cfg("Hana", 1);
    c.ice = IceConfig {
        ice_servers: vec![IceServerJs {
            urls: vec!["turns:relay.example.invalid:443?transport=tcp".into()],
            username: Some("FAKE-TURN-USERNAME-1234".into()),
            credential: Some("FAKE-TURN-CREDENTIAL-5678".into()),
        }],
        relay_only: true,
    };
    let (s, outs) = RoomSession::create_manual(c, None, "Room", w.clock());
    let h = w.push(s, outs);
    let (_id, text) = w.make_invite(h, None);
    let inv = Invite::decode(&text, w.now_unix()).unwrap();
    let all = format!("{text} {inv:?}");
    for secret in [
        "FAKE-TURN-USERNAME-1234",
        "FAKE-TURN-CREDENTIAL-5678",
        "relay.example.invalid",
    ] {
        assert!(!all.contains(secret), "{secret} leaked into the invite");
    }
    let (_m, reply) = w.answer("Mo", 2, &text, None);
    let r = syncwatch_net::invite::Reply::decode(&reply).unwrap();
    assert!(!format!("{reply} {r:?}").contains("FAKE-TURN"));
}

#[test]
fn password_rooms_with_invites_and_wrong_password_throttling() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, Some("s3cret"));
    let (id, text) = w.make_invite(h, None);
    assert!(Invite::decode(&text, w.now_unix()).unwrap().password);
    let (m, reply) = w.answer("Mo", 2, &text, Some("wrong"));
    w.admit_by_reply(h, &id, &reply);
    assert!(!w.in_room(m));
    assert!(
        w.need_password(m).iter().any(|(wrong, _)| *wrong),
        "{:?}",
        w.need_password(m)
    );
    w.run_for(3_000_000);
    w.act(m, Action::SubmitPassword("s3cret".into()));
    w.run_for(1_000_000);
    assert!(w.in_room(m), "{:?}", w.phases(m));
}

#[test]
fn a_connection_that_does_not_match_the_reply_is_closed() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    let (id, text) = w.make_invite(h, None);
    let (m, reply) = w.answer("Mo", 2, &text, None);
    // Someone in the middle: the host sees another certificate than the reply named.
    w.mitm = true;
    w.admit_by_reply(h, &id, &reply);
    assert!(!w.in_room(m));
    assert!(w.members(h).iter().all(|i| i.is_host), "nobody else got in");
}

#[test]
fn a_dropped_member_rejoins_its_kept_seat_with_a_re_invite() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    let (id, text) = w.make_invite(h, None);
    let (m, reply) = w.answer("Mo", 2, &text, None);
    w.admit_by_reply(h, &id, &reply);
    assert!(w.in_room(m));
    let seat = w.members(h).iter().find(|i| !i.is_host).unwrap().id;
    // The connection is really gone (no signaling: no automatic reconnect).
    let link = w.host_link_of(m);
    w.close_sim(m, link);
    w.input(
        m,
        Input::Transport(TransportEvent::Closed {
            peer: link,
            reason: "gone".into(),
        }),
    );
    w.settle();
    assert_eq!(
        w.nodes[m].ended,
        Some(EndReason::ConnectionLost {
            seat_keep_secs: 300
        })
    );
    w.run_for(60 * 1_000_000);
    // The seat is still kept after a minute (5 minutes with invites).
    assert!(w.members(h).iter().any(|i| i.id == seat && !i.connected));
    let (rid, rtext) = w.make_invite(h, Some(seat));
    // Someone else cannot use the re-invite.
    let (_e, ereply) = w.answer("Eve", 3, &rtext, None);
    w.act(h, Action::SubmitReply(ereply));
    assert_eq!(
        w.invite_errors(h).last().unwrap(),
        &InviteError::WrongMember.to_string()
    );
    // The same person (same identity) gets the same seat back.
    let (m2, rreply) = w.answer("Mo", 2, &rtext, None);
    w.admit_by_reply(h, &rid, &rreply);
    assert!(w.in_room(m2));
    assert!(w.members(h).iter().any(|i| i.id == seat && i.connected));
}

#[test]
fn make_host_is_refused_in_invite_rooms() {
    let mut w = World::new();
    let h = w.host_manual("Hana", 1, None);
    let (id, text) = w.make_invite(h, None);
    let (m, reply) = w.answer("Mo", 2, &text, None);
    w.admit_by_reply(h, &id, &reply);
    let seat = w.members(h).iter().find(|i| !i.is_host).unwrap().id;
    w.ui_action(h, UiAction::MakeHost(seat));
    assert!(
        w.nodes[h]
            .ui
            .iter()
            .any(|u| matches!(u, RoomUi::Error(e) if e.contains("Make host")))
    );
    assert!(w.nodes[h].s.is_host());
    assert!(w.in_room(m));
}

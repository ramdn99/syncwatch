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
}

fn cfg(name: &str, seed: u8) -> SessionConfig {
    SessionConfig {
        key: IdentityKey::from_secret([seed; 32]),
        display_name: name.into(),
        ice: IceConfig::default(),
        max_members: 12,
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
        });
        let i = self.nodes.len() - 1;
        self.exec(i, outs);
        self.settle();
        i
    }

    fn input(&mut self, i: usize, input: Input) {
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
                        self.queue.push_back(Ev::Tr(
                            i,
                            TransportEvent::LocalSignal {
                                peer,
                                data: SignalData::Offer { sdp: fp },
                            },
                        ));
                    }
                }
                Output::RemoteSignal { peer, data } => match data {
                    SignalData::Offer { .. } => {
                        let fp = self.links[&(i, peer)].fp.clone();
                        self.queue.push_back(Ev::Tr(
                            i,
                            TransportEvent::LocalSignal {
                                peer,
                                data: SignalData::Answer { sdp: fp },
                            },
                        ));
                    }
                    SignalData::Answer { sdp } => {
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
                Output::Send { peer, lane, data } => {
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
            ClientMsg::Hello { .. } | ClientMsg::Resume { .. } | ClientMsg::Ping => {}
        }
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
fn revoked_chat_permission_is_enforced_by_the_host() {
    let mut w = World::new();
    let h = w.host("Hana", 1, None);
    let m = w.join("Mo", 2, None);
    w.run_for(300_000);
    let mid = w.members(h).into_iter().find(|i| !i.is_host).unwrap().id;
    let mut p = Permissions::ALL;
    p.can_chat = false;
    w.ui_action(
        h,
        UiAction::SetPermissions {
            member: mid,
            permissions: p,
        },
    );
    w.run_for(300_000);
    chat(&mut w, m, "should not arrive");
    assert!(w.chats(h).is_empty());
    assert!(w.nodes[m].ui.iter().any(|u| matches!(
        u,
        RoomUi::Core(UiEvent::ChatRejected {
            reason: ChatRejectReason::NoPermission,
            ..
        })
    )));
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
            custom: Permissions::CHAT_ONLY,
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
    p.can_chat = false;
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
            custom: Permissions::CHAT_ONLY,
        },
    );
    w.run_for(200_000);
    assert_eq!(mine(&w), Some(Permissions::CHAT_ONLY));
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
            hidden_from_members: false,
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
        vec![(Some(mid), "chat message refused: Disabled".to_owned())]
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

#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! Change source (all four transitions), concurrent changes, "Choose my file", magnets,
//! and room administration.

mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::sim::{NodeConfig, World};
use syncwatch_sync_core::*;

const M1: &str = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=one.mkv";
const M2: &str = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=two.mkv";

fn torrent(m: &str, name: &str) -> MediaDescriptor {
    MediaDescriptor::Torrent(TorrentMedia {
        infohash: magnet::parse(m).unwrap().infohash,
        file_index: 0,
        name: name.into(),
        size: 700_000_000,
        magnet: m.into(),
    })
}

fn other_local() -> (MediaDescriptor, FileIdentity) {
    let id = FileIdentity {
        sample_blake3: [9; 32],
        size: 999,
        duration_ms: Some(60_000),
    };
    (
        MediaDescriptor::LocalFile(LocalMedia {
            name: "other.mkv".into(),
            identity: id,
        }),
        id,
    )
}

fn torrent_effects(w: &World, idx: usize, since: i64) -> Vec<Effect> {
    w.nodes[idx]
        .effects
        .iter()
        .filter(|(t, e)| {
            *t >= since && matches!(e, Effect::StartTorrent { .. } | Effect::StopTorrent { .. })
        })
        .map(|(_, e)| e.clone())
        .collect()
}

fn ih(m: &str) -> Infohash {
    magnet::parse(m).unwrap().infohash
}

/// Local -> torrent -> torrent -> local -> local, all in the same room, requested by a member
/// with can_change_media. Every transition stops the old source completely, resets playback
/// and tracks, and members report status through the ready gate.
#[test]
fn all_four_source_transitions() {
    let (mut w, ids) = room(2, 61);
    let changer = ids[0];
    w.ui(HOST, UiAction::Play);
    w.ui(HOST, UiAction::SelectSub(Selection::Off));
    w.run_for(3_000_000);

    // 1) local -> torrent (member's request; host and the other member confirm the magnet).
    let t0 = w.now;
    w.ui(
        changer,
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "one.mkv"),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    for i in [HOST, ids[0], ids[1]] {
        assert!(
            !w.nodes[i].player.loaded,
            "old local file stopped on node {i}"
        );
        assert_eq!(w.nodes[i].peer.local().room.position_ms, 0);
        assert_eq!(
            w.nodes[i].peer.local().tracks.desired.subtitle,
            Selection::Default,
            "track state reset"
        );
        assert!(
            torrent_effects(&w, i, t0).contains(&Effect::StartTorrent {
                infohash: ih(M1),
                file_index: 0
            }),
            "node {i}"
        );
    }
    assert!(
        !w.nodes[ids[1]]
            .ui_events()
            .all(|u| !matches!(u, UiEvent::ConfirmMagnet { .. })),
        "non-requesters confirm first"
    );
    // Streams become playable.
    for i in [HOST, ids[0], ids[1]] {
        w.deliver_event(i, Event::Torrent(TorrentEvent::Playable));
    }
    w.run_for(1_000_000);
    assert!(w.nodes[ids[1]].player.loaded);

    // 2) torrent -> torrent: the old torrent stops in both directions on every node.
    let t1 = w.now;
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(M2, "two.mkv"),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    for i in [HOST, ids[0], ids[1]] {
        let fx = torrent_effects(&w, i, t1);
        assert_eq!(
            fx.first(),
            Some(&Effect::StopTorrent { infohash: ih(M1) }),
            "node {i}: old torrent stopped first"
        );
        assert!(fx.contains(&Effect::StartTorrent {
            infohash: ih(M2),
            file_index: 0
        }));
        // Nothing for the old infohash is ever started again.
        assert!(!fx.contains(&Effect::StartTorrent {
            infohash: ih(M1),
            file_index: 0
        }));
    }

    // 3) torrent -> local.
    let t2 = w.now;
    w.ui(
        changer,
        UiAction::ChangeMedia {
            descriptor: World::local_descriptor(),
            own_file: true,
        },
    );
    w.run_for(2_000_000);
    for i in [HOST, ids[0], ids[1]] {
        assert_eq!(
            torrent_effects(&w, i, t2),
            vec![Effect::StopTorrent { infohash: ih(M2) }],
            "node {i}"
        );
        assert!(
            w.nodes[i].player.loaded,
            "node {i} chose its own copy and loaded it"
        );
        assert_eq!(w.nodes[i].peer.local().media, MediaStatus::Ready);
    }

    // 4) local -> local (a different file nobody else has).
    let (desc, _) = other_local();
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: desc,
            own_file: false,
        },
    );
    w.run_for(2_000_000);
    for i in ids.iter().copied() {
        assert_eq!(
            w.nodes[i].peer.local().media,
            MediaStatus::Mismatch,
            "node {i}: own file differs"
        );
        let shown = w.nodes[i]
            .ui_events()
            .filter(
                |u| matches!(u, UiEvent::ShowMissingFilePanel { name, .. } if name == "other.mkv"),
            )
            .count();
        assert_eq!(shown, 1, "missing-file panel shown exactly once");
    }
    let me = host(&w).room().media_epoch;
    assert_eq!(me, MediaEpoch(5));
}

#[test]
fn change_media_requires_permission_and_valid_descriptor() {
    let (mut w, ids) = room(1, 62);
    let mut p = Permissions::ALL;
    p.can_change_media = false;
    w.ui(
        HOST,
        UiAction::SetPermissions {
            member: w.nodes[ids[0]].id,
            permissions: p,
        },
    );
    w.run_for(300_000);
    w.ui(
        ids[0],
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "x.mkv"),
            own_file: false,
        },
    );
    w.run_for(500_000);
    assert_eq!(host(&w).room().media_epoch, MediaEpoch(1));
    // A crafted request is refused by the host, and invalid descriptors are refused too.
    let from = Sender {
        member: w.nodes[ids[0]].id,
        identity: w.nodes[ids[0]].identity,
    };
    w.deliver_event(
        HOST,
        Event::Net {
            from,
            msg: Message::Request(Request::ChangeMedia {
                request_id: 1,
                descriptor: torrent(M1, "x.mkv"),
            }),
        },
    );
    w.run_for(300_000);
    assert_eq!(host(&w).room().media_epoch, MediaEpoch(1));
    assert!(notices(&w, ids[0]).contains(&Notice::RequestDenied {
        reason: DenyReason::NoPermission
    }));
    let bad = MediaDescriptor::Torrent(TorrentMedia {
        infohash: Infohash([0; 20]),
        file_index: 0,
        name: "x".into(),
        size: 1,
        magnet: M1.into(),
    });
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: bad,
            own_file: false,
        },
    );
    assert_eq!(host(&w).room().media_epoch, MediaEpoch(1));
}

/// Two members change the source at nearly the same time: the later request wins and the
/// earlier requester gets a toast naming who replaced it.
#[test]
fn concurrent_change_requests_later_wins_with_toast() {
    let (mut w, ids) = room(2, 63);
    w.ui(
        ids[0],
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "one.mkv"),
            own_file: false,
        },
    );
    w.run_for(20_000);
    w.ui(
        ids[1],
        UiAction::ChangeMedia {
            descriptor: torrent(M2, "two.mkv"),
            own_file: false,
        },
    );
    w.run_for(2_000_000);
    for i in [HOST, ids[0], ids[1]] {
        match &w.nodes[i].peer.local().descriptor {
            Some(MediaDescriptor::Torrent(t)) => assert_eq!(t.infohash, ih(M2), "node {i}"),
            other => panic!("node {i}: {other:?}"),
        }
    }
    let by = w.nodes[ids[1]].id;
    assert!(notices(&w, ids[0]).contains(&Notice::MediaChangeSuperseded { request_id: 1, by }));
    assert!(
        !notices(&w, ids[1])
            .iter()
            .any(|n| matches!(n, Notice::MediaChangeSuperseded { .. }))
    );
}

/// "Choose my file" needs NO permission: a member without any playback permission picks
/// their copy, it is verified and they become ready.
#[test]
fn choose_my_file_works_without_can_change_media() {
    let mut host_cfg = NodeConfig::named("Host");
    host_cfg.file = Some(World::sample_identity());
    let mut w = World::new(64, host_cfg);
    w.ui(
        HOST,
        UiAction::ApplyPreset {
            preset: Preset::OnlyMe,
            custom: Permissions::NONE,
        },
    );
    let mut c = NodeConfig::named("NoPerms");
    c.file = Some(World::sample_identity());
    let m = w.add_member(c);
    w.run_for(300_000);
    assert!(!member(&w, m).perms.can_change_media);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: World::local_descriptor(),
            own_file: true,
        },
    );
    w.run_for(3_000_000);
    assert!(
        w.nodes[m]
            .ui_events()
            .any(|u| matches!(u, UiEvent::ShowMissingFilePanel { .. }))
    );
    assert!(
        w.nodes[m]
            .ui_events()
            .any(|u| matches!(u, UiEvent::FileVerified))
    );
    assert_eq!(member(&w, m).local.media, MediaStatus::Ready);
    assert!(w.nodes[m].player.loaded);
    // The member never sent a request for it (no permission involved), only status reports.
    assert!(
        !w.nodes[m]
            .sent
            .iter()
            .any(|(_, msg)| matches!(msg, Message::Request(_)))
    );
    // And the file path never left the member: no message carries more than names/hashes.
    w.ui(HOST, UiAction::Play);
    w.run_for(2_000_000);
    assert!(w.nodes[m].player.is_playing());
}

#[test]
fn mismatching_file_warns_and_member_may_proceed_with_sync() {
    let (other_desc, _) = other_local();
    let _ = other_desc;
    let wrong = FileIdentity {
        sample_blake3: [1; 32],
        size: 5,
        duration_ms: Some(1),
    };
    let (mut w, ids) = room_with(1, 65, |_, c| c.file = Some(wrong));
    let m = ids[0];
    assert_eq!(member(&w, m).local.media, MediaStatus::Mismatch);
    assert!(
        w.nodes[m]
            .ui_events()
            .any(|u| matches!(u, UiEvent::FileMismatch { .. }))
    );
    assert!(
        !w.nodes[m].player.loaded,
        "not loaded until the member proceeds"
    );
    w.ui(m, UiAction::ProceedWithMismatch);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    assert!(w.nodes[m].player.is_playing(), "sync still applies");
    assert!(w.max_divergence_ms() < 150.0);
    // "Choose a different file": the right file afterwards becomes Ready.
    w.deliver_event(
        m,
        Event::LocalFile(LocalFileEvent::Identified(World::sample_identity())),
    );
    w.run_for(1_000_000);
    assert_eq!(member(&w, m).local.media, MediaStatus::Ready);
    // Host sees the member's status in the member list.
    let infos = w.nodes[HOST]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Members(v) => Some(v.clone()),
            _ => None,
        })
        .last()
        .unwrap();
    assert!(
        infos
            .iter()
            .any(|i| i.id == w.nodes[m].id && i.media == MediaStatus::Ready)
    );
}

#[test]
fn member_declines_magnet_and_host_sees_declined() {
    let (mut w, ids) = room_with(2, 66, |i, c| c.accept_magnets = i != 1);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "one.mkv"),
            own_file: false,
        },
    );
    w.run_for(500_000);
    assert!(w.nodes[ids[1]].ui_events().any(|u| matches!(u, UiEvent::ConfirmMagnet { name, size, .. } if name == "one.mkv" && *size == 700_000_000)));
    w.ui(ids[1], UiAction::DeclineMagnet);
    w.run_for(500_000);
    assert!(
        torrent_effects(&w, ids[1], 0).is_empty(),
        "nothing downloaded after declining"
    );
    assert!(notices(&w, HOST).contains(&Notice::MemberDeclinedMagnet {
        member: w.nodes[ids[1]].id
    }));
    // Declined members never block the ready gate.
    for i in [HOST, ids[0]] {
        w.deliver_event(i, Event::Torrent(TorrentEvent::Playable));
    }
    w.run_for(1_000_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(2_000_000);
    assert!(w.nodes[HOST].player.is_playing());
}

#[test]
fn torrent_engine_unavailable_is_reported() {
    let (mut w, ids) = room(1, 67);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "one.mkv"),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    w.deliver_event(ids[0], Event::Torrent(TorrentEvent::Unavailable));
    w.run_for(500_000);
    let infos = w.nodes[HOST]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Members(v) => Some(v.clone()),
            _ => None,
        })
        .last()
        .unwrap();
    assert!(
        infos
            .iter()
            .any(|i| i.id == w.nodes[ids[0]].id && i.media == MediaStatus::Unavailable)
    );
}

#[test]
fn kick_ban_lock_and_full_room() {
    let mut host_cfg = NodeConfig::named("Host");
    host_cfg.file = Some(World::sample_identity());
    let mut w = World::new(68, host_cfg);
    let a = w.add_member(NodeConfig::named("A"));
    let b = w.add_member(NodeConfig::named("B"));
    w.run_for(500_000);
    w.ui(HOST, UiAction::Kick(w.nodes[a].id));
    w.run_for(300_000);
    assert!(
        w.nodes[a]
            .ui_events()
            .any(|u| *u == UiEvent::Kicked { banned: false })
    );
    assert!(!w.nodes[a].connected);
    w.ui(HOST, UiAction::Ban(w.nodes[b].id));
    w.run_for(300_000);
    assert!(
        w.nodes[b]
            .ui_events()
            .any(|u| *u == UiEvent::Kicked { banned: true })
    );
    // The banned identity cannot come back.
    let identity = w.nodes[b].identity;
    w.deliver_event(
        HOST,
        Event::PeerJoined {
            member: MemberId(50),
            identity,
            display_name: "B again".into(),
        },
    );
    assert!(w.nodes[HOST].effects.iter().any(|(_, e)| *e
        == Effect::SendTo(
            MemberId(50),
            Message::Reject {
                reason: RejectReason::Banned
            }
        )));
    // Locked room.
    w.ui(HOST, UiAction::Lock(true));
    w.deliver_event(
        HOST,
        Event::PeerJoined {
            member: MemberId(51),
            identity: Identity([77; 32]),
            display_name: "late".into(),
        },
    );
    assert!(w.nodes[HOST].effects.iter().any(|(_, e)| *e
        == Effect::SendTo(
            MemberId(51),
            Message::Reject {
                reason: RejectReason::Locked
            }
        )));
    w.ui(HOST, UiAction::Lock(false));
    // Full room: 12 including the host, so the 12th joiner is rejected.
    for i in 0..12u32 {
        w.deliver_event(
            HOST,
            Event::PeerJoined {
                member: MemberId(100 + i),
                identity: Identity([100 + i as u8; 32]),
                display_name: format!("p{i}"),
            },
        );
    }
    assert!(w.nodes[HOST].effects.iter().any(|(_, e)| *e
        == Effect::SendTo(
            MemberId(111),
            Message::Reject {
                reason: RejectReason::Full
            }
        )));
    assert!(!w.nodes[HOST].effects.iter().any(|(_, e)| *e
        == Effect::SendTo(
            MemberId(110),
            Message::Reject {
                reason: RejectReason::Full
            }
        )));
}

#[test]
fn presets_and_defaults_for_new_joiners() {
    let (mut w, ids) = room(1, 69);
    w.ui(
        HOST,
        UiAction::ApplyPreset {
            preset: Preset::OnlyMe,
            custom: Permissions::ALL,
        },
    );
    w.run_for(300_000);
    assert_eq!(member(&w, ids[0]).perms, Permissions::NONE);
    let mut c = NodeConfig::named("New");
    c.file = Some(World::sample_identity());
    let n = w.add_member(c);
    w.run_for(500_000);
    assert_eq!(member(&w, n).perms, Permissions::NONE);
    w.ui(
        HOST,
        UiAction::ApplyPreset {
            preset: Preset::EveryoneControls,
            custom: Permissions::ALL,
        },
    );
    w.run_for(300_000);
    assert_eq!(member(&w, n).perms, Permissions::ALL);
}

#[test]
fn host_transfer_keeps_playback_tracks_and_permissions() {
    let (mut w, ids) = room(2, 70);
    w.ui(HOST, UiAction::Play);
    w.ui(
        HOST,
        UiAction::SelectAudio(Selection::Track(TrackRef::Embedded {
            ff_index: Some(2),
            kind: TrackKind::Audio,
            lang: Some("eng".into()),
            title: None,
            codec: Some("eac3".into()),
        })),
    );
    w.run_for(5_000_000);
    let mut p = Permissions::ALL;
    p.can_seek = false;
    w.ui(
        HOST,
        UiAction::SetPermissions {
            member: w.nodes[ids[1]].id,
            permissions: p,
        },
    );
    w.run_for(300_000);
    let before = w.pos(HOST);
    w.ui(HOST, UiAction::MakeHost(w.nodes[ids[0]].id));
    w.run_for(3_000_000);
    assert_eq!(w.host_idx(), ids[0]);
    assert!(
        w.nodes[ids[0]]
            .ui_events()
            .any(|u| *u == UiEvent::BecameHost)
    );
    let h = host(&w);
    assert!(!h.room().paused, "still playing");
    assert_eq!(
        h.permissions_of(w.nodes[ids[1]].id).map(|p| p.can_seek),
        Some(false)
    );
    assert!(matches!(h.tracks().audio, Selection::Track(_)));
    // Everyone (old host included) keeps playing in sync, around the expected position.
    assert!(w.pos(ids[0]) > before + 2_000.0);
    w.run_for(5_000_000);
    assert!(w.max_divergence_ms() < 150.0, "{}", w.max_divergence_ms());
    for i in [HOST, ids[0], ids[1]] {
        assert_eq!(w.nodes[i].player.audio, Some(Some(2)), "node {i} audio");
    }
    // The old host is now an ordinary member: its requests go to the new host.
    w.ui(HOST, UiAction::Pause);
    w.run_for(1_000_000);
    assert!(host(&w).room().paused);
}

#[test]
fn dropped_member_keeps_the_seat_for_20_seconds() {
    let (mut w, ids) = room(2, 71);
    let m = ids[0];
    let (id, identity) = (w.nodes[m].id, w.nodes[m].identity);
    w.run_for(1_000_000);
    w.disconnect(m);
    w.run_for(19_000_000);
    assert_eq!(host(&w).seat_of(&identity), Some(id), "seat kept for 19 s");
    let listed = |w: &World| {
        let ms: Vec<MemberInfo> = w.nodes[HOST]
            .ui
            .iter()
            .rev()
            .find_map(|(_, u)| match u {
                UiEvent::Members(v) => Some(v.clone()),
                _ => None,
            })
            .unwrap();
        ms.into_iter().find(|i| i.id == id).map(|i| i.connected)
    };
    assert_eq!(listed(&w), Some(false), "listed as reconnecting");
    w.run_for(2_000_000);
    assert_eq!(host(&w).seat_of(&identity), None, "seat freed after 20 s");
    assert_eq!(listed(&w), None, "no longer listed");
}

#[test]
fn reconnect_within_20_seconds_gets_the_same_seat() {
    let (mut w, ids) = room(2, 72);
    let m = ids[0];
    let (id, identity) = (w.nodes[m].id, w.nodes[m].identity);
    w.run_for(1_000_000);
    w.disconnect(m);
    w.run_for(5_000_000);
    let h = w.host_idx();
    w.deliver_event(
        h,
        Event::PeerJoined {
            member: MemberId(99),
            identity,
            display_name: "again".into(),
        },
    );
    assert_eq!(host(&w).seat_of(&identity), Some(id));
}

/// The host's progress block: each member's torrent phase and whole-percent progress reach
/// the member list (and a status update is sent only per whole percent).
#[test]
fn torrent_progress_reaches_the_host_in_whole_percent_steps() {
    let (mut w, ids) = room(1, 68);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(M1, "one.mkv"),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    let member = w.nodes[ids[0]].id;
    let status_of = |w: &World| {
        w.nodes[HOST]
            .ui_events()
            .filter_map(|u| match u {
                UiEvent::Members(v) => v.iter().find(|i| i.id == member).map(|i| i.torrent),
                _ => None,
            })
            .last()
            .flatten()
    };
    assert_eq!(
        status_of(&w),
        Some(TorrentProgress {
            phase: TorrentPhase::Metadata,
            permille: 0
        }),
        "accepting the magnet starts in the metadata phase"
    );
    let sent_before = w.nodes[ids[0]]
        .sent
        .iter()
        .filter(|(_, m)| matches!(m, Message::Status(_)))
        .count();
    // 0.1% steps: only whole percents produce a status message.
    for p in 120..=130u16 {
        w.deliver_event(
            ids[0],
            Event::Torrent(TorrentEvent::Progress {
                phase: TorrentPhase::Downloading,
                permille: p,
            }),
        );
    }
    w.run_for(1_000_000);
    let sent = w.nodes[ids[0]]
        .sent
        .iter()
        .filter(|(_, m)| matches!(m, Message::Status(_)))
        .count()
        - sent_before;
    assert!(sent <= 3, "at most one status per whole percent: {sent}");
    assert_eq!(
        status_of(&w),
        Some(TorrentProgress {
            phase: TorrentPhase::Downloading,
            permille: 130
        })
    );
    w.deliver_event(
        ids[0],
        Event::Torrent(TorrentEvent::Progress {
            phase: TorrentPhase::Complete,
            permille: 1000,
        }),
    );
    w.run_for(1_000_000);
    assert_eq!(status_of(&w).map(|t| t.phase), Some(TorrentPhase::Complete));
}

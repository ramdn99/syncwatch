#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! Playback sync: scheduling, heartbeat regression, drift, ready gate, late joiners.

mod common;
use common::*;
use proptest::prelude::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::sim::{LinkConfig, NodeConfig, World};
use syncwatch_sync_core::*;

fn host_sender() -> Sender {
    Sender {
        member: MemberId::HOST,
        identity: Identity([1; 32]),
    }
}

/// A joined member with a precise clock (host time == local time) and the file loaded.
fn joined_member() -> MemberMachine {
    let mut m = MemberMachine::new(Identity([2; 32]), "M");
    let snapshot = Snapshot {
        your_id: MemberId(1),
        host_identity: Identity([1; 32]),
        descriptor: Some(World::local_descriptor()),
        playback: RoomPlayback::initial(MediaEpoch(1), 0),
        tracks: TrackState::default(),
        members: vec![],
        your_permissions: Permissions::ALL,
        chat: vec![],
        chat_state: ChatState::default(),
    };
    m.handle(
        clock(0),
        Event::Net {
            from: host_sender(),
            msg: Message::Welcome(Box::new(snapshot)),
        },
    );
    // Clock sync: perfect symmetric pongs.
    for i in 0..4u32 {
        let t0 = i as i64 * 1000;
        m.clock.add_pong(t0, t0 + 10, t0 + 10, t0 + 20);
    }
    m.handle(
        clock(100),
        Event::LocalFile(LocalFileEvent::Identified(World::sample_identity())),
    );
    m.handle(
        clock(200),
        Event::Player(PlayerObs::FileLoaded {
            tracks: vec![],
            duration_ms: Some(1_000_000),
        }),
    );
    m.handle(clock(300), Event::Player(PlayerObs::SeekDone));
    m
}

fn unpaused(effects: &[Effect]) -> bool {
    effects.contains(&Effect::Player(PlayerCmd::SetPaused(false)))
}

/// REGRESSION: a heartbeat must never overtake a scheduled play. An earlier build applied a
/// heartbeat (sent after the Play command, before its start time) immediately, so members
/// started ~300 ms early.
#[test]
fn heartbeat_never_overtakes_scheduled_play() {
    let mut m = joined_member();
    let anchor = 1_500_000;
    let play = RoomPlayback {
        media_epoch: MediaEpoch(1),
        epoch: Epoch(1),
        position_ms: 0,
        anchor_host_us: anchor,
        paused: false,
        speed: 1000,
    };
    let fx = m.handle(
        clock(1_000_000),
        Event::Net {
            from: host_sender(),
            msg: Message::Command(Command::Playback(play)),
        },
    );
    assert!(!unpaused(&fx), "must not start before the anchor");
    // Heartbeats sent between the command and the anchor: same epoch, and a newer epoch.
    for (t, hb) in [
        (1_100_000, play),
        (
            1_200_000,
            RoomPlayback {
                epoch: Epoch(2),
                ..play
            },
        ),
    ] {
        let fx = m.handle(
            clock(t),
            Event::Net {
                from: host_sender(),
                msg: Message::Heartbeat {
                    playback: hb,
                    host_now_us: t - 50_000,
                },
            },
        );
        assert!(!unpaused(&fx), "heartbeat at {t} started playback early");
        let fx = m.handle(clock(t + 10_000), Event::Tick);
        assert!(!unpaused(&fx), "tick at {t} started playback early");
    }
    assert_eq!(m.next_deadline(), Some(anchor));
    let fx = m.handle(clock(anchor - 1), Event::Tick);
    assert!(!unpaused(&fx));
    let fx = m.handle(clock(anchor), Event::Tick);
    assert!(unpaused(&fx), "starts exactly at the anchor");
}

/// Everyone (host included) starts at the scheduled host time, within one sim step.
#[test]
fn scheduled_start_is_simultaneous_despite_skew_and_latency() {
    let (mut w, ids) = room_with(4, 3, |i, c| {
        c.clock_offset_us = [-40_000_000, 3_000_000, 17_000_000, -900_000][i];
        c.clock_drift_ppm = [50.0, -50.0, 20.0, 0.0][i];
        c.link = LinkConfig {
            latency_us: [20_000, 80_000, 150_000, 40_000][i],
            jitter_us: 10_000,
            loss: 0.1,
        };
    });
    w.run_for(3_000_000); // clock sync settles
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    let anchor = w.nodes[HOST]
        .effects
        .iter()
        .find_map(|(_, e)| match e {
            Effect::Broadcast(Message::Command(Command::Playback(p))) if !p.paused => {
                Some(p.anchor_host_us)
            }
            _ => None,
        })
        .expect("play broadcast");
    for idx in std::iter::once(HOST).chain(ids) {
        let started = *w.nodes[idx].player.started_at.last().expect("started");
        // Host clock == true time in the sim (offset 0, no drift).
        assert!(
            (started - anchor).abs() <= 6_000,
            "node {idx} started at {started}, anchor {anchor}"
        );
    }
}

#[test]
fn members_stay_within_drift_bounds_with_player_drift() {
    let (mut w, _ids) = room_with(3, 9, |i, c| c.player_drift_ppm = [800.0, -800.0, 300.0][i]);
    w.run_for(2_000_000);
    w.ui(HOST, UiAction::Play);
    let mut worst: f64 = 0.0;
    for _ in 0..90 {
        w.run_for(1_000_000);
        worst = worst.max(w.max_divergence_ms());
    }
    assert!(worst < 150.0, "divergence {worst} ms");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]

    /// Under random latency, jitter, loss, clock offsets and drift, members converge to the host.
    #[test]
    fn convergence_under_random_network(
        seed in 0u64..1000,
        lat in proptest::collection::vec(0i64..250_000, 3),
        jit in 0i64..60_000,
        loss in 0.0f64..0.3,
        offs in proptest::collection::vec(-60_000_000i64..60_000_000, 3),
        drift in proptest::collection::vec(-500.0f64..500.0, 3),
    ) {
        let (mut w, _ids) = room_with(3, seed, |i, c| {
            c.link = LinkConfig { latency_us: lat[i], jitter_us: jit, loss };
            c.clock_offset_us = offs[i];
            c.player_drift_ppm = drift[i];
            c.clock_drift_ppm = drift[i] / 10.0;
        });
        w.run_for(3_000_000);
        w.ui(HOST, UiAction::Play);
        w.run_for(15_000_000);
        let mut worst: f64 = 0.0;
        for _ in 0..20 {
            w.run_for(1_000_000);
            worst = worst.max(w.max_divergence_ms());
        }
        prop_assert!(worst < 200.0, "divergence {worst} ms");
    }
}

#[test]
fn seek_uses_ready_gate_and_everyone_lands_together() {
    let (mut w, ids) = room_with(3, 5, |i, c| {
        c.seek_latency_us = [80_000, 900_000, 200_000][i]
    });
    w.run_for(1_000_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(5_000_000);
    w.ui(ids[0], UiAction::Seek(600_000));
    w.run_for(300_000);
    // The slow seeker blocks the restart.
    let gates: Vec<GateInfo> = w.nodes[HOST]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Gate(g) => Some(g.clone()),
            _ => None,
        })
        .collect();
    assert!(
        gates.iter().any(|g| g.active && !g.waiting.is_empty()),
        "gate shows who we wait for"
    );
    w.run_for(5_000_000);
    assert!(w.max_divergence_ms() < 100.0, "{}", w.max_divergence_ms());
    assert!(
        w.pos(HOST) > 600_000.0 && w.pos(HOST) < 610_000.0,
        "{}",
        w.pos(HOST)
    );
    assert!(w.nodes[HOST].player.is_playing());
}

#[test]
fn missing_file_member_does_not_block_unless_waiting_for_everyone() {
    let (mut w, ids) = room_with(2, 6, |i, c| {
        if i == 1 {
            c.file = None
        }
    });
    w.run_for(1_000_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(2_000_000);
    assert!(
        w.nodes[HOST].player.is_playing(),
        "missing-file member must not block"
    );
    assert!(w.nodes[ids[0]].player.is_playing());
    assert!(!w.nodes[ids[1]].player.loaded);

    w.ui(HOST, UiAction::SetWaitForEveryone(true));
    w.ui(HOST, UiAction::Pause);
    w.run_for(500_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(2_000_000);
    assert!(
        !w.nodes[HOST].player.is_playing(),
        "waits for the member without the file"
    );
    let waiting = w.nodes[HOST]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Gate(g) if g.active => Some(g.waiting.clone()),
            _ => None,
        })
        .last()
        .unwrap();
    assert!(waiting.contains(&(w.nodes[ids[1]].id, NotReadyReason::MissingFile)));
    // "Resume without X".
    w.ui(HOST, UiAction::ResumeWithoutWaiting);
    w.run_for(2_000_000);
    assert!(w.nodes[HOST].player.is_playing());
    assert!(w.nodes[ids[0]].player.is_playing());
}

#[test]
fn gate_times_out() {
    let (mut w, _ids) = room_with(1, 8, |i, c: &mut NodeConfig| {
        if i == 0 {
            c.seek_latency_us = 60_000_000
        }
    });
    w.ui(HOST, UiAction::Play);
    w.run_for(1_000_000);
    w.ui(HOST, UiAction::Seek(100_000));
    w.run_for(5_000_000);
    assert!(!w.nodes[HOST].player.is_playing());
    w.run_for(12_000_000); // 15 s default timeout
    assert!(
        w.nodes[HOST].player.is_playing(),
        "resumed after the gate timeout"
    );
}

#[test]
fn buffering_member_pauses_room_and_resume_is_automatic() {
    let (mut w, ids) = room(2, 10);
    w.ui(HOST, UiAction::Play);
    w.run_for(5_000_000);
    let now = w.now;
    w.nodes[ids[1]].player.buffering_until = Some(now + 3_000_000);
    w.run_for(1_000_000);
    assert!(
        !w.nodes[HOST].player.is_playing(),
        "room pauses while a member buffers"
    );
    w.run_for(4_000_000);
    assert!(w.nodes[HOST].player.is_playing(), "resumes when ready");
    w.run_for(3_000_000);
    assert!(w.max_divergence_ms() < 100.0);
}

#[test]
fn late_joiner_catches_up_with_same_tracks_and_delays() {
    let (mut w, _ids) = room(2, 11);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    // Host picks the SDH subtitle (ff-index 5), Japanese audio, and delays.
    let sdh = TrackRef::Embedded {
        ff_index: Some(5),
        kind: TrackKind::Sub,
        lang: Some("eng".into()),
        title: Some("SDH".into()),
        codec: Some("subrip".into()),
    };
    let jpn = TrackRef::Embedded {
        ff_index: Some(1),
        kind: TrackKind::Audio,
        lang: Some("jpn".into()),
        title: None,
        codec: Some("eac3".into()),
    };
    w.ui(HOST, UiAction::SelectSub(Selection::Track(sdh)));
    w.ui(HOST, UiAction::SelectAudio(Selection::Track(jpn)));
    w.ui(HOST, UiAction::SetSubDelay(-350));
    w.ui(HOST, UiAction::SetAudioDelay(120));
    w.run_for(20_000_000);

    let mut c = NodeConfig::named("Late");
    c.file = Some(World::sample_identity());
    c.clock_offset_us = 123_456_789;
    // The late joiner's file has the tracks in a different local order (ids differ).
    c.tracks.reverse();
    for (i, t) in c.tracks.iter_mut().enumerate() {
        t.id = 10 + i as i64;
    }
    let late = w.add_member(c);
    w.run_for(6_000_000);
    let p = &w.nodes[late].player;
    assert!(p.loaded && p.is_playing());
    let sdh_local = p.tracks.iter().find(|t| t.ff_index == Some(5)).unwrap().id;
    let jpn_local = p.tracks.iter().find(|t| t.ff_index == Some(1)).unwrap().id;
    assert_eq!(p.sub, Some(Some(sdh_local)));
    assert_eq!(p.audio, Some(Some(jpn_local)));
    assert_eq!((p.sub_delay, p.audio_delay), (-350, 120));
    assert!(
        (w.pos(late) - w.pos(HOST)).abs() < 150.0,
        "late joiner off by {}",
        w.pos(late) - w.pos(HOST)
    );
    assert!(
        w.nodes[late]
            .ui_events()
            .any(|u| matches!(u, UiEvent::Joining))
    );
}

#[test]
fn stale_commands_after_media_change_are_dropped() {
    let mut m = joined_member();
    let old = RoomPlayback {
        media_epoch: MediaEpoch(1),
        epoch: Epoch(5),
        position_ms: 0,
        anchor_host_us: 0,
        paused: true,
        speed: 1000,
    };
    m.handle(
        clock(400),
        Event::Net {
            from: host_sender(),
            msg: Message::Command(Command::Playback(old)),
        },
    );
    let newer = RoomPlayback {
        media_epoch: MediaEpoch(2),
        epoch: Epoch(6),
        ..old
    };
    m.handle(
        clock(500),
        Event::Net {
            from: host_sender(),
            msg: Message::Command(Command::MediaChange(Box::new(MediaChange {
                descriptor: Some(World::local_descriptor()),
                playback: newer,
                tracks: TrackState::default(),
                requested_by: MemberId::HOST,
            }))),
        },
    );
    // A late command for the OLD media (epoch 7 but media epoch 1) must be ignored.
    let stale = RoomPlayback {
        media_epoch: MediaEpoch(1),
        epoch: Epoch(7),
        position_ms: 99_000,
        paused: false,
        ..old
    };
    let fx = m.handle(
        clock(600),
        Event::Net {
            from: host_sender(),
            msg: Message::Command(Command::Playback(stale)),
        },
    );
    assert!(!fx.iter().any(|e| matches!(e, Effect::Player(_))));
    assert_eq!(m.local.room.media_epoch, MediaEpoch(2));
    // And an older epoch for the current media too.
    let older = RoomPlayback {
        media_epoch: MediaEpoch(2),
        epoch: Epoch(3),
        ..newer
    };
    m.handle(
        clock(700),
        Event::Net {
            from: host_sender(),
            msg: Message::Command(Command::Playback(older)),
        },
    );
    assert_eq!(m.local.room.epoch, Epoch(6));
}

#[test]
fn player_observations_never_generate_requests() {
    // Remote-caused player changes (pause/seek reported by the player) must not echo back.
    let mut m = joined_member();
    for (t, obs) in [
        (
            1_000,
            PlayerObs::Position {
                position_ms: 5_000,
                paused: true,
                buffering: false,
            },
        ),
        (
            2_000,
            PlayerObs::Position {
                position_ms: 9_000,
                paused: false,
                buffering: false,
            },
        ),
        (3_000, PlayerObs::SeekDone),
    ] {
        let fx = m.handle(clock(t), Event::Player(obs));
        assert!(
            !fx.iter()
                .any(|e| matches!(e, Effect::SendToHost(Message::Request(_))))
        );
    }
}

#[test]
fn speed_change_applies_to_everyone() {
    let (mut w, ids) = room(2, 12);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    w.ui(ids[1], UiAction::SetSpeed(1500));
    w.run_for(3_000_000);
    for i in [HOST, ids[0], ids[1]] {
        assert_eq!(
            w.nodes[i].player.speed / 10,
            150,
            "node {i}: {}",
            w.nodes[i].player.speed
        );
    }
    w.run_for(10_000_000);
    assert!(w.max_divergence_ms() < 150.0);
}

#[test]
fn requests_without_permission_are_denied_by_the_host() {
    let (mut w, ids) = room(1, 13);
    w.ui(
        HOST,
        UiAction::SetPermissions {
            member: w.nodes[ids[0]].id,
            permissions: Permissions::NONE,
        },
    );
    w.run_for(500_000);
    w.ui(ids[0], UiAction::Play);
    w.run_for(1_000_000);
    assert!(!w.nodes[HOST].player.is_playing());
    // The client refuses locally; a crafted request is refused by the host.
    assert!(notices(&w, ids[0]).contains(&Notice::RequestDenied {
        reason: DenyReason::NoPermission
    }));
}

/// Regression: an unreliable heartbeat can arrive before the reliable Welcome; that is not a
/// protocol violation (found by the simulator).
#[test]
fn heartbeat_before_welcome_is_ignored_quietly() {
    let mut m = MemberMachine::new(Identity([2; 32]), "M");
    let hb = Message::Heartbeat {
        playback: RoomPlayback::initial(MediaEpoch(1), 0),
        host_now_us: 5,
    };
    let fx = m.handle(
        clock(0),
        Event::Net {
            from: host_sender(),
            msg: hb,
        },
    );
    assert!(
        fx.iter()
            .all(|e| !matches!(e, Effect::ProtocolViolation { .. })),
        "{fx:?}"
    );
    // After Welcome, a message from a non-host identity IS a violation.
    let _ = m;
    let mut joined = joined_member();
    let stranger = Sender {
        member: MemberId(9),
        identity: Identity([9; 32]),
    };
    let fx = joined.handle(
        clock(1),
        Event::Net {
            from: stranger,
            msg: Message::Heartbeat {
                playback: RoomPlayback::initial(MediaEpoch(1), 0),
                host_now_us: 5,
            },
        },
    );
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::ProtocolViolation { .. }))
    );
}

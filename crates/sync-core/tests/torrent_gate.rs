#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! ADR 0012: READY means "has the data I need now" (the read window at the room position), the
//! gate names the real reason with the data, every member's torrent follows the ROOM position,
//! and the buffering popup is this window's own real stall only.

mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::sim::World;
use syncwatch_sync_core::*;

const M1: &str = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=one.mkv";

fn ih() -> Infohash {
    magnet::parse(M1).unwrap().infohash
}

fn torrent() -> MediaDescriptor {
    MediaDescriptor::Torrent(TorrentMedia {
        infohash: ih(),
        file_index: 0,
        name: "one.mkv".into(),
        size: 700_000_000,
        magnet: M1.into(),
    })
}

fn ready() -> Event {
    Event::Torrent(TorrentEvent::Window(Some(WindowState {
        ready: true,
        at_ms: 0,
        missing: 0,
        permille: 1000,
    })))
}

fn short(at_ms: i64) -> Event {
    Event::Torrent(TorrentEvent::Window(Some(WindowState {
        ready: false,
        at_ms,
        missing: 3_000_000,
        permille: 600,
    })))
}

fn progress(phase: TorrentPhase, permille: u16) -> Event {
    Event::Torrent(TorrentEvent::Progress { phase, permille })
}

/// Host + `n` members streaming the torrent at 40% overall, window on disk, playing.
fn streaming(n: usize, seed: u64) -> (World, Vec<usize>) {
    let (mut w, ids) = room(n, seed);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    for i in std::iter::once(HOST).chain(ids.iter().copied()) {
        w.deliver_event(i, progress(TorrentPhase::Streaming, 400));
        w.deliver_event(i, Event::Torrent(TorrentEvent::Playable));
        w.deliver_event(i, ready());
    }
    w.run_for(2_000_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    for i in std::iter::once(HOST).chain(ids.iter().copied()) {
        assert!(w.nodes[i].player.is_playing(), "node {i} plays");
    }
    (w, ids)
}

fn last_gate(w: &World, idx: usize) -> Option<GateInfo> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Gate(g) => Some(g.clone()),
            _ => None,
        })
        .last()
}

fn stalls(w: &World, idx: usize, since: i64) -> Vec<(i64, bool, Option<BufferWait>)> {
    w.nodes[idx]
        .effects
        .iter()
        .filter_map(|(t, e)| match e {
            Effect::Ui(UiEvent::Stall { stalled, wait }) if *t >= since => {
                Some((*t, *stalled, *wait))
            }
            _ => None,
        })
        .collect()
}

fn targets(w: &World, idx: usize, since: i64) -> Vec<(i64, MediaMs, bool, bool)> {
    w.nodes[idx]
        .effects
        .iter()
        .filter_map(|(t, e)| match e {
            Effect::TorrentTarget {
                position_ms,
                playing,
                retarget,
                ..
            } if *t >= since => Some((*t, *position_ms, *playing, *retarget)),
            _ => None,
        })
        .collect()
}

#[test]
fn downloading_40_percent_with_the_window_on_disk_never_holds_the_gate() {
    let (mut w, ids) = streaming(1, 90);
    let m = ids[0];
    // Even with "wait for everyone": overall progress is not readiness.
    w.ui(HOST, UiAction::SetWaitForEveryone(true));
    let since = w.now;
    for p in [410u16, 420, 430] {
        w.deliver_event(m, progress(TorrentPhase::Streaming, p));
        w.deliver_event(m, ready());
        w.run_for(1_000_000);
    }
    // A pause and resume pass the gate at once.
    w.ui(HOST, UiAction::Pause);
    w.run_for(500_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(2_000_000);
    assert!(w.nodes[HOST].player.is_playing() && w.nodes[m].player.is_playing());
    let blocked = w.nodes[HOST]
        .effects
        .iter()
        .filter(|(t, _)| *t >= since)
        .any(|(_, e)| matches!(e, Effect::Ui(UiEvent::Gate(g)) if g.waiting.iter().any(|(_, r)| matches!(r, NotReadyReason::Downloading | NotReadyReason::Buffering(_)))));
    assert!(
        !blocked,
        "never 'downloading' or 'buffering' for a member whose window is there"
    );
    assert!(stalls(&w, m, since).iter().all(|(_, s, _)| !s), "no popup");
}

#[test]
fn a_missing_window_holds_the_gate_with_the_reason_and_the_data() {
    let (mut w, ids) = streaming(2, 91);
    let (m, other) = (ids[0], ids[1]);
    let mid = w.nodes[m].id;
    let since = w.now;
    w.deliver_event(m, short(750_000));
    w.run_for(1_500_000);
    assert!(!w.nodes[HOST].player.is_playing(), "the room waits");
    // The host's gate: the member, "buffering", at 12:30, with what is missing.
    let g = last_gate(&w, HOST).unwrap();
    assert!(g.active);
    let wait = g
        .waiting
        .iter()
        .find_map(|(id, r)| match r {
            NotReadyReason::Buffering(Some(b)) if *id == mid => Some(*b),
            _ => None,
        })
        .expect("buffering with the target");
    assert_eq!(wait.at_ms, 750_000);
    assert_eq!(wait.permille, 600);
    assert!((3_000_000..3_000_000 + 512 * 1024).contains(&wait.missing));
    // The host's members list shows it too.
    let listed = w.nodes[HOST]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Members(v) => v
                .iter()
                .find(|i| i.id == mid)
                .map(|i| (i.buffering, i.wait)),
            _ => None,
        })
        .last()
        .unwrap();
    assert_eq!(listed, (true, Some(wait)));
    // The buffering member gets its popup with the data; nobody else does.
    let st = stalls(&w, m, since);
    assert!(
        st.last()
            .is_some_and(|(_, s, w)| *s && w.is_some_and(|b| b.at_ms == 750_000)),
        "{st:?}"
    );
    for i in [HOST, other] {
        assert!(
            stalls(&w, i, since).iter().all(|(_, s, _)| !s),
            "node {i}: no popup for another member's buffering"
        );
    }
    // Members see who they wait for (the UI words it "Waiting for X to catch up").
    let mg = last_gate(&w, other).unwrap();
    assert!(
        mg.active
            && mg
                .waiting
                .iter()
                .any(|(id, r)| *id == mid && matches!(r, NotReadyReason::Buffering(_)))
    );
    // The window arrives: ready at once, the room resumes, the popup goes.
    w.deliver_event(m, ready());
    w.run_for(2_000_000);
    assert!(w.nodes[HOST].player.is_playing() && w.nodes[m].player.is_playing());
    assert!(!last_gate(&w, HOST).unwrap().active);
    assert!(stalls(&w, m, since).last().is_some_and(|(_, s, _)| !s));
}

#[test]
fn a_stalled_member_cannot_hold_the_room_forever() {
    let (mut w, ids) = streaming(1, 92);
    let m = ids[0];
    w.deliver_event(m, short(10_000));
    w.run_for(1_500_000);
    assert!(!w.nodes[HOST].player.is_playing());
    w.run_for(16_000_000); // the 15 s gate timeout
    assert!(
        w.nodes[HOST].player.is_playing(),
        "resumed without X after the timeout"
    );
    // Still short: it is excused and does not pause the room again.
    w.deliver_event(m, short(30_000));
    w.run_for(3_000_000);
    assert!(w.nodes[HOST].player.is_playing());
}

#[test]
fn every_member_follows_the_room_position_even_before_its_player_exists() {
    let (mut w, ids) = room(1, 93);
    let m = ids[0];
    // The room is already at 10:00 when the torrent starts.
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(),
            own_file: false,
        },
    );
    w.run_for(100_000);
    w.ui(HOST, UiAction::Seek(600_000));
    let t0 = w.now;
    w.run_for(1_000_000);
    // Accepting (no player yet): the engine is told the room position first.
    let t = targets(&w, m, t0);
    assert!(
        t.iter()
            .any(|(_, p, playing, r)| *p == 600_000 && !playing && *r),
        "re-target at the room position on start: {t:?}"
    );
    let first_start = w.nodes[m]
        .effects
        .iter()
        .position(|(_, e)| matches!(e, Effect::StartTorrent { .. }))
        .unwrap();
    let first_target = w.nodes[m]
        .effects
        .iter()
        .position(|(_, e)| matches!(e, Effect::TorrentTarget { .. }))
        .unwrap();
    assert!(
        first_target < first_start,
        "the target is known before the engine starts"
    );
    // Host plays (the member is not loaded and does not block) and seeks: the member's engine
    // follows each room change at once, with a re-target.
    w.deliver_event(HOST, Event::Torrent(TorrentEvent::Playable));
    w.run_for(1_000_000);
    let t1 = w.now;
    w.ui(HOST, UiAction::Play);
    w.run_for(1_500_000);
    assert!(
        targets(&w, m, t1)
            .iter()
            .any(|(_, p, playing, r)| *playing && *r && (*p - 600_000).abs() < 2_000)
    );
    let t2 = w.now;
    w.ui(HOST, UiAction::Seek(900_000));
    w.run_for(300_000);
    assert!(
        targets(&w, m, t2)
            .iter()
            .any(|(_, p, _, r)| *r && (*p - 900_000).abs() < 1_000)
    );
    let t3 = w.now;
    w.ui(HOST, UiAction::Pause);
    w.run_for(300_000);
    assert!(
        targets(&w, m, t3)
            .iter()
            .any(|(_, _, playing, r)| !playing && *r)
    );
    // Heartbeats refresh it without a re-target.
    let t4 = w.now;
    w.run_for(3_000_000);
    let hb = targets(&w, m, t4);
    assert!(hb.len() >= 2 && hb.iter().all(|(_, _, _, r)| !r), "{hb:?}");
    // The host's own engine follows the room too.
    assert!(!targets(&w, HOST, t4).is_empty());
    // A member that never loaded the file still maps time to bytes with the host's duration.
    assert!(!w.nodes[m].peer.local().is_loaded());
    assert!(w.nodes[m].peer.local().media_duration_hint().is_some());
}

#[test]
fn the_popup_clears_on_completion_and_on_a_media_change() {
    let (mut w, ids) = streaming(2, 94);
    let (a, b) = (ids[0], ids[1]);
    let since = w.now;
    w.deliver_event(a, short(5_000));
    w.run_for(1_500_000);
    assert!(stalls(&w, a, since).last().is_some_and(|(_, s, _)| *s));
    // The torrent completes (and the stream hands over to the finished file): gone at once.
    let t = w.now;
    w.deliver_event(a, progress(TorrentPhase::Complete, 1000));
    let st = stalls(&w, a, t);
    assert!(
        st.first().is_some_and(|(at, s, _)| !s && *at == t),
        "cleared in the same step: {st:?}"
    );
    // A late window report of the old stream can no longer bring it back.
    w.deliver_event(a, short(5_000));
    w.run_for(2_000_000);
    assert!(stalls(&w, a, t).iter().all(|(_, s, _)| !s));
    // Another member stalls; then the host changes the source: cleared on the media change.
    let t2 = w.now;
    w.deliver_event(b, short(9_000));
    w.run_for(1_500_000);
    assert!(stalls(&w, b, t2).last().is_some_and(|(_, s, _)| *s));
    let t3 = w.now;
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: MediaDescriptor::LocalFile(LocalMedia {
                name: "x.mkv".into(),
                identity: World::sample_identity(),
            }),
            own_file: true,
        },
    );
    w.run_for(500_000);
    assert!(stalls(&w, b, t3).iter().any(|(_, s, _)| !s));
    assert!(stalls(&w, b, t3).last().is_some_and(|(_, s, _)| !s));
}

#[test]
fn a_short_cache_toggle_never_shows_the_popup_but_a_real_stall_does() {
    let (mut w, ids) = streaming(1, 95);
    let m = ids[0];
    let since = w.now;
    let now = w.now;
    w.nodes[m].player.buffering_until = Some(now + 400_000);
    w.run_for(3_000_000);
    assert!(
        stalls(&w, m, since).iter().all(|(_, s, _)| !s),
        "a 0.4 s toggle shows nothing"
    );
    let t = w.now;
    w.nodes[m].player.buffering_until = Some(t + 3_000_000);
    w.run_for(1_000_000);
    let st = stalls(&w, m, t);
    assert!(
        st.iter().any(|(at, s, _)| *s && *at - t >= 700_000),
        "shown after the delay: {st:?}"
    );
    w.run_for(4_000_000);
    assert!(
        stalls(&w, m, t).last().is_some_and(|(_, s, _)| !s),
        "gone after recovery"
    );
    assert!(w.nodes[HOST].player.is_playing() && w.nodes[m].player.is_playing());
}

/// ADR 0012 (revised): the stream is offered at once. A member joining a room that already
/// plays opens its player directly at the room's position, is "downloading" (never blocking
/// the room) until the player has opened the file, then "ready" and in sync.
#[test]
fn a_member_joining_a_playing_room_opens_at_the_room_position_and_plays() {
    let (mut w, ids) = room(1, 96);
    let m = ids[0];
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: torrent(),
            own_file: false,
        },
    );
    // The host plays from 5:00 before the member's stream exists.
    w.deliver_event(HOST, Event::Torrent(TorrentEvent::Playable));
    w.run_for(1_000_000);
    w.ui(HOST, UiAction::Seek(300_000));
    w.run_for(300_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    assert!(w.nodes[HOST].player.is_playing());
    // The member's stream is offered: it loads at once, at about the room's position.
    let t = w.now;
    w.deliver_event(m, progress(TorrentPhase::Streaming, 10));
    // Its window at the room position is not there yet when its player opens.
    w.deliver_event(m, short(303_000));
    w.deliver_event(m, Event::Torrent(TorrentEvent::Playable));
    let load = w.nodes[m]
        .effects
        .iter()
        .filter(|(at, _)| *at >= t)
        .find_map(|(_, e)| match e {
            Effect::Player(PlayerCmd::LoadTorrentStream { start_ms, .. }) => Some(*start_ms),
            _ => None,
        })
        .expect("loads at once");
    assert!(
        (load - 303_300).abs() < 1_500,
        "opens at the room position, not 0: {load}"
    );
    // Still "downloading" until the player opened the file: the room keeps playing.
    assert!(matches!(
        w.nodes[m].peer.local().status(None, None).media,
        MediaStatus::Downloading { .. }
    ));
    // The player opened (file loaded) while the window is still missing: the member is
    // catching up and must not pause the room.
    w.run_for(1_500_000);
    assert_eq!(
        w.nodes[m].peer.local().status(None, None).media,
        MediaStatus::Ready
    );
    assert!(w.nodes[m].peer.local().status(None, None).buffering);
    let paused = w.nodes[HOST]
        .player
        .log
        .iter()
        .any(|(at, c)| *at >= t && *c == PlayerCmd::SetPaused(true));
    assert!(!paused, "a joining member never pauses the room");
    w.deliver_event(m, ready());
    w.run_for(3_000_000);
    assert!(
        w.nodes[HOST].player.is_playing(),
        "the member's opening never paused the room"
    );
    assert_eq!(
        w.nodes[m].peer.local().status(None, None).media,
        MediaStatus::Ready
    );
    assert!(w.nodes[m].player.is_playing());
    w.run_for(3_000_000);
    assert!(
        w.max_divergence_ms() < 100.0,
        "in sync: {}",
        w.max_divergence_ms()
    );
}

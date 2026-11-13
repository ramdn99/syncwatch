#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! ADR 0012 bug reproduction (kept as a regression test). It uses only the API from before the
//! fix, so the same file ran against the old code, where it FAILED: a member at 40% whose current
//! region is on disk held the room, because the gate took the player's raw paused-for-cache flag
//! as "buffering" at once. Brief cache toggles while playing from data on disk then paused
//! everyone ("Waiting for X (buffering)"), over and over.

mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::*;

const M1: &str = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=one.mkv";

#[test]
fn a_partial_member_whose_region_is_on_disk_never_holds_the_room() {
    let (mut w, ids) = room(1, 80);
    let m = ids[0];
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: MediaDescriptor::Torrent(TorrentMedia {
                infohash: magnet::parse(M1).unwrap().infohash,
                file_index: 0,
                name: "one.mkv".into(),
                size: 700_000_000,
                magnet: M1.into(),
            }),
            own_file: false,
        },
    );
    w.run_for(1_000_000);
    // Both stream while downloading; the member is at 40% overall.
    for i in [HOST, m] {
        w.deliver_event(
            i,
            Event::Torrent(TorrentEvent::Progress {
                phase: TorrentPhase::Streaming,
                permille: 400,
            }),
        );
        w.deliver_event(i, Event::Torrent(TorrentEvent::Playable));
    }
    w.run_for(2_000_000);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    assert!(w.nodes[HOST].player.is_playing() && w.nodes[m].player.is_playing());
    let since = w.now;
    // Ordinary playback from data on disk: the stream's cache flag toggles briefly (< 0.5 s)
    // now and then, as mpv's does on the loopback stream.
    for _ in 0..5 {
        let now = w.now;
        w.nodes[m].player.buffering_until = Some(now + 300_000);
        w.run_for(2_000_000);
    }
    let gate = w.nodes[HOST]
        .ui_events()
        .filter(|u| matches!(u, UiEvent::Gate(g) if g.active))
        .count();
    let reported = w.nodes[m]
        .sent
        .iter()
        .filter(|(t, msg)| *t >= since && matches!(msg, Message::Status(s) if s.buffering))
        .count();
    let host_paused = w.nodes[HOST]
        .player
        .log
        .iter()
        .any(|(t, c)| *t >= since && *c == PlayerCmd::SetPaused(true));
    assert_eq!(
        (gate, reported, host_paused),
        (0, 0, false),
        "the member never reports buffering, the gate never opens, the room never pauses"
    );
    assert!(w.nodes[HOST].player.is_playing() && w.nodes[m].player.is_playing());
}

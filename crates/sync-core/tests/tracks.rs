#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! Track control (host only), external file transfer, and the outbound priority queue.

mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::queue::OutboundQueue;
use syncwatch_sync_core::sim::World;
use syncwatch_sync_core::*;

fn sdh() -> TrackRef {
    TrackRef::Embedded {
        ff_index: Some(5),
        kind: TrackKind::Sub,
        lang: Some("eng".into()),
        title: Some("SDH".into()),
        codec: Some("subrip".into()),
    }
}

/// A member forging a host track command is rejected and logged; nobody's tracks change.
#[test]
fn forged_track_command_rejected() {
    let (mut w, ids) = room(2, 21);
    let forged = Message::Command(Command::Tracks(TrackState {
        track_epoch: TrackEpoch(99),
        subtitle: Selection::Off,
        ..TrackState::default()
    }));
    let from = Sender {
        member: w.nodes[ids[0]].id,
        identity: w.nodes[ids[0]].identity,
    };
    w.deliver_event(
        HOST,
        Event::Net {
            from,
            msg: forged.clone(),
        },
    );
    w.run_for(500_000);
    assert!(
        w.nodes[HOST]
            .violations
            .contains(&"forged track command from a member")
    );
    assert_eq!(
        host(&w).tracks().track_epoch,
        TrackEpoch(1),
        "host state unchanged"
    );
    assert!(!w.nodes[ids[1]].sent.is_empty());

    // Member side: a track command that does not come from the host identity is refused.
    let impostor = Sender {
        member: w.nodes[ids[0]].id,
        identity: w.nodes[ids[0]].identity,
    };
    w.deliver_event(
        ids[1],
        Event::Net {
            from: impostor,
            msg: forged,
        },
    );
    assert!(
        w.nodes[ids[1]]
            .violations
            .contains(&"message not from the room host")
    );
    assert_eq!(
        member(&w, ids[1]).local.tracks.desired.track_epoch,
        TrackEpoch(1)
    );
    // Moderation from a member is refused too.
    let modr = Message::Command(Command::ChatModeration(Moderation::Clear));
    w.deliver_event(HOST, Event::Net { from, msg: modr });
    assert!(
        w.nodes[HOST]
            .violations
            .contains(&"forged moderation command from a member")
    );
}

#[test]
fn members_cannot_change_tracks_and_have_no_permission_for_it() {
    let (mut w, ids) = room(1, 22);
    w.ui(ids[0], UiAction::SelectSub(Selection::Off));
    assert!(notices(&w, ids[0]).contains(&Notice::RequestDenied {
        reason: DenyReason::NoPermission
    }));
    assert!(
        !w.nodes[ids[0]]
            .sent
            .iter()
            .any(|(_, m)| matches!(m, Message::Command(_)))
    );
}

#[test]
fn host_track_change_applies_to_everyone_immediately_without_seek() {
    let (mut w, ids) = room(3, 23);
    w.ui(HOST, UiAction::Play);
    w.run_for(3_000_000);
    let seeks_before: Vec<usize> = ids
        .iter()
        .map(|i| {
            w.nodes[*i]
                .player
                .log
                .iter()
                .filter(|(_, c)| matches!(c, PlayerCmd::Seek(_)))
                .count()
        })
        .collect();
    w.ui(HOST, UiAction::SelectSub(Selection::Track(sdh())));
    w.ui(HOST, UiAction::SetSubDelay(250));
    w.run_for(400_000);
    for (k, i) in ids.iter().enumerate() {
        let p = &w.nodes[*i].player;
        assert_eq!(p.sub, Some(Some(3)), "member {k} selects SDH (local id 3)");
        assert_eq!(p.sub_delay, 250);
        assert!(p.is_playing(), "no ready gate for track changes");
        let seeks = p
            .log
            .iter()
            .filter(|(_, c)| matches!(c, PlayerCmd::Seek(_)))
            .count();
        assert_eq!(seeks, seeks_before[k], "track change must not seek");
    }
    // Subtitles off.
    w.ui(HOST, UiAction::SelectSub(Selection::Off));
    w.run_for(400_000);
    assert!(ids.iter().all(|i| w.nodes[*i].player.sub == Some(None)));
}

#[test]
fn unresolvable_track_warns_without_blocking() {
    let (mut w, ids) = room_with(1, 24, |_, c| {
        c.tracks.retain(|t| t.title.as_deref() != Some("SDH"))
    });
    w.ui(HOST, UiAction::Play);
    w.ui(HOST, UiAction::SelectSub(Selection::Track(sdh())));
    w.run_for(2_000_000);
    assert!(w.nodes[ids[0]].ui_events().any(|u| matches!(
        u,
        UiEvent::TrackUnresolved {
            kind: TrackKind::Sub
        }
    )));
    assert_eq!(
        member(&w, ids[0]).local.tracks.status,
        TrackSyncStatus::Unresolved
    );
    assert!(w.nodes[ids[0]].player.is_playing(), "non-blocking");
}

#[test]
fn external_subtitle_transfer_is_verified_and_selected_everywhere() {
    let (mut w, ids) = room(2, 25);
    let bytes: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 256) as u8).collect();
    let id = w.host_add_external(ExternalKind::Subtitle, "fansub.ass", "ass", bytes.clone());
    w.ui(
        HOST,
        UiAction::SelectSub(Selection::Track(TrackRef::External { external_id: id })),
    );
    w.run_for(3_000_000);
    for i in &ids {
        assert_eq!(
            w.nodes[*i].ext_files.get(&id),
            Some(&bytes),
            "byte-exact copy"
        );
        assert!(w.nodes[*i].ext_complete.contains(&id));
        let p = &w.nodes[*i].player;
        let local = p
            .tracks
            .iter()
            .find(|t| t.external_id == Some(id))
            .expect("added to player")
            .id;
        assert_eq!(p.sub, Some(Some(local)));
    }
    // Host plays it from its own copy.
    assert!(
        w.nodes[HOST]
            .player
            .tracks
            .iter()
            .any(|t| t.external_id == Some(id))
    );
}

#[test]
fn subtitle_preempts_audio_download_and_audio_resumes() {
    let (mut w, ids) = room(1, 26);
    let audio: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    let sub: Vec<u8> = (0..40_000u32).map(|i| (i % 7) as u8).collect();
    let a = w.host_add_external(ExternalKind::Audio, "dub.mka", "mka", audio.clone());
    w.run_for(200_000); // audio download under way
    let s = w.host_add_external(ExternalKind::Subtitle, "subs.srt", "srt", sub.clone());
    w.run_for(200_000);
    let done = &w.nodes[ids[0]].ext_complete;
    assert_eq!(done.first(), Some(&s), "subtitle completes first");
    w.run_for(5_000_000);
    assert_eq!(
        w.nodes[ids[0]].ext_files.get(&a),
        Some(&audio),
        "audio resumed and verified"
    );
    assert!(
        w.nodes[ids[0]].violations.is_empty(),
        "{:?}",
        w.nodes[ids[0]].violations
    );
}

#[test]
fn corrupted_external_is_discarded() {
    let (mut w, ids) = room(1, 27);
    let bytes = vec![1u8; 50_000];
    let id = w.host_add_external(ExternalKind::Subtitle, "x.srt", "srt", bytes);
    // The host's file changes after hashing (or a malicious host lies about the hash).
    w.nodes[HOST].ext_files.insert(id, vec![2u8; 50_000]);
    w.run_for(2_000_000);
    assert!(!w.nodes[ids[0]].ext_complete.contains(&id));
    assert!(
        w.nodes[ids[0]]
            .ui_events()
            .any(|u| *u == UiEvent::ExternalFailed(id))
    );
    assert!(
        !w.nodes[ids[0]]
            .player
            .tracks
            .iter()
            .any(|t| t.external_id == Some(id))
    );
}

/// File-transfer messages only ever reference manifest entries, in both directions.
#[test]
fn file_transfer_references_only_manifest_entries() {
    let (mut w, ids) = room(1, 28);
    let from = Sender {
        member: w.nodes[ids[0]].id,
        identity: w.nodes[ids[0]].identity,
    };
    // A member asks for something that is not in the manifest (e.g. the main media).
    w.deliver_event(
        HOST,
        Event::Net {
            from,
            msg: Message::FileRequest {
                external_id: ExternalId(77),
                offset: 0,
            },
        },
    );
    assert!(
        w.nodes[HOST]
            .violations
            .contains(&"file request outside the external manifest")
    );
    assert!(
        !w.nodes[HOST]
            .effects
            .iter()
            .any(|(_, e)| matches!(e, Effect::StartUpload { .. }))
    );
    // The host (or anyone) pushes unsolicited bytes to a member.
    let host_sender = Sender {
        member: w.nodes[HOST].id,
        identity: w.nodes[HOST].identity,
    };
    w.deliver_event(
        ids[0],
        Event::Net {
            from: host_sender,
            msg: Message::FileChunk {
                external_id: ExternalId(77),
                offset: 0,
                data: vec![0; 1000],
            },
        },
    );
    assert!(
        w.nodes[ids[0]]
            .violations
            .contains(&"file chunk outside the external manifest")
    );
    assert!(w.nodes[ids[0]].ext_files.is_empty(), "nothing written");
    // A member cannot push chunks to the host either.
    w.deliver_event(
        HOST,
        Event::Net {
            from,
            msg: Message::FileChunk {
                external_id: ExternalId(1),
                offset: 0,
                data: vec![0; 10],
            },
        },
    );
    assert!(
        w.nodes[HOST]
            .violations
            .contains(&"file chunk from a member")
    );
    // Every transfer message in the whole session referenced a manifest id.
    let id = w.host_add_external(ExternalKind::Subtitle, "y.srt", "srt", vec![3u8; 70_000]);
    w.run_for(2_000_000);
    for n in &w.nodes {
        for (_, m) in &n.sent {
            if let Message::FileChunk { external_id, .. }
            | Message::FileRequest { external_id, .. } = m
            {
                assert_eq!(*external_id, id);
            }
        }
    }
}

#[test]
fn member_can_decline_a_download_and_cached_files_are_skipped() {
    let (mut w, ids) = room(2, 29);
    let audio: Vec<u8> = vec![9u8; 2_000_000];
    let a = w.host_add_external(ExternalKind::Audio, "dub.mka", "mka", audio);
    // Member 0 declines; member 1 already holds an identical file.
    w.run_for(50_000);
    w.ui(ids[0], UiAction::CancelExternalDownload(a));
    w.deliver_event(ids[1], Event::ExternalCached(a));
    w.run_for(3_000_000);
    assert!(!w.nodes[ids[0]].ext_complete.contains(&a));
    assert!(
        !w.nodes[ids[0]].ext_files.contains_key(&a),
        "partial file discarded"
    );
    assert!(
        w.nodes[ids[1]]
            .sent
            .iter()
            .any(|(_, m)| *m == Message::FileHave { external_id: a })
    );
    let chunks_to_1 = w.nodes[HOST]
        .sent
        .iter()
        .filter(|(_, m)| matches!(m, Message::FileChunk { .. }))
        .count();
    assert!(
        chunks_to_1 < 2_000_000 / limits::CHUNK_BYTES,
        "uploads stopped for both members"
    );
    // The cached copy is used.
    assert!(
        w.nodes[ids[1]]
            .player
            .tracks
            .iter()
            .any(|t| t.external_id == Some(a))
    );
}

#[test]
fn external_limits_and_format_allowlist() {
    let (mut w, _ids) = room(1, 30);
    let before = host(&w).tracks().manifest.len();
    // Executable disguised as a subtitle.
    w.ui(
        HOST,
        UiAction::AddExternal {
            kind: ExternalKind::Subtitle,
            display_name: "x.exe".into(),
            format: "exe".into(),
            size: 10,
            blake3: [0; 32],
        },
    );
    // Over the 20 MB subtitle ceiling.
    w.ui(
        HOST,
        UiAction::AddExternal {
            kind: ExternalKind::Subtitle,
            display_name: "big.ass".into(),
            format: "ass".into(),
            size: 21 * 1024 * 1024,
            blake3: [0; 32],
        },
    );
    // Path in the name.
    w.ui(
        HOST,
        UiAction::AddExternal {
            kind: ExternalKind::Subtitle,
            display_name: "..\\evil.ass".into(),
            format: "ass".into(),
            size: 10,
            blake3: [0; 32],
        },
    );
    assert_eq!(host(&w).tracks().manifest.len(), before);
}

/// No artificial rate limiter: bulk data fills every free slot of the link, yet a control
/// message waits at most one chunk transmission.
#[test]
fn no_rate_limiter_and_control_latency_stays_bounded() {
    let mut q = OutboundQueue::default();
    let chunk = |i: u64| Message::FileChunk {
        external_id: ExternalId(1),
        offset: i * 16_384,
        data: vec![0; 16_384],
    };
    let mut next_chunk = 0u64;
    let total_chunks = 2_000u64;
    let low_watermark = 8;
    let mut sent_bulk = 0u64;
    let mut idle_slots_with_bulk_left = 0;
    let mut control_waits = Vec::new();
    let mut control_enqueued_at: Option<u64> = None;
    for slot in 0..3_000u64 {
        // Uploader refills below the low watermark (bufferedAmount style), never throttles.
        while q.bulk_len() < low_watermark && next_chunk < total_chunks {
            q.push(chunk(next_chunk));
            next_chunk += 1;
        }
        if slot % 97 == 0 {
            q.push(Message::Request(Request::Pause));
            control_enqueued_at = Some(slot);
        }
        if slot % 31 == 0 {
            q.push(Message::ChatSend {
                client_id: 1,
                text: "hi".into(),
                claimed_media_ms: None,
            });
        }
        match q.pop() {
            Some(Message::Request(_)) => {
                if let Some(t) = control_enqueued_at.take() {
                    control_waits.push(slot - t);
                }
            }
            Some(Message::FileChunk { .. }) => sent_bulk += 1,
            Some(_) => {}
            None => {
                if sent_bulk < total_chunks {
                    idle_slots_with_bulk_left += 1;
                }
            }
        }
    }
    assert_eq!(sent_bulk, total_chunks, "the whole file went through");
    assert_eq!(
        idle_slots_with_bulk_left, 0,
        "no slot was left idle while bulk data was waiting"
    );
    assert!(
        control_waits.iter().all(|w| *w == 0),
        "control goes out in the next slot: {control_waits:?}"
    );
    let _ = World::sample_identity();
}

//! ADR 0009 approval condition: with a THROTTLED seeder, the tail cursor and the background
//! fill must not stall playback. Runs the same scenario with the exclusive playback window on
//! (the design) and off (the control) and compares time-to-prebuffer after a seek and stall
//! time while "playing" at a constant rate.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

mod common;
use common::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use syncwatch_torrent::{EngineEvent, EngineHandle, StopReason};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MIB: u64 = 1024 * 1024;

/// A player reading `[start, ..)` at `rate` bytes/s for `secs`. Returns (time to the first
/// `first` bytes, total stall time: time spent waiting beyond the playback schedule).
async fn play(url: &str, start: u64, rate: u64, secs: u64, first: u64) -> (Duration, Duration) {
    let u = url::Url::parse(url).unwrap();
    let mut s = tokio::net::TcpStream::connect((u.host_str().unwrap(), u.port().unwrap()))
        .await
        .unwrap();
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nRange: bytes={start}-\r\nConnection: close\r\n\r\n",
        u.path(),
        u.host_str().unwrap(),
        u.port().unwrap()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let t0 = Instant::now();
    let mut buf = vec![0u8; 64 * 1024];
    let mut got: u64 = 0;
    let mut head_done = false;
    let mut pending = Vec::new();
    let mut ttf = None;
    let mut stall = Duration::ZERO;
    // Playback clock starts when the prebuffer is there; afterwards bytes are "consumed" at
    // `rate`; a stall is time where the consumer is ahead of what has arrived.
    let mut play_start: Option<Instant> = None;
    loop {
        if let Some(ps) = play_start
            && ps.elapsed() >= Duration::from_secs(secs)
        {
            break;
        }
        let r = tokio::time::timeout(Duration::from_millis(50), s.read(&mut buf)).await;
        if let Ok(Ok(n)) = r {
            if n == 0 {
                break;
            }
            if !head_done {
                pending.extend_from_slice(&buf[..n]);
                if let Some(p) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
                    head_done = true;
                    got += (pending.len() - p - 4) as u64;
                }
            } else {
                got += n as u64;
            }
        }
        if ttf.is_none() && got >= first {
            ttf = Some(t0.elapsed());
            play_start = Some(Instant::now());
        }
        if let Some(ps) = play_start {
            let needed = first + (ps.elapsed().as_secs_f64() * rate as f64) as u64;
            if got < needed {
                stall += Duration::from_millis(50);
            }
        }
        if ttf.is_none() && t0.elapsed() > Duration::from_secs(60) {
            break;
        }
    }
    (ttf.unwrap_or(Duration::from_secs(60)), stall)
}

async fn scenario(exclusive: bool) -> (Duration, Duration) {
    syncwatch_ice_config::install_crypto_provider();
    let dir = tmp(&format!("window-{exclusive}"));
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = data(48 * MIB as usize, 11);
    std::fs::write(src.join("w.mkv"), &bytes).unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (hurl, _http) = http_tracker(peers.clone()).await;
    // Throttled seeder: 1.5 MiB/s.
    let sd = seeder(&src.join("w.mkv"), vec![hurl.clone()], Some(1536 * 1024)).await;
    peers.lock().unwrap().push(sd.addr);
    let mut cfg = engine_cfg(dir.join("cache"), free_port());
    cfg.policy.read_ahead = 3 * MIB;
    cfg.policy.calm_us = 1_000_000;
    cfg.policy.exclusive_playback = exclusive;
    let (eng, mut ev) = EngineHandle::spawn(cfg);
    let mag = magnet(&sd.infohash, "w.mkv", &[hurl]);
    eng.start(media(sd.infohash, 0, "w.mkv", bytes.len() as u64, mag));
    let mut seen = Vec::new();
    let url = match wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("{e:?}"),
    };
    // Drain events in the background (the engine must never block on them).
    let drain = tokio::spawn(async move { while ev.recv().await.is_some() {} });
    // Play from the start for a while (the window fills; tail/fill come back), then seek.
    let _ = play(&url, 0, 700 * 1024, 4, MIB).await;
    let r = play(&url, 30 * MIB, 700 * 1024, 8, MIB).await;
    eng.stop(sd.infohash, StopReason::Leave).await;
    drain.abort();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn playback_window_beats_competing_streams_with_a_throttled_seeder() {
    let (ttf_x, stall_x) = scenario(true).await;
    let (ttf_c, stall_c) = scenario(false).await;
    eprintln!(
        "WINDOW exclusive: prebuffer after seek {ttf_x:?}, stall {stall_x:?} | control (tail+fill compete): prebuffer {ttf_c:?}, stall {stall_c:?}"
    );
    // 1 MiB at 1.5 MiB/s is ~0.7 s; allow piece/handshake overhead.
    assert!(
        ttf_x < Duration::from_secs(3),
        "exclusive prebuffer after seek: {ttf_x:?}"
    );
    assert!(
        stall_x <= Duration::from_millis(500),
        "no stalls with the exclusive window: {stall_x:?}"
    );
    // The control stalls while tail and fill take bandwidth from playback.
    assert!(
        stall_x <= stall_c,
        "exclusive stalls no more than the control: {stall_x:?} vs {stall_c:?}"
    );
    let _ = ttf_c;
}

/// ADR 0012: a member whose player has not opened the file (late join, waiting at the gate)
/// fetches the ROOM's current part first, and is ready there long before the file is complete.
/// Throttled seeder; the room is paused at 80% of the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_without_a_player_fetches_the_room_position_first() {
    syncwatch_ice_config::install_crypto_provider();
    let dir = tmp("roomfirst");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let len = 64 * MIB;
    let bytes = data(len as usize, 12);
    std::fs::write(src.join("r.mkv"), &bytes).unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (hurl, _http) = http_tracker(peers.clone()).await;
    let sd = seeder(
        &src.join("r.mkv"),
        vec![hurl.clone()],
        Some(2 * 1024 * 1024),
    )
    .await;
    peers.lock().unwrap().push(sd.addr);
    let cfg = engine_cfg(dir.join("cache"), free_port());
    let (eng, mut ev) = EngineHandle::spawn(cfg);
    // 64 MiB over 640 s (0.1 MiB per second of media); the room waits paused at 512 s.
    let room_ms = 512_000;
    let room_byte = len * 8 / 10;
    eng.duration(sd.infohash, Some(640_000));
    eng.room(
        sd.infohash,
        syncwatch_torrent::RoomTarget {
            ms: room_ms,
            playing: false,
            speed: 1000,
            retarget: true,
        },
    );
    let mag = magnet(&sd.infohash, "r.mkv", &[hurl]);
    eng.start(media(sd.infohash, 0, "r.mkv", len, mag));
    let t0 = Instant::now();
    let mut seen = Vec::new();
    // The stream is offered at once; the "player" opens it and reads its 64 KiB header (on
    // demand), as mpv does.
    let url = match wait_event(&mut ev, &mut seen, 30, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("no stream: {e:?}"),
    };
    let header = tokio::spawn(async move { http_get(&url, 0, Some(64 * 1024)).await.len() });
    let st = match wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Status { status, .. } if status.window.is_some_and(|w| w.ready))
    })
    .await
    {
        Some(EngineEvent::Status { status, .. }) => status,
        e => panic!("never ready at the room position: {e:?}"),
    };
    let mib = |b: u64| b as f64 / MIB as f64;
    let ranges: Vec<String> = st
        .ranges
        .iter()
        .map(|(a, b)| format!("{:.1}-{:.1}", mib(*a), mib(*b)))
        .collect();
    eprintln!(
        "ROOMFIRST ready at the room position after {:?} with {:.1} of {:.0} MiB; ranges MiB {}",
        t0.elapsed(),
        mib(st.have_bytes),
        mib(len),
        ranges.join(" ")
    );
    // Readiness while not ready reported the missing part (progress for the host's UI).
    assert!(
        seen.iter()
            .any(|e| matches!(e, EngineEvent::Status { status, .. }
        if status.window.is_some_and(|w| !w.ready && w.missing > 0 && w.at_ms == room_ms)))
    );
    // The room window (from 1 s before the position) is there ...
    let win = (room_byte - MIB / 10, room_byte + 2 * MIB);
    assert!(
        st.ranges.iter().any(|(a, b)| *a <= win.0 && *b >= win.1),
        "room window on disk: {ranges:?}"
    );
    // ... and, apart from the header the player asked for, nothing at the file's start or
    // end: no blind head or tail.
    assert!(
        st.ranges.iter().all(|(a, b)| (*a == 0 && *b <= 2 * MIB)
            || (*a >= win.0.saturating_sub(MIB) && *b <= win.1 + 8 * MIB)),
        "only the header and the room's part: {ranges:?}"
    );
    assert!(st.have_bytes < 8 * MIB, "{} MiB", mib(st.have_bytes));
    assert_eq!(header.await.unwrap(), 64 * 1024);
    // The player also reads an index near the end (like MKV Cues), then opens: the recorded
    // container ranges are exactly what it read, not the room's part.
    let url2 = seen
        .iter()
        .find_map(|e| match e {
            EngineEvent::Playable { url, .. } => Some(url.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        http_get(&url2, len - 100 * 1024, None).await.len(),
        100 * 1024
    );
    let _ = http_get(&url2, room_byte, Some(room_byte + 256 * 1024)).await;
    eng.opened(sd.infohash);
    let st_c = match wait_event(&mut ev, &mut seen, 10, |e| {
        matches!(e, EngineEvent::Status { status, .. } if status.container.iter().any(|(_, b)| *b == len))
    })
    .await
    {
        Some(EngineEvent::Status { status, .. }) => status,
        e => panic!("container ranges not recorded: {e:?}"),
    };
    eprintln!("ROOMFIRST container ranges: {:?}", st_c.container);
    assert!(
        st_c.container
            .iter()
            .any(|(a, b)| *a == 0 && *b >= 64 * 1024),
        "header"
    );
    assert!(
        st_c.container
            .iter()
            .any(|(a, b)| *a == len - 100 * 1024 && *b == len),
        "index at the end"
    );
    assert!(
        st_c.container
            .iter()
            .all(|(a, b)| *b <= win.0 || *a >= win.1),
        "not the room's part"
    );
    // A room seek to 20% (12.8 MiB): the window jumps there at once and that part comes first.
    let t1 = Instant::now();
    eng.room(
        sd.infohash,
        syncwatch_torrent::RoomTarget {
            ms: 128_000,
            playing: false,
            speed: 1000,
            retarget: true,
        },
    );
    let st2 = match wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Status { status, .. } if status.window.is_some_and(|w| w.ready && w.at_ms == 128_000))
    })
    .await
    {
        Some(EngineEvent::Status { status, .. }) => status,
        e => panic!("never ready after the seek: {e:?}"),
    };
    let r2: Vec<String> = st2
        .ranges
        .iter()
        .map(|(a, b)| format!("{:.1}-{:.1}", mib(*a), mib(*b)))
        .collect();
    eprintln!(
        "ROOMFIRST after the seek to 12.8 MiB: ready in {:?}; ranges MiB {}",
        t1.elapsed(),
        r2.join(" ")
    );
    let new_win = (len / 5 - MIB / 10, len / 5 - MIB / 10 + 2 * MIB);
    assert!(
        st2.ranges
            .iter()
            .any(|(a, b)| *a <= new_win.0 && *b >= new_win.1),
        "new window: {r2:?}"
    );
    // The new part is its own range, starting at the new position (not grown from earlier
    // data); the fill that ran before the seek may have added the file start meanwhile.
    let at = st2
        .ranges
        .iter()
        .find(|(a, b)| *a <= new_win.0 && *b >= new_win.1)
        .unwrap();
    assert!(
        at.0 >= new_win.0 - MIB,
        "grows from the new position: {r2:?}"
    );
    eng.stop(sd.infohash, StopReason::Leave).await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Bug reproduction (manual test 2026-11-14): the host already plays when a member's torrent
/// starts. The member must open the stream quickly and then keep up with the room. Before the
/// fix it never became "playable" (it needed head, tail and a 4 MiB pre-buffer at a room
/// position that moves, while only the 2 MiB window was fetched): stuck at "buffering".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_joining_a_playing_room_opens_and_keeps_up() {
    syncwatch_ice_config::install_crypto_provider();
    let dir = tmp("joinplaying");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let len = 64 * MIB;
    let bytes = data(len as usize, 13);
    std::fs::write(src.join("p.mkv"), &bytes).unwrap();
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (hurl, _http) = http_tracker(peers.clone()).await;
    let sd = seeder(
        &src.join("p.mkv"),
        vec![hurl.clone()],
        Some(2 * 1024 * 1024),
    )
    .await;
    peers.lock().unwrap().push(sd.addr);
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(dir.join("cache"), free_port()));
    // 64 MiB over 640 s (0.1 MiB per second of media); the room plays from 5:00.
    eng.duration(sd.infohash, Some(640_000));
    let t_room = Instant::now();
    eng.room(
        sd.infohash,
        syncwatch_torrent::RoomTarget {
            ms: 300_000,
            playing: true,
            speed: 1000,
            retarget: true,
        },
    );
    let mag = magnet(&sd.infohash, "p.mkv", &[hurl]);
    eng.start(media(sd.infohash, 0, "p.mkv", len, mag));
    let mut seen = Vec::new();
    let url = match wait_event(&mut ev, &mut seen, 30, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("the member's stream never became playable: {e:?}"),
    };
    let opened = t_room.elapsed();
    let last = seen.iter().rev().find_map(|e| match e {
        EngineEvent::Status { status, .. } => Some((status.have_bytes, status.ranges.clone())),
        _ => None,
    });
    eprintln!("JOINPLAYING on disk when playable: {last:?}");
    assert!(
        opened < Duration::from_secs(6),
        "the member's stream opens at once: {opened:?}"
    );
    let drain = tokio::spawn(async move { while ev.recv().await.is_some() {} });
    // The player: the container header, then playback at the room's current position.
    let t0 = Instant::now();
    let head = http_get(&url, 0, Some(64 * 1024)).await;
    let header = t0.elapsed();
    assert_eq!(head.len(), 64 * 1024);
    let at = (300_000 + t_room.elapsed().as_millis() as u64) * len / 640_000;
    let rate = MIB / 10;
    let (ttf, stall) = play(&url, at, rate, 10, rate).await;
    eprintln!(
        "JOINPLAYING playable after {opened:?}, header in {header:?}, first second at the room position in {ttf:?}, stall {stall:?}"
    );
    assert!(
        ttf < Duration::from_secs(5),
        "starts at the room position: {ttf:?}"
    );
    assert!(stall <= Duration::from_millis(500), "keeps up: {stall:?}");
    eng.stop(sd.infohash, StopReason::Leave).await;
    drain.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

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
    cfg.policy.head = 256 * 1024;
    cfg.policy.tail = 4 * MIB;
    cfg.policy.prebuffer = MIB;
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

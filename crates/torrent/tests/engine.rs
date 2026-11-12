//! Engine integration tests (spec sections 10 and 12): real librqbit on 127.0.0.1 with an
//! in-process seeder and fake trackers that record every announce.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
use common::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use syncwatch_torrent::{EngineEvent, EngineHandle, Phase, StopReason};

const OUR_PREFIX: &[u8] = b"-SW0100-";

fn ours(log: &TrackerLog) -> Vec<Announce> {
    log.all()
        .into_iter()
        .filter(|a| a.peer_id.starts_with(OUR_PREFIX))
        .collect()
}

fn init() {
    syncwatch_ice_config::install_crypto_provider();
}

struct Swarm {
    seeder: Seeder,
    http: TrackerLog,
    udp: TrackerLog,
    magnet: String,
    trackers: Vec<String>,
}

async fn swarm(root: &std::path::Path, name: &str, upload_bps: Option<u32>) -> Swarm {
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (hurl, http) = http_tracker(peers.clone()).await;
    let (uurl, udp) = udp_tracker(peers.clone()).await;
    let trackers = vec![hurl, uurl];
    let seeder = seeder(root, trackers.clone(), upload_bps).await;
    peers.lock().unwrap().push(seeder.addr);
    let magnet = magnet(&seeder.infohash, name, &trackers);
    Swarm {
        seeder,
        http,
        udp,
        magnet,
        trackers,
    }
}

/// THE top rule (ADR 0010): at 100% verified, downloading AND uploading stop, every socket
/// closes, trackers get "stopped" and nothing after, the engine shuts down, the file plays,
/// and a restart + rejoin never re-adds the torrent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_everything_at_100() {
    init();
    let dir = tmp("stop100");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = data(6 * 1024 * 1024 + 12345, 1);
    std::fs::write(src.join("movie.mkv"), &bytes).unwrap();
    let sw = swarm(&src.join("movie.mkv"), "movie.mkv", None).await;
    let port = free_port();
    let cache = dir.join("cache");
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(cache.clone(), port));
    let media = media(
        sw.seeder.infohash,
        0,
        "movie.mkv",
        bytes.len() as u64,
        sw.magnet.clone(),
    );
    eng.start(media.clone());
    let mut seen = Vec::new();
    let url = match wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("not playable: {e:?} after {seen:?}"),
    };
    wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Complete { .. })
    })
    .await
    .expect("complete");
    assert!(seen.iter().any(
        |e| matches!(e, EngineEvent::Status { status, .. } if status.phase == Phase::Complete)
    ),);
    wait_event(&mut ev, &mut seen, 15, |e| {
        matches!(e, EngineEvent::EngineDown)
    })
    .await
    .expect("engine shut down");
    assert!(!eng.session_running().await, "no session after completion");
    let announced = wait_event(&mut ev, &mut seen, 10, |e| {
        matches!(
            e,
            EngineEvent::Announced {
                reason: StopReason::Complete,
                ..
            }
        )
    })
    .await
    .expect("stopped announce");
    if let EngineEvent::Announced { results, .. } = announced {
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(_, ok)| *ok), "{results:?}");
    }
    // Trackers: "stopped" from us on HTTP and UDP.
    let h = ours(&sw.http);
    let u = ours(&sw.udp);
    assert!(
        h.iter().any(|a| a.event.as_deref() == Some("stopped")),
        "http {h:?}"
    );
    assert!(
        u.iter().any(|a| a.event.as_deref() == Some("stopped")),
        "udp {u:?}"
    );
    // The file plays from the same URL (served from disk).
    let got = http_get(&url, 0, None).await;
    assert_eq!(got.len(), bytes.len());
    assert!(got == bytes, "byte-exact file");
    // Zero traffic both ways for 10 s, no peers, the listener closed, no further announces.
    let up0 = sw.seeder.uploaded();
    let n_http = sw.http.all().len();
    let n_udp_ours = ours(&sw.udp).len();
    let t0 = Instant::now();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(sw.seeder.uploaded(), up0, "seeder sent nothing after 100%");
    assert_eq!(sw.seeder.live_peers(), 0, "no peer connections remain");
    assert!(port_free(port), "our listen port is closed");
    assert!(
        ours(&sw.http).iter().all(|a| a.at < t0) && ours(&sw.udp).len() == n_udp_ours,
        "no announce from us after stopped"
    );
    let _ = n_http;
    // Restart + rejoin: a new engine (same cache) plays the file WITHOUT any session.
    drop(eng);
    let (eng2, mut ev2) = EngineHandle::spawn(engine_cfg(cache.clone(), port));
    eng2.start(media.clone());
    let mut seen2 = Vec::new();
    let url2 = match wait_event(&mut ev2, &mut seen2, 10, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("{e:?}"),
    };
    assert!(
        !eng2.session_running().await,
        "rejoin never creates an engine"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        http_get(&url2, 1000, Some(2000)).await,
        bytes[1000..2000].to_vec()
    );
    assert_eq!(sw.seeder.uploaded(), up0, "still zero bytes");
    assert_eq!(ours(&sw.http).len(), h.len(), "no new tracker announce");
    assert_eq!(ours(&sw.udp).len(), n_udp_ours);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A multi-file torrent downloads ONLY the selected file (neighbour boundary bytes are pruned).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_file_downloads_only_the_selected_file() {
    init();
    let dir = tmp("multi");
    let root = dir.join("src").join("Pack");
    std::fs::create_dir_all(root.join("extras")).unwrap();
    let a = data(3 * 1024 * 1024 + 100, 2);
    let b = data(5 * 1024 * 1024 + 7, 3);
    let c = data(4 * 1024 * 1024, 4);
    std::fs::write(root.join("a.mkv"), &a).unwrap();
    std::fs::write(root.join("b.mkv"), &b).unwrap();
    std::fs::write(root.join("extras").join("c.mkv"), &c).unwrap();
    let sw = swarm(&root, "Pack", None).await;
    let cache = dir.join("cache");
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(cache.clone(), free_port()));
    let info = eng.metadata(&sw.magnet).await.unwrap();
    assert_eq!(info.name, "Pack");
    let paths: Vec<_> = info
        .files
        .iter()
        .map(|f| (f.path.clone(), f.len, f.video))
        .collect();
    assert_eq!(paths.len(), 3, "{paths:?}");
    let bfile = info
        .files
        .iter()
        .find(|f| f.path.ends_with("b.mkv"))
        .unwrap()
        .clone();
    eng.start(media(
        sw.seeder.infohash,
        bfile.index,
        "b.mkv",
        bfile.len,
        sw.magnet.clone(),
    ));
    let mut seen = Vec::new();
    wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Complete { .. })
    })
    .await
    .expect("complete");
    let up = sw.seeder.uploaded();
    assert!(
        up < (b.len() as u64) + 2 * PIECE as u64,
        "only b.mkv (+ boundary pieces) was sent: {up}"
    );
    let tdir = cache.join(sw.seeder.infohash.to_hex());
    let mut files = Vec::new();
    fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out)
            } else {
                out.push(e.path())
            }
        }
    }
    walk(&tdir, &mut files);
    assert_eq!(files.len(), 1, "only the selected file is kept: {files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), b);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The player's open response keeps going from the same offset across completion: the server
/// switches from the torrent stream to the finished file, no reload, no position change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handoff_preserves_position() {
    init();
    let dir = tmp("handoff");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = data(8 * 1024 * 1024, 5);
    std::fs::write(src.join("m.mp4"), &bytes).unwrap();
    // Throttled so the read below spans the completion.
    let sw = swarm(&src.join("m.mp4"), "m.mp4", Some(2 * 1024 * 1024)).await;
    let mut cfg = engine_cfg(dir.join("cache"), free_port());
    cfg.policy.head = 256 * 1024;
    cfg.policy.tail = 512 * 1024;
    cfg.policy.prebuffer = 512 * 1024;
    let (eng, mut ev) = EngineHandle::spawn(cfg);
    eng.start(media(
        sw.seeder.infohash,
        0,
        "m.mp4",
        bytes.len() as u64,
        sw.magnet.clone(),
    ));
    let mut seen = Vec::new();
    let url = match wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("{e:?}"),
    };
    // One request from 1 MiB to the end, started while downloading.
    let start = 1024 * 1024u64;
    let reader = tokio::spawn({
        let url = url.clone();
        async move { http_get(&url, start, None).await }
    });
    wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Complete { .. })
    })
    .await
    .expect("complete during the read");
    let got = tokio::time::timeout(Duration::from_secs(30), reader)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        got.len(),
        bytes.len() - start as usize,
        "one response, no reload"
    );
    assert!(
        got == bytes[start as usize..],
        "same bytes across the handoff"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cancel and switch mid-download: "stopped" to every tracker, zero traffic from the old
/// torrent afterwards, partial data deleted, engine down when idle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_switch_stop_the_old_torrent_completely() {
    init();
    let dir = tmp("switch");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let a = data(16 * 1024 * 1024, 6);
    let b = data(2 * 1024 * 1024, 7);
    std::fs::write(src.join("a.mkv"), &a).unwrap();
    std::fs::create_dir_all(src.join("b")).unwrap();
    std::fs::write(src.join("b").join("b.mkv"), &b).unwrap();
    let swa = swarm(&src.join("a.mkv"), "a.mkv", Some(512 * 1024)).await;
    let swb = swarm(&src.join("b").join("b.mkv"), "b.mkv", None).await;
    let cache = dir.join("cache");
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(cache.clone(), free_port()));
    eng.start(media(
        swa.seeder.infohash,
        0,
        "a.mkv",
        a.len() as u64,
        swa.magnet.clone(),
    ));
    let mut seen = Vec::new();
    wait_event(
        &mut ev,
        &mut seen,
        60,
        |e| matches!(e, EngineEvent::Status { status, .. } if status.have_bytes > 1024 * 1024),
    )
    .await
    .expect("downloading A");
    // Source switch: stop A (never blocks on trackers), start B.
    let t = Instant::now();
    eng.stop(swa.seeder.infohash, StopReason::Switch).await;
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "stop does not wait for trackers"
    );
    eng.start(media(
        swb.seeder.infohash,
        0,
        "b.mkv",
        b.len() as u64,
        swb.magnet.clone(),
    ));
    wait_event(&mut ev, &mut seen, 10, |e| {
        matches!(
            e,
            EngineEvent::Announced {
                reason: StopReason::Switch,
                ..
            }
        )
    })
    .await
    .expect("stopped announce for A");
    assert!(
        ours(&swa.http)
            .iter()
            .any(|a| a.event.as_deref() == Some("stopped"))
    );
    assert!(
        ours(&swa.udp)
            .iter()
            .any(|a| a.event.as_deref() == Some("stopped"))
    );
    assert!(
        !cache.join(swa.seeder.infohash.to_hex()).exists(),
        "partial A deleted"
    );
    let up_a = swa.seeder.uploaded();
    wait_event(&mut ev, &mut seen, 60, |e| {
        matches!(e, EngineEvent::Complete { .. })
    })
    .await
    .expect("B complete");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        swa.seeder.uploaded(),
        up_a,
        "zero traffic from the old torrent"
    );
    assert_eq!(swa.seeder.live_peers(), 0);
    wait_event(&mut ev, &mut seen, 15, |e| {
        matches!(e, EngineEvent::EngineDown)
    })
    .await
    .expect("down");
    // Cancel while fetching metadata of a torrent nobody seeds.
    let dead = magnet(
        &syncwatch_protocol::Infohash([9; 20]),
        "x.mkv",
        &swa.trackers,
    );
    let (eng2, mut ev2) = EngineHandle::spawn({
        let mut c = engine_cfg(dir.join("cache2"), free_port());
        c.metadata_timeout = Duration::from_secs(3);
        c
    });
    eng2.start(media(
        syncwatch_protocol::Infohash([9; 20]),
        0,
        "x.mkv",
        10,
        dead,
    ));
    let mut seen2 = Vec::new();
    let f = wait_event(&mut ev2, &mut seen2, 20, |e| {
        matches!(e, EngineEvent::Failed { .. })
    })
    .await;
    assert!(
        matches!(
            f,
            Some(EngineEvent::Failed {
                error: syncwatch_torrent::EngineError::MetadataTimeout,
                ..
            })
        ),
        "{f:?}"
    );
    assert!(!eng2.session_running().await);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Uploading while downloading has no cap: a second leecher downloads from us mid-download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_while_downloading_without_a_cap() {
    init();
    let dir = tmp("upload");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = data(12 * 1024 * 1024, 8);
    std::fs::write(src.join("u.mkv"), &bytes).unwrap();
    let sw = swarm(&src.join("u.mkv"), "u.mkv", Some(1024 * 1024)).await;
    let port = free_port();
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(dir.join("cache"), port));
    eng.start(media(
        sw.seeder.infohash,
        0,
        "u.mkv",
        bytes.len() as u64,
        sw.magnet.clone(),
    ));
    let mut seen = Vec::new();
    wait_event(
        &mut ev,
        &mut seen,
        60,
        |e| matches!(e, EngineEvent::Status { status, .. } if status.have_bytes > 3 * 1024 * 1024),
    )
    .await
    .expect("downloading");
    // A plain leecher that only knows us.
    let other = librqbit::Session::new_with_opts(
        dir.join("other"),
        librqbit::SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            disable_trackers: true,
            listen: None,
            ipv4_only: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let r = other
        .add_torrent(
            librqbit::AddTorrent::TorrentFileBytes(sw.seeder.torrent.clone()),
            Some(librqbit::AddTorrentOptions {
                initial_peers: Some(vec![format!("127.0.0.1:{port}").parse().unwrap()]),
                disable_trackers: true,
                output_folder: Some(dir.join("other").to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let h = match r {
        librqbit::AddTorrentResponse::Added(_, h) => h,
        _ => panic!(),
    };
    let mut got_from_us = 0;
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        got_from_us = h.stats().progress_bytes;
        if got_from_us > 1024 * 1024 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        got_from_us > 1024 * 1024,
        "the other leecher got data from us: {got_from_us}"
    );
    let up = seen.iter().rev().find_map(|e| match e {
        EngineEvent::Status { status, .. } => Some(status.up_bps),
        _ => None,
    });
    let _ = up;
    other.stop().await;
    eng.stop(sw.seeder.infohash, StopReason::Leave).await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cancelling while the magnet is still resolving (no seeds) is immediate: the lookup is
/// aborted, the trackers get "stopped", and no engine stays up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_while_resolving_is_immediate() {
    init();
    let dir = tmp("cancelmeta");
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (hurl, http) = http_tracker(peers.clone()).await;
    let (uurl, udp) = udp_tracker(peers).await;
    let ih = syncwatch_protocol::Infohash([7; 20]);
    let m = magnet(&ih, "nobody.mkv", &[hurl, uurl]);
    let mut cfg = engine_cfg(dir.join("cache"), free_port());
    cfg.metadata_timeout = Duration::from_secs(60);
    let (eng, mut ev) = EngineHandle::spawn(cfg);
    eng.start(media(ih, 0, "nobody.mkv", 1000, m));
    let mut seen = Vec::new();
    wait_event(
        &mut ev,
        &mut seen,
        10,
        |e| matches!(e, EngineEvent::Status { status, .. } if status.phase == Phase::Metadata),
    )
    .await
    .expect("resolving");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let t = Instant::now();
    eng.stop(ih, StopReason::Cancel).await;
    assert!(
        t.elapsed() < Duration::from_secs(2),
        "cancel is immediate: {:?}",
        t.elapsed()
    );
    wait_event(&mut ev, &mut seen, 10, |e| {
        matches!(
            e,
            EngineEvent::Announced {
                reason: StopReason::Cancel,
                ..
            }
        )
    })
    .await
    .expect("stopped announce");
    assert!(
        ours(&http)
            .iter()
            .any(|a| a.event.as_deref() == Some("stopped"))
    );
    assert!(
        ours(&udp)
            .iter()
            .any(|a| a.event.as_deref() == Some("stopped"))
    );
    assert!(!eng.session_running().await, "no engine left");
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE member bug (M7 manual test): a member whose only working path to the metadata is the
/// host. Before the fix the member fetched the file list from the swarm itself and timed out;
/// now it starts from the host's offered metadata and the host's engine as a peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn member_without_a_path_to_the_swarm_starts_from_the_hosts_metadata() {
    init();
    let dir = tmp("hostmeta");
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let bytes = data(5 * 1024 * 1024 + 77, 21);
    std::fs::write(src.join("film.mkv"), &bytes).unwrap();
    let sw = swarm(&src.join("film.mkv"), "film.mkv", None).await;
    // The host: normal magnet, fetches the metadata, downloads.
    let host_port = free_port();
    let (host, mut hev) = EngineHandle::spawn(engine_cfg(dir.join("host"), host_port));
    host.start(media(
        sw.seeder.infohash,
        0,
        "film.mkv",
        bytes.len() as u64,
        sw.magnet.clone(),
    ));
    let mut hseen = Vec::new();
    let (meta, port) = match wait_event(&mut hev, &mut hseen, 60, |e| {
        matches!(e, EngineEvent::Metadata { .. })
    })
    .await
    {
        Some(EngineEvent::Metadata { torrent, port, .. }) => (torrent, port),
        e => panic!("host has no metadata: {e:?}"),
    };
    assert_eq!(port, host_port, "the host reports its engine port");
    // The member: its magnet only knows a tracker that never lists anyone.
    let peers = Arc::new(Mutex::new(Vec::new()));
    let (dead, _dead_log) = http_tracker(peers).await;
    let dead_magnet = magnet(&sw.seeder.infohash, "film.mkv", &[dead]);
    let member_media = media(
        sw.seeder.infohash,
        0,
        "film.mkv",
        bytes.len() as u64,
        dead_magnet,
    );
    // BEFORE the fix: fetching the file list itself, the member never gets it.
    let mut cfg = engine_cfg(dir.join("old"), free_port());
    cfg.metadata_timeout = Duration::from_secs(8);
    let (old, mut oev) = EngineHandle::spawn(cfg);
    old.start(member_media.clone());
    let mut oseen = Vec::new();
    let f = wait_event(&mut oev, &mut oseen, 30, |e| {
        matches!(e, EngineEvent::Failed { .. })
    })
    .await;
    assert!(
        matches!(
            f,
            Some(EngineEvent::Failed {
                error: syncwatch_torrent::EngineError::MetadataTimeout,
                ..
            })
        ),
        "the old path stalls: {f:?}"
    );
    // AFTER the fix: the host's metadata and the host's engine as a peer.
    let (member, mut mev) = EngineHandle::spawn(engine_cfg(dir.join("member"), free_port()));
    member.start_with_meta(
        member_media,
        meta,
        vec![format!("127.0.0.1:{port}").parse().unwrap()],
    );
    let mut mseen = Vec::new();
    let url = match wait_event(&mut mev, &mut mseen, 60, |e| {
        matches!(e, EngineEvent::Playable { .. })
    })
    .await
    {
        Some(EngineEvent::Playable { url, .. }) => url,
        e => panic!("member did not start: {e:?} {mseen:?}"),
    };
    assert!(
        !mseen.iter().any(
            |e| matches!(e, EngineEvent::Status { status, .. } if status.phase == Phase::Metadata)
        ),
        "no metadata phase: the member never asked the swarm"
    );
    wait_event(&mut mev, &mut mseen, 90, |e| {
        matches!(e, EngineEvent::Complete { .. })
    })
    .await
    .expect("member complete");
    assert!(http_get(&url, 0, None).await == bytes, "byte-exact");
    // Hostile metadata is refused (wrong infohash).
    let (m2, mut m2ev) = EngineHandle::spawn(engine_cfg(dir.join("m2"), free_port()));
    let mut wrong = media(
        syncwatch_protocol::Infohash([3; 20]),
        0,
        "film.mkv",
        bytes.len() as u64,
        magnet(&syncwatch_protocol::Infohash([3; 20]), "x", &[]),
    );
    wrong.name = "film.mkv".into();
    let meta2 = match hseen.iter().find_map(|e| match e {
        EngineEvent::Metadata { torrent, .. } => Some(torrent.clone()),
        _ => None,
    }) {
        Some(m) => m,
        None => panic!(),
    };
    m2.start_with_meta(wrong, meta2, Vec::new());
    let mut s2 = Vec::new();
    let f = wait_event(&mut m2ev, &mut s2, 10, |e| {
        matches!(e, EngineEvent::Failed { .. })
    })
    .await;
    assert!(
        matches!(
            f,
            Some(EngineEvent::Failed {
                error: syncwatch_torrent::EngineError::MetaMismatch,
                ..
            })
        ),
        "{f:?}"
    );
    host.stop(sw.seeder.infohash, StopReason::Leave).await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A member without metadata only plays a file it already finished: no fetch, no session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_only_start_never_touches_the_swarm() {
    init();
    let dir = tmp("cachedonly");
    let (eng, mut ev) = EngineHandle::spawn(engine_cfg(dir.join("cache"), free_port()));
    let ih = syncwatch_protocol::Infohash([5; 20]);
    eng.start_cached_only(media(
        ih,
        0,
        "x.mkv",
        100,
        magnet(&ih, "x.mkv", &["http://127.0.0.1:1/announce".into()]),
    ));
    let mut seen = Vec::new();
    let any = wait_event(&mut ev, &mut seen, 3, |_| true).await;
    assert!(any.is_none(), "nothing happens: {any:?}");
    assert!(!eng.session_running().await, "no engine session");
    let _ = std::fs::remove_dir_all(&dir);
}

//! Dev tool: inspect a .torrent with our defensive parser, optionally try it briefly.
//!   cargo run -p syncwatch-torrent --example inspect_torrent -- <file.torrent> [seconds]
//! With seconds: starts OUR engine on the first video file (trackers only, DHT off, listening on
//! 127.0.0.1) for that long and prints what happened. Downloads into a temp folder, deleted after.
#![allow(clippy::print_stdout)]

use std::time::Duration;
use syncwatch_torrent::{EngineEvent, StopReason, meta};

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    syncwatch_ice_config::install_crypto_provider();
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: inspect_torrent <file.torrent> [seconds]");
        std::process::exit(2);
    };
    let bytes = std::fs::read(path).unwrap_or_default();
    let m = match meta::inspect(&bytes, meta::DEFAULT_META_CAP, None) {
        Ok(m) => m,
        Err(e) => {
            println!("REFUSED {e}");
            return;
        }
    };
    println!(
        "OK name={:?} infohash={} files={} total={} trackers={:?} web_seeds={}",
        m.name,
        m.infohash.to_hex(),
        m.files.len(),
        m.total_size,
        m.trackers,
        m.web_seeds
    );
    for f in m.files.iter().take(12) {
        println!("  #{} {} {} video={}", f.index, f.path, f.len, f.video);
    }
    let Some(secs) = args.get(2).and_then(|s| s.parse::<u64>().ok()) else {
        return;
    };
    let Some(f) = m.files.iter().find(|f| f.video) else {
        println!("no video file");
        return;
    };
    let dir = std::env::temp_dir().join("sw-inspect-run");
    let mut cfg = syncwatch_torrent::EngineConfig::new(dir.clone());
    cfg.dht = false;
    cfg.bind_ip = "127.0.0.1".parse().ok();
    let (eng, mut ev) = syncwatch_torrent::EngineHandle::spawn(cfg);
    let media = syncwatch_protocol::TorrentMedia {
        infohash: m.infohash,
        file_index: f.index,
        name: f.path.rsplit('/').next().unwrap_or("x").to_owned(),
        size: f.len,
        magnet: m.magnet(),
    };
    eng.start_with_meta(media, m.sanitized(), Vec::new());
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut last = String::new();
    while let Ok(Some(e)) = tokio::time::timeout_at(end, ev.recv()).await {
        let line = match &e {
            EngineEvent::Status { status, .. } => format!(
                "status phase={:?} have={} peers={} down={}B/s web_seeds={}",
                status.phase, status.have_bytes, status.peers, status.down_bps, status.web_seeds
            ),
            other => format!("{other:?}").chars().take(160).collect(),
        };
        if line != last {
            println!("{line}");
            last = line;
        }
    }
    eng.stop(m.infohash, StopReason::Cancel).await;
    let _ = std::fs::remove_dir_all(&dir);
    println!("stopped");
}

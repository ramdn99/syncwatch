//! Dev tool: creates OUR OWN test torrent from generated files (never a public one) for the
//! qBittorrent seeder in scripts/dev/torrent-lab.ps1.
//!
//!   cargo run -p syncwatch-torrent --example make_test_torrent -- <file-or-folder> <out.torrent> <tracker-url> [piece-KiB]
//!
//! Prints `MAGNET <magnet>` and `INFOHASH <hex>`.
#![allow(clippy::print_stdout)]

use librqbit::{CreateTorrentOptions, create_torrent};
use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!(
            "usage: make_test_torrent <file-or-folder> <out.torrent> <tracker-url> [piece-KiB]"
        );
        std::process::exit(2);
    }
    let root = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);
    let tracker = args[3].clone();
    let piece_kib: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(256);
    let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
    let res = match create_torrent(
        &root,
        CreateTorrentOptions {
            name: None,
            trackers: vec![tracker.clone()],
            piece_length: Some(piece_kib * 1024),
        },
        &spawner,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("could not create the torrent: {e}");
            std::process::exit(1);
        }
    };
    let bytes = match res.as_bytes() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("could not encode the torrent: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::write(&out, &bytes) {
        eprintln!("could not write {}: {e}", out.display());
        std::process::exit(1);
    }
    let ih = res.info_hash().as_string();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    println!("INFOHASH {ih}");
    println!(
        "MAGNET magnet:?xt=urn:btih:{ih}&dn={}&tr={}",
        enc(&name),
        enc(&tracker)
    );
}

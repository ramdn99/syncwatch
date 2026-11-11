//! Dev tool: fetch a magnet's file list with OUR engine settings and librqbit debug logs.
//!   cargo run -p syncwatch-torrent --example probe_magnet -- "<magnet>"
#![allow(clippy::print_stdout)]

use std::time::Duration;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| {
            "librqbit=debug,librqbit_tracker_comms=debug,syncwatch_torrent=debug".into()
        }))
        .init();
    syncwatch_ice_config::install_crypto_provider();
    let Some(magnet) = std::env::args().nth(1) else {
        eprintln!("usage: probe_magnet <magnet>");
        std::process::exit(2);
    };
    let dir = std::env::temp_dir().join("sw-probe-magnet");
    let mut cfg = syncwatch_torrent::EngineConfig::new(dir);
    cfg.bind_ip = "127.0.0.1".parse().ok();
    cfg.dht = false;
    cfg.metadata_timeout = Duration::from_secs(30);
    let (eng, _ev) = syncwatch_torrent::EngineHandle::spawn(cfg);
    match eng.metadata(&magnet).await {
        Ok(i) => println!("OK {} files: {:?}", i.files.len(), i.files),
        Err(e) => println!("ERR {e}"),
    }
}

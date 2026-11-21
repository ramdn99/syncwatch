//! Dev-only network lab (debug builds; `SYNCWATCH_NET_LAB=host|join`). Drives the real
//! signaling client and the real WebView2 bridge between two app instances and writes
//! measurements to `$SYNCWATCH_NET_LAB_DIR/result-<role>.json` (see scripts/dev/net-lab.ps1).
//!
//! Env: SYNCWATCH_NET_LAB_ICE=none|coturn|metered, SYNCWATCH_NET_LAB_SECONDS,
//! SYNCWATCH_NET_LAB_MB (throughput test size), SYNCWATCH_NET_LAB_DROP_AT /
//! SYNCWATCH_NET_LAB_FREEZE_AT (seconds after open; member side), SYNCWATCH_NET_LAB_MINIMIZE=1.

#![cfg(windows)]

use crate::bridge::Bridge;
use crate::connection::ConnectionManager;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use syncwatch_net::frame::BridgeCommand;
use syncwatch_net::signaling::{SignalingConn, configured_url};
use syncwatch_net::*;
use syncwatch_protocol::signaling::*;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

/// Default bulk chunk (spec: 16 KiB); SYNCWATCH_NET_LAB_CHUNK_KB overrides it for experiments.
const CHUNK: usize = 16 * 1024;
/// Self-test limit for "join takes seconds" (spec section 2, pitfall 3).
const CONNECT_LIMIT_MS: u128 = 5_000;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct LabResult {
    role: String,
    ice: String,
    signaling_host: String,
    connect_ms: Vec<u128>,
    connect_ok: bool,
    relayed: Option<bool>,
    fingerprints_present: bool,
    rtt_ms_p50: Option<f64>,
    rtt_ms_p95: Option<f64>,
    throughput_mb_s: Option<f64>,
    /// Dev bench: two peer connections inside one bridge page, no IPC.
    loopback_mb_s: Option<f64>,
    throughput_bytes: u64,
    keepalive_worst_gap_ms: u64,
    frames_rx: u64,
    interrupts: u32,
    recoveries: u32,
    restarts: u32,
    restart_recovery_ms: Vec<u128>,
    bridge_recreations: u32,
    elapsed_s: u64,
    minimized: bool,
    done: bool,
    error: Option<String>,
}

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// Relay-only through the local dev coturn (deploy/compose.dev.yml). The user below is a local
/// placeholder for that container only, not a real credential.
pub fn coturn_ice() -> IceConfig {
    IceConfig {
        ice_servers: vec![IceServerJs {
            urls: vec![
                "turn:127.0.0.1:3478?transport=udp".into(),
                "turn:127.0.0.1:3478?transport=tcp".into(),
            ],
            username: Some("syncwatch-dev".into()),
            credential: Some("syncwatch-dev-pass".into()),
        }],
        relay_only: true,
    }
}

async fn ice_config(app: &AppHandle, mode: &str) -> IceConfig {
    match mode {
        "coturn" => coturn_ice(),
        "metered" => {
            // The static ICE list from SYNCWATCH_NET_LAB_ICE_FILE (the git-ignored
            // .env.ice.local: no API request), else the saved connection settings. Values are
            // registered with the log scrubber and never logged.
            let from_file = std::env::var("SYNCWATCH_NET_LAB_ICE_FILE")
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|t| syncwatch_ice_config::parse_ice_servers(&t).ok());
            let servers = match from_file {
                Some(s) => s,
                None => {
                    let m = app.state::<ConnectionManager>();
                    m.servers_for_session()
                        .await
                        .map(|(s, _)| s)
                        .unwrap_or_default()
                }
            };
            for s in &servers {
                for v in [&s.username, &s.credential].into_iter().flatten() {
                    crate::logging::register_secret(v);
                }
            }
            IceConfig {
                ice_servers: servers
                    .iter()
                    .map(|s| IceServerJs {
                        urls: s.urls.iter().map(|u| u.to_string()).collect(),
                        username: s.username.clone(),
                        credential: s.credential.clone(),
                    })
                    .collect(),
                relay_only: true,
            }
        }
        _ => IceConfig::default(),
    }
}

pub fn maybe_start(
    app: &AppHandle,
    bridge: Arc<Bridge>,
    events: mpsc::UnboundedReceiver<TransportEvent>,
) -> Option<mpsc::UnboundedReceiver<TransportEvent>> {
    let role = std::env::var("SYNCWATCH_NET_LAB")
        .ok()
        .filter(|_| cfg!(debug_assertions));
    let Some(role) = role else {
        return Some(events);
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if std::env::var("SYNCWATCH_NET_LAB_MINIMIZE").as_deref() == Ok("1") {
            // Give the UI time to paint, then minimize (no input simulation).
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.minimize();
            }
        }
        let dir = PathBuf::from(
            std::env::var("SYNCWATCH_NET_LAB_DIR")
                .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
        );
        let mut lab = Lab::new(&app, bridge, role.clone(), dir).await;
        if let Err(e) = lab.run(events).await {
            lab.r.error = Some(e);
        }
        lab.r.done = true;
        lab.write();
    });
    None
}

struct Lab {
    bridge: Arc<Bridge>,
    host: bool,
    ice: IceConfig,
    dir: PathBuf,
    r: LabResult,
    started: Instant,
}

impl Lab {
    async fn new(app: &AppHandle, bridge: Arc<Bridge>, role: String, dir: PathBuf) -> Self {
        let mode = std::env::var("SYNCWATCH_NET_LAB_ICE").unwrap_or_else(|_| "none".into());
        let ice = ice_config(app, &mode).await;
        Self {
            bridge,
            host: role == "host",
            ice,
            dir,
            r: LabResult {
                role,
                ice: mode,
                minimized: std::env::var("SYNCWATCH_NET_LAB_MINIMIZE").as_deref() == Ok("1"),
                ..LabResult::default()
            },
            started: Instant::now(),
        }
    }

    fn write(&mut self) {
        self.r.elapsed_s = self.started.elapsed().as_secs();
        self.r.bridge_recreations = self.bridge.recreations();
        if let Ok(j) = serde_json::to_vec_pretty(&self.r) {
            let p = self.dir.join(format!(
                "result-{}.json",
                if self.host { "host" } else { "member" }
            ));
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, j).is_ok() {
                let _ = std::fs::rename(&tmp, p);
            }
        }
    }

    async fn run(
        &mut self,
        mut events: mpsc::UnboundedReceiver<TransportEvent>,
    ) -> Result<(), String> {
        if !self.bridge.wait_ready(Duration::from_secs(20)).await {
            return Err("the network bridge did not start".into());
        }
        let bench_mb = env_u64("SYNCWATCH_NET_LAB_BENCH_MB", 0);
        if self.host && bench_mb > 0 {
            let bytes = bench_mb * 1024 * 1024;
            if let Some(d) = self.bridge.dev_bench(bytes, 16 * 1024).await {
                self.r.loopback_mb_s = Some(bytes as f64 / 1e6 / d.as_secs_f64().max(1e-6));
            }
        }
        let url = configured_url().unwrap_or_else(|| "ws://127.0.0.1:8787/ws".to_owned());
        let mut sig = SignalingConn::connect(&url)
            .await
            .map_err(|e| e.to_string())?;
        self.r.signaling_host = sig.host.clone();
        tracing::info!(host = %sig.host, "net lab: signaling connected");
        let room_file = self.dir.join("room.txt");
        let peer: PeerId;
        let connect_started: Instant;
        if self.host {
            sig.send(&ClientMsg::Create { max_members: 12 })
                .await
                .map_err(|e| e.to_string())?;
            let ServerMsg::Created { room, .. } = sig.recv().await.map_err(|e| e.to_string())?
            else {
                return Err("create failed".into());
            };
            std::fs::write(&room_file, &room).map_err(|e| e.to_string())?;
            peer = loop {
                if let ServerMsg::PeerJoined { peer } =
                    sig.recv().await.map_err(|e| e.to_string())?
                {
                    break peer;
                }
            };
            connect_started = Instant::now();
            self.bridge
                .create_peer(peer, IceRole::Answerer, self.ice.clone())
                .map_err(|e| e.to_string())?;
        } else {
            let room = loop {
                if let Ok(r) = std::fs::read_to_string(&room_file) {
                    break r.trim().to_owned();
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            peer = 0;
            connect_started = Instant::now();
            sig.send(&ClientMsg::Join { room })
                .await
                .map_err(|e| e.to_string())?;
            self.bridge
                .create_peer(peer, IceRole::Offerer, self.ice.clone())
                .map_err(|e| e.to_string())?;
        }

        let total = Duration::from_secs(env_u64("SYNCWATCH_NET_LAB_SECONDS", 60));
        let mb = env_u64("SYNCWATCH_NET_LAB_MB", 64);
        let chunk = (env_u64("SYNCWATCH_NET_LAB_CHUNK_KB", (CHUNK / 1024) as u64) as usize * 1024)
            .clamp(1024, 60 * 1024);
        let drop_at = std::env::var("SYNCWATCH_NET_LAB_DROP_AT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        let freeze_at = std::env::var("SYNCWATCH_NET_LAB_FREEZE_AT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok());
        let mut opened_at: Option<Instant> = None;
        let mut first_open: Option<Instant> = None;
        let mut restart_at: Option<Instant> = None;
        let mut rtts: Vec<f64> = Vec::new();
        let mut ping_sent: Option<(u32, Instant)> = None;
        let mut ping_seq = 0u32;
        let mut tp_sent = 0u64;
        let mut tp_target = 0u64;
        let mut tp_rx = 0u64;
        let mut tp_rx_start: Option<Instant> = None;
        let mut dropped = false;
        let mut frozen = false;
        let mut iv = tokio::time::interval(Duration::from_millis(5));
        let mut last_write = Instant::now();

        loop {
            if first_open.is_some_and(|t| t.elapsed() >= total) {
                break;
            }
            if self.started.elapsed() > total + Duration::from_secs(120) {
                return Err("never connected".into());
            }
            tokio::select! {
                m = sig.recv() => {
                    match m.map_err(|e| e.to_string())? {
                        ServerMsg::Signal { data, .. } => {
                            // A fresh offer for an existing link = the remote restarted it.
                            // Every offer gets a fresh answerer connection (cheap; the answerer has
                            // no candidates before it answers).
                            if self.host && matches!(data, SignalData::Offer { .. }) {
                                self.bridge.create_peer(peer, IceRole::Answerer, self.ice.clone()).map_err(|e| e.to_string())?;
                                if first_open.is_some() {
                                    opened_at = None;
                                    restart_at.get_or_insert_with(Instant::now);
                                }
                            }
                            let _ = self.bridge.remote_signal(peer, data);
                        }
                        ServerMsg::PeerJoined { .. } | ServerMsg::Pong => {}
                        other => tracing::info!(?other, "net lab: signaling message"),
                    }
                }
                ev = events.recv() => {
                    let Some(ev) = ev else { return Err("transport closed".into()) };
                    match ev {
                        TransportEvent::IceState { .. } => {}
                        TransportEvent::LocalSignal { data, .. } => {
                            let to = if self.host { Some(peer) } else { None };
                            sig.send(&ClientMsg::Signal { to, data }).await.map_err(|e| e.to_string())?;
                        }
                        TransportEvent::Open { info, .. } => {
                            let ms = connect_started.elapsed().as_millis();
                            if let Some(t) = restart_at.take() {
                                self.r.restart_recovery_ms.push(t.elapsed().as_millis());
                            } else {
                                self.r.connect_ms.push(ms);
                            }
                            self.r.connect_ok = self.r.connect_ms.iter().all(|m| *m <= CONNECT_LIMIT_MS);
                            self.r.relayed = Some(info.relayed);
                            self.r.fingerprints_present = info.local_fingerprint.is_some() && info.remote_fingerprint.is_some();
                            opened_at = Some(Instant::now());
                            first_open.get_or_insert_with(Instant::now);
                            tracing::info!(ms, relayed = info.relayed, "net lab: open");
                        }
                        TransportEvent::Frame { data, .. } => match data.first() {
                            Some(b'P') if !self.host => { let _ = self.bridge.send(peer, Lane::Reliable, { let mut d = data.clone(); d[0] = b'Q'; d }); }
                            Some(b'Q') if self.host => {
                                if let Some((seq, t)) = ping_sent && data.len() >= 5 && u32::from_le_bytes([data[1], data[2], data[3], data[4]]) == seq {
                                    rtts.push(t.elapsed().as_secs_f64() * 1000.0);
                                    ping_sent = None;
                                }
                            }
                            Some(b'T') if !self.host => {
                                tp_rx_start.get_or_insert_with(Instant::now);
                                tp_rx += data.len() as u64;
                                if data.len() >= 9 {
                                    let target = u64::from_le_bytes([data[1], data[2], data[3], data[4], data[5], data[6], data[7], data[8]]);
                                    if tp_rx >= target
                                        && let Some(s) = tp_rx_start
                                    {
                                        let secs = s.elapsed().as_secs_f64().max(1e-6);
                                        self.r.throughput_mb_s = Some(tp_rx as f64 / 1e6 / secs);
                                        self.r.throughput_bytes = tp_rx;
                                        let _ = self.bridge.send(peer, Lane::Reliable, format!("D{}", self.r.throughput_mb_s.unwrap_or(0.0)).into_bytes());
                                    }
                                }
                            }
                            Some(b'D') if self.host => {
                                self.r.throughput_mb_s = std::str::from_utf8(&data[1..]).ok().and_then(|s| s.parse().ok());
                                self.r.throughput_bytes = tp_sent;
                            }
                            _ => {}
                        },
                        TransportEvent::Interrupted { diag, .. } => {
                            self.r.interrupts += 1;
                            tracing::warn!(?diag, "net lab: interrupted");
                        }
                        TransportEvent::Recovered { .. } => self.r.recoveries += 1,
                        TransportEvent::RestartNeeded { diag, .. } => {
                            self.r.restarts += 1;
                            tracing::warn!(?diag, "net lab: restart");
                            opened_at = None;
                            restart_at.get_or_insert_with(Instant::now);
                            if !self.host {
                                self.bridge.create_peer(peer, IceRole::Offerer, self.ice.clone()).map_err(|e| e.to_string())?;
                            }
                        }
                        TransportEvent::BridgeReloaded => {
                            opened_at = None;
                            restart_at.get_or_insert_with(Instant::now);
                            if !self.host {
                                // Wait for the new page to attach, then renegotiate.
                                self.bridge.wait_ready(Duration::from_secs(15)).await;
                                self.bridge.create_peer(peer, IceRole::Offerer, self.ice.clone()).map_err(|e| e.to_string())?;
                            }
                        }
                        TransportEvent::Closed { .. } => {}
                    }
                }
                _ = iv.tick() => {
                    let Some(open) = opened_at else { continue };
                    let since = open.elapsed();
                    // Host: 50 RTT probes, then the throughput test.
                    if self.host && rtts.len() < 50 && ping_sent.is_none_or(|(_, t)| t.elapsed() > Duration::from_secs(2)) && since > Duration::from_millis(300) {
                        ping_seq += 1;
                        let mut d = vec![b'P'];
                        d.extend_from_slice(&ping_seq.to_le_bytes());
                        if self.bridge.send(peer, Lane::Reliable, d).is_ok() {
                            ping_sent = Some((ping_seq, Instant::now()));
                        }
                    }
                    if self.host && rtts.len() >= 50 && tp_target == 0 && mb > 0 {
                        tp_target = mb * 1024 * 1024;
                    }
                    if self.host && tp_sent < tp_target {
                        while tp_sent < tp_target && self.bridge.can_send_bulk(peer) {
                            let mut d = vec![0u8; chunk];
                            d[0] = b'T';
                            d[1..9].copy_from_slice(&tp_target.to_le_bytes());
                            if self.bridge.send(peer, Lane::Reliable, d).is_err() { break }
                            tp_sent += chunk as u64;
                        }
                    }
                    if !self.host {
                        if let Some(d) = drop_at && !dropped && since >= Duration::from_secs(d) {
                            dropped = true;
                            tracing::warn!("net lab: dropping traffic to test the watchdog");
                            self.bridge.dev_command(BridgeCommand::DevDropTraffic { peer });
                        }
                        if let Some(f) = freeze_at && !frozen && since >= Duration::from_secs(f) {
                            frozen = true;
                            tracing::warn!("net lab: freezing the bridge page to test recreation");
                            self.bridge.dev_command(BridgeCommand::DevFreeze);
                        }
                    }
                    if !rtts.is_empty() {
                        let mut s = rtts.clone();
                        s.sort_by(|a, b| a.total_cmp(b));
                        self.r.rtt_ms_p50 = Some(s[s.len() / 2]);
                        self.r.rtt_ms_p95 = Some(s[(s.len() * 95 / 100).min(s.len() - 1)]);
                    }
                    if let Some(st) = self.bridge.stats(peer) {
                        self.r.keepalive_worst_gap_ms = self.r.keepalive_worst_gap_ms.max(st.worst_gap_ms);
                        self.r.frames_rx = st.frames_rx;
                    }
                    if last_write.elapsed() > Duration::from_secs(2) {
                        last_write = Instant::now();
                        self.write();
                    }
                }
            }
        }
        // Close cleanly so the WebRTC stack releases its TURN allocations (a killed process
        // leaves them to expire on the server).
        self.bridge.close_peer(peer);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = sig.send(&ClientMsg::Leave).await;
        Ok(())
    }
}

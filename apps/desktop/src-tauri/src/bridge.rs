//! The WebView2 bridge transport (spec section 1 "Transport", ADR 0005).
//!
//! A never-visible webview window ("bridge") runs WebRTC. Rust owns every timer: it pings the
//! page every second (recreating the window if it stops answering for 5 s), sends a keepalive on
//! every link every second and flags a link as interrupted only when inbound traffic really
//! stopped. Bytes cross the IPC as raw binary in both directions.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use syncwatch_net::frame::{self, BridgeCommand, BridgeEvent, Frame, Wire};
use syncwatch_net::sdp::dtls_fingerprint;
use syncwatch_net::watchdog::{
    BridgeAction, BridgeWatch, KEEPALIVE_EVERY_US, PeerAction, PeerWatch,
};
use syncwatch_net::*;
use syncwatch_protocol::signaling::SignalData;
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::{AppHandle, Manager, State, Webview, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::mpsc;

pub const BRIDGE_LABEL: &str = "bridge";

/// Browser arguments for EVERY webview of the app (WebView2 shares one browser process, so all
/// windows must use identical arguments). wry's defaults + the anti-throttling flags.
/// `tauri.conf.json` must carry the same string (checked by a test).
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows";

/// Bulk data is queued in the bridge only while it holds less than this.
const BULK_HIGH_WATER: u64 = 4 * 1024 * 1024;

struct PeerState {
    watch: PeerWatch,
    open: bool,
    ice: String,
    connection: String,
    keep_seq: u32,
    last_keepalive_us: i64,
    /// Flow control: wire bytes queued to the bridge, bytes it confirmed, its bufferedAmount.
    tx_bytes: u64,
    acked_rx: u64,
    buffered: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStats {
    pub open: bool,
    pub frames_rx: u64,
    pub frames_tx: u64,
    pub worst_gap_ms: u64,
}

struct Inner {
    channel: Option<Channel<InvokeResponseBody>>,
    peers: HashMap<PeerId, PeerState>,
    bridge_watch: BridgeWatch,
    recreations: u32,
    /// Label of the current bridge window generation.
    label: String,
    bench: Option<(Instant, tokio::sync::oneshot::Sender<std::time::Duration>)>,
}

pub struct Bridge {
    app: AppHandle,
    epoch: Instant,
    inner: Mutex<Inner>,
    events: mpsc::UnboundedSender<TransportEvent>,
    out: mpsc::UnboundedSender<Vec<u8>>,
}

impl Bridge {
    pub fn now_us(&self) -> i64 {
        self.epoch.elapsed().as_micros() as i64
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Creates the bridge window and starts the watchdog. Call from `setup`.
    pub fn start(
        app: &AppHandle,
    ) -> tauri::Result<(Arc<Self>, mpsc::UnboundedReceiver<TransportEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let bridge = Arc::new(Self {
            app: app.clone(),
            epoch: Instant::now(),
            inner: Mutex::new(Inner {
                channel: None,
                peers: HashMap::new(),
                bridge_watch: BridgeWatch::new(0),
                recreations: 0,
                label: BRIDGE_LABEL.to_owned(),
                bench: None,
            }),
            events: tx,
            out: out_tx,
        });
        tauri::async_runtime::spawn(Arc::clone(&bridge).writer(out_rx));
        bridge.create_window()?;
        let b = Arc::clone(&bridge);
        tauri::async_runtime::spawn(async move {
            let mut iv = tokio::time::interval(std::time::Duration::from_millis(250));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                b.tick();
            }
        });
        Ok((bridge, rx))
    }

    fn is_current(&self, label: &str) -> bool {
        self.lock().label == label
    }

    fn create_window(&self) -> tauri::Result<()> {
        let label = self.lock().label.clone();
        WebviewWindowBuilder::new(&self.app, label, WebviewUrl::App("bridge.html".into()))
            .title("SyncWatch bridge")
            .visible(false)
            .skip_taskbar(true)
            .focused(false)
            .inner_size(1.0, 1.0)
            .additional_browser_args(BROWSER_ARGS)
            .build()?;
        Ok(())
    }

    fn emit(&self, e: TransportEvent) {
        let _ = self.events.send(e);
    }

    /// Queues a frame for the writer task, which coalesces whatever is queued into one batch.
    fn send_raw(&self, bytes: Vec<u8>) -> Result<(), NetError> {
        if self.lock().channel.is_none() {
            return Err(NetError::BridgeNotReady);
        }
        self.out.send(bytes).map_err(|_| NetError::BridgeNotReady)
    }

    async fn writer(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Vec<u8>>) {
        while let Some(first) = rx.recv().await {
            let mut size = first.len();
            let mut batch = vec![first];
            while size < frame::MAX_BATCH {
                match rx.try_recv() {
                    Ok(f) => {
                        size += f.len() + 4;
                        batch.push(f);
                    }
                    Err(_) => break,
                }
            }
            let Some(ch) = self.lock().channel.clone() else {
                continue;
            };
            let msg = if batch.len() == 1 {
                batch.pop().unwrap_or_default()
            } else {
                frame::encode_batch(&batch)
            };
            let _ = ch.send(InvokeResponseBody::Raw(msg));
        }
    }

    fn command(&self, c: &BridgeCommand) -> Result<(), NetError> {
        self.send_raw(frame::encode_json(c))
    }

    /// Dev hooks used by the network lab.
    pub fn dev_command(&self, c: BridgeCommand) {
        let _ = self.command(&c);
    }

    pub fn stats(&self, peer: PeerId) -> Option<PeerStats> {
        self.lock().peers.get(&peer).map(|p| PeerStats {
            open: p.open,
            frames_rx: p.watch.frames_rx,
            frames_tx: p.watch.frames_tx,
            worst_gap_ms: (p.watch.worst_gap_us / 1000) as u64,
        })
    }

    /// True once the bridge page attached its channel. Waits up to `timeout`.
    pub async fn wait_ready(&self, timeout: std::time::Duration) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if self.lock().channel.is_some() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Dev only: in-page loopback data-channel benchmark; returns the elapsed time.
    pub async fn dev_bench(&self, bytes: u64, chunk: u32) -> Option<std::time::Duration> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.lock().bench = Some((Instant::now(), tx));
        self.command(&BridgeCommand::DevBench { bytes, chunk })
            .ok()?;
        tokio::time::timeout(std::time::Duration::from_secs(120), rx)
            .await
            .ok()?
            .ok()
    }

    pub fn recreations(&self) -> u32 {
        self.lock().recreations
    }

    /// Bulk sends should wait while this is false (bufferedAmount watermark).
    pub fn can_send_bulk(&self, peer: PeerId) -> bool {
        self.lock().peers.get(&peer).is_some_and(|p| {
            p.open && p.tx_bytes.saturating_sub(p.acked_rx) + p.buffered < BULK_HIGH_WATER
        })
    }

    fn diag(p: &PeerState, now: i64, cause: &'static str) -> Diagnostics {
        Diagnostics {
            silence_ms: (p.watch.silence_us(now) / 1000) as u64,
            frames_rx: p.watch.frames_rx,
            frames_tx: p.watch.frames_tx,
            ice_state: p.ice.clone(),
            connection_state: p.connection.clone(),
            cause,
        }
    }

    fn tick(&self) {
        let now = self.now_us();
        let mut out_frames = Vec::new();
        let mut events = Vec::new();
        let mut recreate = false;
        {
            let mut g = self.lock();
            match g.bridge_watch.tick(now) {
                Some(BridgeAction::Ping { seq }) => {
                    out_frames.push(frame::encode_json(&BridgeCommand::Ping { seq }))
                }
                Some(BridgeAction::Recreate { .. }) => recreate = true,
                None => {}
            }
            for (id, p) in g.peers.iter_mut().filter(|(_, p)| p.open) {
                if now - p.last_keepalive_us >= KEEPALIVE_EVERY_US {
                    p.last_keepalive_us = now;
                    p.keep_seq = p.keep_seq.wrapping_add(1);
                    p.watch.on_tx();
                    let f = frame::encode_data(
                        *id,
                        Lane::Unreliable,
                        &frame::wire_keepalive(p.keep_seq, now),
                    );
                    p.tx_bytes += f.len() as u64;
                    out_frames.push(f);
                }
                match p.watch.tick(now) {
                    Some(PeerAction::Interrupted { .. }) => {
                        let diag = Self::diag(p, now, "no inbound traffic");
                        tracing::warn!(peer = id, ?diag, "link interrupted");
                        events.push(TransportEvent::Interrupted { peer: *id, diag });
                    }
                    Some(PeerAction::Restart { .. }) => {
                        let diag = Self::diag(p, now, "silent link, restarting peer");
                        tracing::warn!(peer = id, ?diag, "restarting silent link");
                        events.push(TransportEvent::RestartNeeded { peer: *id, diag });
                    }
                    _ => {}
                }
            }
        }
        if recreate {
            self.recreate();
            return;
        }
        for f in out_frames {
            let _ = self.send_raw(f);
        }
        for e in events {
            self.emit(e);
        }
    }

    /// The page stopped answering pings: destroy and rebuild the window. All peers are gone.
    fn recreate(&self) {
        // destroy() completes asynchronously, so every generation gets its own label; frames
        // from an older (hung) page are refused by `is_current`.
        let (old, peers): (String, Vec<PeerId>) = {
            let mut g = self.lock();
            g.channel = None;
            g.recreations += 1;
            let next = format!("{BRIDGE_LABEL}-{}", g.recreations);
            let old = std::mem::replace(&mut g.label, next);
            (old, g.peers.drain().map(|(id, _)| id).collect())
        };
        tracing::warn!(
            peers = peers.len(),
            "bridge page hung (no pong for 5 s); recreating it"
        );
        if let Some(w) = self.app.get_webview_window(&old) {
            let _ = w.destroy();
        }
        if let Err(e) = self.create_window() {
            tracing::error!(error = %e, "could not recreate the bridge window");
        }
        for peer in peers {
            self.emit(TransportEvent::Closed {
                peer,
                reason: "bridge recreated".into(),
            });
        }
        self.emit(TransportEvent::BridgeReloaded);
    }

    fn attach(&self, ch: Channel<InvokeResponseBody>) {
        let now = self.now_us();
        let mut g = self.lock();
        g.channel = Some(ch);
        g.bridge_watch.on_pong(now);
    }

    fn on_frame(&self, bytes: &[u8]) {
        let now = self.now_us();
        let frames = match frame::decode_all(bytes) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, "bad bridge frame");
                return;
            }
        };
        for f in frames {
            match f {
                Frame::Data {
                    peer,
                    lane,
                    payload,
                } => {
                    let wire = frame::decode_wire(&payload);
                    let recovered = {
                        let mut g = self.lock();
                        let Some(p) = g.peers.get_mut(&peer) else {
                            continue;
                        };
                        p.watch.on_rx(now)
                    };
                    if let Some(PeerAction::Recovered { silence_us }) = recovered {
                        tracing::info!(peer, silence_ms = silence_us / 1000, "link recovered");
                        self.emit(TransportEvent::Recovered { peer });
                    }
                    if let Some(Wire::App(data)) = wire {
                        self.emit(TransportEvent::Frame { peer, lane, data });
                    }
                }
                Frame::Json(j) => match serde_json::from_slice::<BridgeEvent>(&j) {
                    Ok(ev) => self.on_event(ev, now),
                    Err(_) => tracing::warn!("unparseable bridge event"),
                },
            }
        }
    }

    fn on_event(&self, ev: BridgeEvent, now: i64) {
        match ev {
            BridgeEvent::Ready => tracing::info!("bridge ready"),
            BridgeEvent::Pong { .. } => self.lock().bridge_watch.on_pong(now),
            BridgeEvent::LocalSignal { peer, data } => {
                self.emit(TransportEvent::LocalSignal { peer, data })
            }
            BridgeEvent::Open {
                peer,
                relayed,
                local_sdp,
                remote_sdp,
            } => {
                {
                    let mut g = self.lock();
                    let Some(p) = g.peers.get_mut(&peer) else {
                        return;
                    };
                    p.open = true;
                    p.watch = PeerWatch::new(now);
                    p.last_keepalive_us = 0;
                }
                let info = PeerInfo {
                    local_fingerprint: dtls_fingerprint(&local_sdp),
                    remote_fingerprint: dtls_fingerprint(&remote_sdp),
                    relayed,
                };
                tracing::info!(peer, relayed, "peer connected");
                self.emit(TransportEvent::Open { peer, info });
            }
            BridgeEvent::State {
                peer,
                ice,
                connection,
            } => {
                if let Some(p) = self.lock().peers.get_mut(&peer) {
                    p.ice = ice;
                    p.connection = connection;
                }
            }
            BridgeEvent::Buffered { peer, rx, buffered } => {
                if let Some(p) = self.lock().peers.get_mut(&peer) {
                    p.acked_rx = rx;
                    p.buffered = buffered;
                }
            }
            BridgeEvent::Closed { peer, reason } => {
                let was = self.lock().peers.remove(&peer).is_some();
                if was {
                    tracing::info!(peer, %reason, "peer closed");
                    self.emit(TransportEvent::Closed { peer, reason });
                }
            }
            BridgeEvent::BenchDone { .. } => {
                if let Some((t, tx)) = self.lock().bench.take() {
                    let _ = tx.send(t.elapsed());
                }
            }
            BridgeEvent::Error { peer, message } => {
                // WebRTC error strings contain no credentials; keep them short in the log.
                let m: String = message.chars().take(200).collect();
                tracing::warn!(?peer, error = %m, "bridge error");
            }
        }
    }
}

impl PeerTransport for Bridge {
    fn create_peer(&self, peer: PeerId, role: IceRole, ice: IceConfig) -> Result<(), NetError> {
        let now = self.now_us();
        self.lock().peers.insert(
            peer,
            PeerState {
                watch: PeerWatch::new(now),
                open: false,
                ice: "new".into(),
                connection: "new".into(),
                keep_seq: 0,
                last_keepalive_us: 0,
                buffered: 0,
                tx_bytes: 0,
                acked_rx: 0,
            },
        );
        self.command(&BridgeCommand::CreatePeer {
            peer,
            offerer: role == IceRole::Offerer,
            ice,
        })
    }

    fn remote_signal(&self, peer: PeerId, data: SignalData) -> Result<(), NetError> {
        self.command(&BridgeCommand::RemoteSignal { peer, data })
    }

    fn send(&self, peer: PeerId, lane: Lane, data: Vec<u8>) -> Result<(), NetError> {
        if data.len() > frame::MAX_IPC_FRAME - 16 {
            return Err(NetError::TooLarge);
        }
        let bytes = frame::encode_data(peer, lane, &frame::wire_app(&data));
        {
            let mut g = self.lock();
            let p = g.peers.get_mut(&peer).ok_or(NetError::UnknownPeer)?;
            p.watch.on_tx();
            p.tx_bytes += bytes.len() as u64;
        }
        self.send_raw(bytes)
    }

    fn close_peer(&self, peer: PeerId) {
        self.lock().peers.remove(&peer);
        let _ = self.command(&BridgeCommand::ClosePeer { peer });
    }
}

/// The bridge page hands Rust its binary channel. Only the current bridge window may call this.
#[tauri::command]
pub fn bridge_attach(
    webview: Webview,
    channel: Channel<InvokeResponseBody>,
    bridge: State<'_, Arc<Bridge>>,
) -> Result<(), ()> {
    if !bridge.is_current(webview.label()) {
        tracing::warn!("bridge_attach from a non-bridge webview refused");
        return Err(());
    }
    bridge.attach(channel);
    Ok(())
}

/// Raw binary frames from the bridge page. Only the current bridge window may call this.
#[tauri::command]
pub fn bridge_in(
    webview: Webview,
    request: Request<'_>,
    bridge: State<'_, Arc<Bridge>>,
) -> Result<(), ()> {
    if !bridge.is_current(webview.label()) {
        return Err(());
    }
    if let InvokeBody::Raw(bytes) = request.body() {
        bridge.on_frame(bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_args_match_tauri_conf_and_include_anti_throttle_flags() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let main = conf["app"]["windows"][0]["additionalBrowserArgs"]
            .as_str()
            .unwrap();
        assert_eq!(
            main, BROWSER_ARGS,
            "all webviews must share identical browser args"
        );
        for flag in [
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            "msSmartScreenProtection",
        ] {
            assert!(BROWSER_ARGS.contains(flag), "{flag}");
        }
    }
}

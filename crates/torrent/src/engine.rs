//! librqbit adapter (ADRs 0009, 0010). One actor task owns the session, the cache and the
//! active torrents. The engine exists only while a torrent is active: it is created on the
//! first start and shut down (DHT, listeners, sockets) when the last torrent stops.
//!
//! Stop sequence (completion, leave, source switch, cancel): hand the player over to the
//! finished file (completion only), pause (every peer disconnects), our own tracker "stopped"
//! announce in the background, forget the torrent (files kept on completion, deleted
//! otherwise), and stop the session when nothing else is active.

use crate::announce::{STOPPED_TIMEOUT, StoppedAnnounce, send_stopped};
use crate::cache::Cache;
use crate::juggle::{Juggler, StreamDriver};
use crate::meta;
use crate::policy::{ByteMap, FileGeometry, PiecePolicy, PolicyConfig};
use crate::ready::{Readiness, ReadyConfig, ReadyTracker};
use crate::server::{Loopback, Reader, StreamOpener};
use futures_util::future::BoxFuture;
use librqbit::api::TorrentIdOrHash;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ListenerMode, ListenerOptions,
    ManagedTorrent, Session, SessionOptions,
};
use tokio::io::{AsyncRead, AsyncSeek};

type ManagedTorrentHandle = Arc<ManagedTorrent>;

/// librqbit's FileStream (its type is not exported): read + seek.
pub trait SeekRead: AsyncRead + AsyncSeek + Send + Unpin {}
impl<T: AsyncRead + AsyncSeek + Send + Unpin> SeekRead for T {}
type FileStream = Box<dyn SeekRead>;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use syncwatch_protocol::{Infohash, TorrentMedia};
use tokio::io::AsyncSeekExt;
use tokio::sync::{mpsc, oneshot};
use url::Url;

/// The completion message (exact wording approved for M7; it never claims we left the swarm).
pub const COMPLETE_MESSAGE: &str = "Downloading and sharing have stopped. No further traffic.";

pub type FreeSpace = Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

#[derive(Clone)]
pub struct EngineConfig {
    pub cache_dir: PathBuf,
    pub cap_bytes: u64,
    pub keep_finished: bool,
    /// Incoming connections: 0 = random port.
    pub listen_port: u16,
    pub bind_ip: Option<IpAddr>,
    pub dht: bool,
    pub metadata_timeout: Duration,
    pub policy: PolicyConfig,
    /// The read-ahead window that makes a participant ready at the room position (ADR 0012).
    pub ready: ReadyConfig,
    /// Tests: peers to connect to directly.
    pub initial_peers: Vec<SocketAddr>,
    /// No peers for this long: "waiting for peers".
    pub no_peers_after: Duration,
    /// While waiting for peers, restart peer discovery this often.
    pub retry_every: Duration,
    pub free_space: Option<FreeSpace>,
    /// Largest torrent metadata accepted (a .torrent file or the host's offer).
    pub meta_cap: usize,
    /// Tick period of the actor.
    pub tick: Duration,
}

impl EngineConfig {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            cap_bytes: 50 * 1024 * 1024 * 1024,
            keep_finished: true,
            listen_port: 0,
            bind_ip: None,
            dht: true,
            metadata_timeout: Duration::from_secs(90),
            policy: PolicyConfig::default(),
            ready: ReadyConfig::default(),
            initial_peers: Vec::new(),
            no_peers_after: Duration::from_secs(15),
            retry_every: Duration::from_secs(30),
            free_space: None,
            meta_cap: meta::DEFAULT_META_CAP,
            tick: Duration::from_millis(200),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("not a valid magnet link")]
    BadMagnet,
    #[error("could not get the torrent's file list in time")]
    MetadataTimeout,
    #[error("could not get the torrent's file list")]
    Metadata,
    #[error("the torrent contains an unsafe file name or path")]
    UnsafePath,
    #[error("the selected file does not exist in this torrent")]
    NoSuchFile,
    #[error("the selected file differs from the host's (size)")]
    SizeMismatch,
    #[error("not enough disk space")]
    DiskFull,
    #[error("the torrent cache is full")]
    CacheFull,
    #[error("the torrent engine could not start")]
    Engine,
    #[error("the torrent metadata does not belong to this torrent")]
    MetaMismatch,
    #[error("the torrent metadata is larger than the limit")]
    MetaTooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub index: u32,
    /// Display path ("Folder/file.mkv"), validated.
    pub path: String,
    pub len: u64,
    pub video: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentInfo {
    pub infohash: Infohash,
    pub name: String,
    pub files: Vec<FileEntry>,
    pub total_size: u64,
    /// Validated trackers (count) and BEP 19 web seeds (librqbit 9.0.1 cannot use them).
    pub trackers: usize,
    pub web_seeds: usize,
    /// Sanitized .torrent bytes (original info dictionary + validated trackers only).
    pub torrent: Vec<u8>,
}

impl TorrentInfo {
    /// From validated metadata (our parser), skipping padding files.
    pub fn from_meta(m: &meta::TorrentMeta) -> Self {
        Self {
            infohash: m.infohash,
            name: m.name.clone(),
            files: m
                .files
                .iter()
                .filter(|f| !f.padding)
                .map(|f| FileEntry {
                    index: f.index,
                    path: f.path.clone(),
                    len: f.len,
                    video: f.video,
                })
                .collect(),
            total_size: m.total_size,
            trackers: m.trackers.len(),
            web_seeds: m.web_seeds,
            torrent: m.sanitized(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Metadata,
    Downloading,
    Streaming,
    NoPeers,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TorrentStatus {
    pub phase: Phase,
    pub have_bytes: u64,
    pub size: u64,
    pub down_bps: u64,
    pub up_bps: u64,
    pub peers: u32,
    pub eta_secs: Option<u64>,
    pub ranges: Vec<(u64, u64)>,
    /// Readiness at the ROOM position (window + container pieces, with hysteresis); `None`
    /// until the room position and a time -> byte mapping are known. This, not the overall
    /// progress or the player's read frontier, says whether this participant is buffering.
    pub window: Option<Readiness>,
    /// Only the playback stream is running (window short / just seeked).
    pub exclusive: bool,
    /// What the engine is using to find peers (status text).
    pub trackers: u32,
    pub dht: bool,
    pub web_seeds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Complete,
    Leave,
    Switch,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Status {
        infohash: Infohash,
        status: TorrentStatus,
    },
    /// The loopback URL is ready for the player (contains a secret token: never log it).
    Playable {
        infohash: Infohash,
        url: String,
    },
    /// The torrent was added: its sanitized metadata and our engine's listen port (the host
    /// offers both to members).
    Metadata {
        infohash: Infohash,
        torrent: Vec<u8>,
        port: u16,
    },
    Complete {
        infohash: Infohash,
    },
    Failed {
        infohash: Infohash,
        error: EngineError,
    },
    /// Our "stopped" announce finished: (tracker label, ok).
    Announced {
        infohash: Infohash,
        reason: StopReason,
        results: Vec<(String, bool)>,
    },
    /// The session is gone: no DHT, no listeners, no sockets.
    EngineDown,
}

enum Cmd {
    Metadata {
        magnet: String,
        reply: oneshot::Sender<Result<TorrentInfo, EngineError>>,
    },
    Start {
        media: TorrentMedia,
        /// Host-provided (member) or .torrent-file metadata: no swarm lookup at all.
        meta: Option<Vec<u8>>,
        extra_peers: Vec<SocketAddr>,
        /// Only play an already finished file; never fetch or join anything.
        cached_only: bool,
    },
    Stop {
        infohash: Infohash,
        reason: StopReason,
        reply: Option<oneshot::Sender<()>>,
    },
    Seek {
        infohash: Infohash,
        ms: u64,
    },
    Duration {
        infohash: Infohash,
        ms: Option<u64>,
    },
    Room {
        infohash: Infohash,
        target: RoomTarget,
    },
    Config {
        cfg: Box<EngineConfig>,
    },
    Running {
        reply: oneshot::Sender<bool>,
    },
}

/// Handle to the engine actor (cheap to clone).
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl EngineHandle {
    /// Spawns the actor on the current tokio runtime. No session exists until a torrent starts.
    pub fn spawn(cfg: EngineConfig) -> (Self, mpsc::UnboundedReceiver<EngineEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        tokio::spawn(Actor::new(cfg, ev_tx).run(rx));
        (Self { tx }, ev_rx)
    }

    pub async fn metadata(&self, magnet: &str) -> Result<TorrentInfo, EngineError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Metadata {
                magnet: magnet.to_owned(),
                reply,
            })
            .map_err(|_| EngineError::Engine)?;
        rx.await.unwrap_or(Err(EngineError::Engine))
    }

    /// Starts from metadata we already have (the host's offer, or a .torrent file): the
    /// engine never asks the swarm for the file list. `extra_peers`: e.g. the host's engine.
    pub fn start_with_meta(
        &self,
        media: TorrentMedia,
        meta: Vec<u8>,
        extra_peers: Vec<SocketAddr>,
    ) {
        let _ = self.tx.send(Cmd::Start {
            media,
            meta: Some(meta),
            extra_peers,
            cached_only: false,
        });
    }

    /// Plays this torrent's file if it is already finished in the cache (no swarm, no fetch);
    /// otherwise does nothing (a member then waits for the host's metadata).
    pub fn start_cached_only(&self, media: TorrentMedia) {
        let _ = self.tx.send(Cmd::Start {
            media,
            meta: None,
            extra_peers: Vec::new(),
            cached_only: true,
        });
    }

    pub fn start(&self, media: TorrentMedia) {
        let _ = self.tx.send(Cmd::Start {
            media,
            meta: None,
            extra_peers: Vec::new(),
            cached_only: false,
        });
    }

    /// Stops a torrent (never blocks on trackers: the announce runs in the background).
    pub async fn stop(&self, infohash: Infohash, reason: StopReason) {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Cmd::Stop {
                infohash,
                reason,
                reply: Some(reply),
            })
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    pub fn stop_nowait(&self, infohash: Infohash, reason: StopReason) {
        let _ = self.tx.send(Cmd::Stop {
            infohash,
            reason,
            reply: None,
        });
    }

    /// A room seek to media time `ms`: pre-target the download before the player asks.
    pub fn seek(&self, infohash: Infohash, ms: u64) {
        let _ = self.tx.send(Cmd::Seek { infohash, ms });
    }

    pub fn duration(&self, infohash: Infohash, ms: Option<u64>) {
        let _ = self.tx.send(Cmd::Duration { infohash, ms });
    }

    /// The ROOM's authoritative position (ADR 0012). The engine extrapolates it while the room
    /// plays, fetches its window first (also before the player opened the file or while it
    /// waits at the gate) and reports readiness there. `retarget`: a room seek, media change,
    /// pause or resume (the window jumps; fetch it at once, before anything else).
    pub fn room(&self, infohash: Infohash, target: RoomTarget) {
        let _ = self.tx.send(Cmd::Room { infohash, target });
    }

    pub fn set_config(&self, cfg: EngineConfig) {
        let _ = self.tx.send(Cmd::Config { cfg: Box::new(cfg) });
    }

    /// Whether a librqbit session exists right now (tests and diagnostics).
    pub async fn session_running(&self) -> bool {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Running { reply }).is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }
}

fn rand_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    let _ = getrandom::fill(&mut b);
    b
}

/// Our peer id ("-SW0100-" + 12 random bytes); we must know it for the "stopped" announce.
fn new_peer_id() -> [u8; 20] {
    let mut id = rand_bytes::<20>();
    id[..8].copy_from_slice(b"-SW0100-");
    id
}

struct Driver {
    api: Api,
    id: usize,
    file: usize,
}

async fn open_stream(api: &Api, id: usize, file: usize, pos: u64) -> std::io::Result<FileStream> {
    let s = api
        .api_stream(TorrentIdOrHash::Id(id), file)
        .await
        .map_err(|_| std::io::Error::other("stream unavailable"))?;
    let mut s: FileStream = Box::new(s);
    s.seek(std::io::SeekFrom::Start(pos)).await?;
    Ok(s)
}

impl StreamDriver for Driver {
    type Stream = FileStream;
    fn open(&self, pos: u64) -> BoxFuture<'_, Option<FileStream>> {
        Box::pin(async move { open_stream(&self.api, self.id, self.file, pos).await.ok() })
    }
    fn seek<'a>(&'a self, s: &'a mut FileStream, pos: u64) -> BoxFuture<'a, bool> {
        Box::pin(async move { s.seek(std::io::SeekFrom::Start(pos)).await.is_ok() })
    }
}

struct Opener {
    api: Api,
    id: usize,
    file: usize,
}

impl StreamOpener for Opener {
    fn open_at(&self, pos: u64) -> BoxFuture<'static, std::io::Result<Reader>> {
        let (api, id, file) = (self.api.clone(), self.id, self.file);
        Box::pin(async move {
            let s = open_stream(&api, id, file, pos).await?;
            Ok(Box::new(s) as Reader)
        })
    }
}

struct Active {
    media: TorrentMedia,
    handle: ManagedTorrentHandle,
    id: usize,
    file: usize,
    first_piece: u32,
    rel_path: String,
    trackers: Vec<Url>,
    web_seeds: usize,
    policy: PiecePolicy,
    juggler: Juggler<FileStream>,
    bytes: ByteMap,
    loopback: Loopback,
    last_read_seq: u64,
    last_read_end: u64,
    pending_seek_ms: Option<u64>,
    ready: ReadyTracker,
    playable_sent: bool,
    no_peers_since: Option<Instant>,
    last_retry: Instant,
    failed: bool,
}

/// The room's playback position as the app knows it (see [`EngineHandle::room`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomTarget {
    pub ms: u64,
    pub playing: bool,
    /// Speed in permille (1000 = normal).
    pub speed: u32,
    pub retarget: bool,
}

#[derive(Debug, Clone, Copy)]
struct RoomAt {
    target: RoomTarget,
    at: Instant,
}

impl RoomAt {
    fn ms_now(&self) -> u64 {
        if !self.target.playing {
            return self.target.ms;
        }
        let el = self.at.elapsed().as_micros() as u64 * self.target.speed as u64 / 1_000_000;
        self.target.ms.saturating_add(el)
    }
}

/// A finished file served without any engine (restart / rejoin never re-adds the torrent).
struct Served {
    loopback: Loopback,
}

struct Actor {
    cfg: EngineConfig,
    events: mpsc::UnboundedSender<EngineEvent>,
    session: Option<Arc<Session>>,
    api: Option<Api>,
    peer_id: [u8; 20],
    cache: Cache,
    active: HashMap<Infohash, Active>,
    served: HashMap<Infohash, Served>,
    /// Torrents being prepared (metadata phase, in their own task); a stop aborts them.
    starting: HashMap<Infohash, (TorrentMedia, tokio::task::AbortHandle)>,
    /// The room position per torrent (kept while the torrent is still starting).
    rooms: HashMap<Infohash, RoomAt>,
    /// Media durations learned before the torrent was added.
    durations: HashMap<Infohash, u64>,
    /// Host metadata queries in flight (they keep the session alive).
    queries: usize,
    prep_tx: mpsc::UnboundedSender<Prepared>,
    prep_rx: Option<mpsc::UnboundedReceiver<Prepared>>,
    http: reqwest::Client,
    epoch: Instant,
}

/// A metadata fetch finished (it runs in its own task so a stop is never delayed by it).
struct Prepared {
    infohash: Infohash,
    trackers: Vec<Url>,
    kind: PrepKind,
    result: Result<(TorrentInfo, bytes::Bytes), EngineError>,
}

enum PrepKind {
    Query(oneshot::Sender<Result<TorrentInfo, EngineError>>),
    Start(TorrentMedia),
}

fn magnet_trackers(m: &syncwatch_protocol::magnet::Magnet) -> Vec<Url> {
    m.trackers
        .iter()
        .filter_map(|t| Url::parse(t).ok())
        .collect()
}

/// Resolves the magnet's file list (validated) and the raw torrent for the later add.
async fn fetch_metadata(
    session: Arc<Session>,
    magnet: String,
    timeout: Duration,
    initial_peers: Vec<SocketAddr>,
    cap: usize,
) -> Result<(TorrentInfo, bytes::Bytes), EngineError> {
    let parsed = syncwatch_protocol::magnet::parse(&magnet).map_err(|_| EngineError::BadMagnet)?;
    let opts = AddTorrentOptions {
        list_only: true,
        initial_peers: (!initial_peers.is_empty()).then_some(initial_peers),
        ..Default::default()
    };
    let res = tokio::time::timeout(
        timeout,
        session.add_torrent(AddTorrent::from_url(&magnet), Some(opts)),
    )
    .await;
    let resp = match res {
        Err(_) => return Err(EngineError::MetadataTimeout),
        Ok(Err(e)) => {
            tracing::info!(error = %e, "torrent metadata failed");
            return Err(EngineError::Metadata);
        }
        Ok(Ok(AddTorrentResponse::ListOnly(r))) => r,
        Ok(Ok(_)) => return Err(EngineError::Metadata),
    };
    // The swarm's metadata is untrusted too: the same defensive parser as for .torrent files.
    let m =
        meta::inspect(&resp.torrent_bytes, cap, Some(parsed.infohash)).map_err(|e| match e {
            meta::MetaError::UnsafePath => EngineError::UnsafePath,
            meta::MetaError::TooLarge => EngineError::MetaTooLarge,
            _ => EngineError::Metadata,
        })?;
    let info = TorrentInfo::from_meta(&m);
    let bytes = bytes::Bytes::from(info.torrent.clone());
    Ok((info, bytes))
}

impl Actor {
    fn new(cfg: EngineConfig, events: mpsc::UnboundedSender<EngineEvent>) -> Self {
        let cache = Cache::open(cfg.cache_dir.clone(), cfg.cap_bytes);
        let (prep_tx, prep_rx) = mpsc::unbounded_channel();
        // No idle keep-alive connections: after the "stopped" announce nothing stays open.
        let http = reqwest::Client::builder()
            .timeout(STOPPED_TIMEOUT)
            .pool_max_idle_per_host(0)
            .build()
            .unwrap_or_default();
        Self {
            cfg,
            events,
            session: None,
            api: None,
            peer_id: new_peer_id(),
            cache,
            active: HashMap::new(),
            served: HashMap::new(),
            starting: HashMap::new(),
            rooms: HashMap::new(),
            durations: HashMap::new(),
            queries: 0,
            prep_tx,
            prep_rx: Some(prep_rx),
            http,
            epoch: Instant::now(),
        }
    }

    fn emit(&self, e: EngineEvent) {
        let _ = self.events.send(e);
    }

    fn now_us(&self) -> i64 {
        self.epoch.elapsed().as_micros() as i64
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        let mut tick = tokio::time::interval(self.cfg.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let Some(mut prep_rx) = self.prep_rx.take() else {
            return;
        };
        loop {
            tokio::select! {
                c = rx.recv() => match c {
                    Some(c) => self.on_cmd(c).await,
                    None => break,
                },
                Some(p) = prep_rx.recv() => self.on_prepared(p).await,
                _ = tick.tick() => self.on_tick().await,
            }
        }
        for (_, (_, h)) in self.starting.drain() {
            h.abort();
        }
        self.queries = 0;
        // The app is going away: stop everything (announce best-effort).
        let all: Vec<Infohash> = self.active.keys().copied().collect();
        for ih in all {
            self.stop(ih, StopReason::Leave).await;
        }
        self.shutdown_if_idle().await;
    }

    async fn ensure_session(&mut self) -> Result<Arc<Session>, EngineError> {
        if let Some(s) = &self.session {
            return Ok(s.clone());
        }
        let ip = self
            .cfg
            .bind_ip
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let opts = SessionOptions {
            dht: self.cfg.dht.then(|| librqbit::DhtSessionConfig {
                persistence: None,
                ..Default::default()
            }),
            fastresume: false,
            persistence: None,
            peer_id: Some(librqbit_core::Id20::new(self.peer_id)),
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: SocketAddr::new(ip, self.cfg.listen_port),
                enable_upnp_port_forwarding: false,
                ipv4_only: ip.is_ipv4(),
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            ipv4_only: ip.is_ipv4(),
            trackers: Default::default(),
            ..Default::default()
        };
        let _ = std::fs::create_dir_all(self.cache.dir());
        let s = Session::new_with_opts(self.cache.dir().to_path_buf(), opts)
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "torrent engine failed to start");
                EngineError::Engine
            })?;
        tracing::info!(
            dht = self.cfg.dht,
            port = s.listen_addr().map(|a| a.port()).unwrap_or(0),
            "torrent engine started"
        );
        self.api = Some(Api::new(s.clone(), None));
        self.session = Some(s.clone());
        Ok(s)
    }

    async fn shutdown_if_idle(&mut self) {
        if !self.active.is_empty() || !self.starting.is_empty() || self.queries > 0 {
            return;
        }
        if let Some(s) = self.session.take() {
            self.api = None;
            s.stop().await;
            drop(s);
            tracing::info!("torrent engine shut down (no DHT, no listeners, no sockets)");
            self.emit(EngineEvent::EngineDown);
        }
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::Metadata { magnet, reply } => {
                let Ok(parsed) = syncwatch_protocol::magnet::parse(&magnet) else {
                    let _ = reply.send(Err(EngineError::BadMagnet));
                    return;
                };
                match self.ensure_session().await {
                    Ok(s) => {
                        self.queries += 1;
                        self.spawn_fetch(s, magnet, parsed, PrepKind::Query(reply));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            Cmd::Start {
                media,
                meta,
                extra_peers,
                cached_only,
            } => self.start(media, meta, extra_peers, cached_only).await,
            Cmd::Stop {
                infohash,
                reason,
                reply,
            } => {
                self.stop(infohash, reason).await;
                self.shutdown_if_idle().await;
                if let Some(r) = reply {
                    let _ = r.send(());
                }
            }
            Cmd::Seek { infohash, ms } => {
                let now = self.now_us();
                if let Some(a) = self.active.get_mut(&infohash) {
                    let b = a.bytes.byte_for(ms);
                    a.policy.on_seek(b, now);
                    a.pending_seek_ms = Some(ms);
                }
            }
            Cmd::Duration { infohash, ms } => {
                match ms.filter(|d| *d > 0) {
                    Some(d) => {
                        self.durations.insert(infohash, d);
                    }
                    None => {
                        self.durations.remove(&infohash);
                    }
                }
                if let Some(a) = self.active.get_mut(&infohash) {
                    a.bytes.set_duration(ms);
                }
            }
            Cmd::Room { infohash, target } => {
                let retarget =
                    target.retarget || self.rooms.get(&infohash).is_none_or(|r| r.target.retarget);
                self.rooms.insert(
                    infohash,
                    RoomAt {
                        target: RoomTarget { retarget, ..target },
                        at: Instant::now(),
                    },
                );
                // Re-target now (not at the next tick): the room's part goes first.
                if retarget && self.active.contains_key(&infohash) {
                    self.on_tick().await;
                }
            }
            Cmd::Config { cfg } => {
                self.cache.set_cap(cfg.cap_bytes);
                self.cfg = *cfg;
            }
            Cmd::Running { reply } => {
                let _ = reply.send(self.session.is_some());
            }
        }
    }

    fn spawn_fetch(
        &self,
        session: Arc<Session>,
        magnet: String,
        parsed: syncwatch_protocol::magnet::Magnet,
        kind: PrepKind,
    ) -> tokio::task::AbortHandle {
        let tx = self.prep_tx.clone();
        let (timeout, peers, cap) = (
            self.cfg.metadata_timeout,
            self.cfg.initial_peers.clone(),
            self.cfg.meta_cap,
        );
        tokio::spawn(async move {
            let result = fetch_metadata(session, magnet, timeout, peers, cap).await;
            let _ = tx.send(Prepared {
                infohash: parsed.infohash,
                trackers: magnet_trackers(&parsed),
                kind,
                result,
            });
        })
        .abort_handle()
    }

    async fn on_prepared(&mut self, p: Prepared) {
        // Resolving the magnet announced "started" to its trackers: tell them we are gone.
        self.announce_stopped(p.infohash, StopReason::Cancel, p.trackers, 0, 0, 0);
        match p.kind {
            PrepKind::Query(reply) => {
                self.queries = self.queries.saturating_sub(1);
                let _ = reply.send(p.result.map(|(i, _)| i));
            }
            PrepKind::Start(media) => {
                // Stopped meanwhile? Then the result is dropped.
                if self.starting.remove(&p.infohash).is_some() {
                    let r = match p.result {
                        Ok((info, bytes)) => self.add(&media, info, bytes, Vec::new()).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = r {
                        self.cache.remove(&p.infohash);
                        self.fail(p.infohash, e);
                    }
                }
            }
        }
        self.shutdown_if_idle().await;
    }

    fn announce_stopped(
        &self,
        ih: Infohash,
        reason: StopReason,
        trackers: Vec<Url>,
        uploaded: u64,
        downloaded: u64,
        left: u64,
    ) {
        if trackers.is_empty() {
            return;
        }
        let port = self
            .session
            .as_ref()
            .and_then(|s| s.announce_port().or(s.listen_addr().map(|a| a.port())))
            .unwrap_or(self.cfg.listen_port);
        let a = StoppedAnnounce {
            info_hash: ih.0,
            peer_id: self.peer_id,
            port,
            uploaded,
            downloaded,
            left,
        };
        let http = self.http.clone();
        let events = self.events.clone();
        // Background: never blocks the UI, the room driver or this actor.
        tokio::spawn(async move {
            let results = send_stopped(&http, &trackers, a, STOPPED_TIMEOUT).await;
            let _ = events.send(EngineEvent::Announced {
                infohash: ih,
                reason,
                results,
            });
        });
    }

    async fn start(
        &mut self,
        media: TorrentMedia,
        given: Option<Vec<u8>>,
        extra_peers: Vec<SocketAddr>,
        cached_only: bool,
    ) {
        let ih = media.infohash;
        if self.active.contains_key(&ih) || self.served.contains_key(&ih) {
            return;
        }
        // Finished before: play the file, never re-join the swarm (ADR 0010).
        if let Some(path) = self.cache.finished(&ih, media.file_index, media.size) {
            match Loopback::start().await {
                Ok(lb) => {
                    lb.set_file(media.size, path);
                    let url = lb.url();
                    self.served.insert(ih, Served { loopback: lb });
                    tracing::info!("torrent file already complete: playing it without any engine");
                    self.emit(EngineEvent::Playable { infohash: ih, url });
                    self.emit(EngineEvent::Status {
                        infohash: ih,
                        status: complete_status(media.size),
                    });
                    self.emit(EngineEvent::Complete { infohash: ih });
                    // A host serving a finished file still offers its metadata to members.
                    if let Some(m) = given
                        .as_deref()
                        .and_then(|b| meta::inspect(b, self.cfg.meta_cap, Some(ih)).ok())
                    {
                        self.emit(EngineEvent::Metadata {
                            infohash: ih,
                            torrent: m.sanitized(),
                            port: 0,
                        });
                    }
                }
                Err(_) => self.fail(ih, EngineError::Engine),
            }
            return;
        }
        if cached_only {
            return;
        }
        // Metadata already known (host offer / .torrent): verify it and add directly.
        if let Some(bytes) = given {
            let r = match meta::inspect(&bytes, self.cfg.meta_cap, Some(ih)) {
                Ok(m) => {
                    let info = TorrentInfo::from_meta(&m);
                    let b = bytes::Bytes::from(info.torrent.clone());
                    self.add(&media, info, b, extra_peers).await
                }
                Err(meta::MetaError::HashMismatch) => Err(EngineError::MetaMismatch),
                Err(meta::MetaError::TooLarge) => Err(EngineError::MetaTooLarge),
                Err(meta::MetaError::UnsafePath) => Err(EngineError::UnsafePath),
                Err(_) => Err(EngineError::Metadata),
            };
            if let Err(e) = r {
                self.cache.remove(&ih);
                self.fail(ih, e);
                self.shutdown_if_idle().await;
            }
            return;
        }
        let parsed = match syncwatch_protocol::magnet::parse(&media.magnet) {
            Ok(p) => p,
            Err(_) => return self.fail(ih, EngineError::BadMagnet),
        };
        let trackers = parsed.trackers.len() as u32;
        let session = match self.ensure_session().await {
            Ok(s) => s,
            Err(e) => return self.fail(ih, e),
        };
        let h = self.spawn_fetch(
            session,
            media.magnet.clone(),
            parsed,
            PrepKind::Start(media.clone()),
        );
        self.starting.insert(ih, (media.clone(), h));
        self.emit(EngineEvent::Status {
            infohash: ih,
            status: TorrentStatus {
                phase: Phase::Metadata,
                have_bytes: 0,
                size: media.size,
                down_bps: 0,
                up_bps: 0,
                peers: 0,
                eta_secs: None,
                ranges: Vec::new(),
                window: None,
                exclusive: true,
                trackers,
                dht: self.cfg.dht,
                web_seeds: 0,
            },
        });
    }

    /// Adds the torrent (selected file only) once its metadata is known.
    async fn add(
        &mut self,
        media: &TorrentMedia,
        info: TorrentInfo,
        torrent_bytes: bytes::Bytes,
        extra_peers: Vec<SocketAddr>,
    ) -> Result<(), EngineError> {
        let ih = media.infohash;
        let entry = info
            .files
            .iter()
            .find(|f| f.index == media.file_index)
            .ok_or(EngineError::NoSuchFile)?;
        if entry.len != media.size {
            return Err(EngineError::SizeMismatch);
        }
        let protect: Vec<Infohash> = self.active.keys().copied().collect();
        if !self.cache.fits(media.size, &protect) {
            return Err(EngineError::CacheFull);
        }
        let dir = self.cache.torrent_dir(&ih);
        let _ = std::fs::create_dir_all(&dir);
        if let Some(free) = self.cfg.free_space.as_ref().and_then(|f| f(&dir))
            && free < media.size.saturating_add(64 * 1024 * 1024)
        {
            return Err(EngineError::DiskFull);
        }
        let session = self.ensure_session().await?;
        let parsed =
            syncwatch_protocol::magnet::parse(&media.magnet).map_err(|_| EngineError::BadMagnet)?;
        let opts = AddTorrentOptions {
            only_files: Some(vec![media.file_index as usize]),
            overwrite: true,
            output_folder: Some(dir.to_string_lossy().into_owned()),
            trackers: (!parsed.trackers.is_empty()).then(|| parsed.trackers.clone()),
            initial_peers: {
                let mut p = self.cfg.initial_peers.clone();
                p.extend(extra_peers);
                (!p.is_empty()).then_some(p)
            },
            ..Default::default()
        };
        let resp = session
            .add_torrent(AddTorrent::TorrentFileBytes(torrent_bytes), Some(opts))
            .await
            .map_err(|e| {
                tracing::info!(error = %e, "adding the torrent failed");
                EngineError::Engine
            })?;
        let (id, handle) = match resp {
            AddTorrentResponse::Added(id, h) | AddTorrentResponse::AlreadyManaged(id, h) => (id, h),
            AddTorrentResponse::ListOnly(_) => return Err(EngineError::Engine),
        };
        let _ = handle.wait_until_initialized().await;
        let file = media.file_index as usize;
        let meta = handle.metadata.load_full().ok_or(EngineError::Engine)?;
        let fi = meta.file_infos.get(file).ok_or(EngineError::NoSuchFile)?;
        let geo = FileGeometry::new(
            fi.len,
            meta.info.lengths().default_piece_length() as u64,
            fi.offset_in_torrent,
        );
        let rel = {
            let p = handle
                .output_folder()
                .join(&fi.relative_filename)
                .strip_prefix(&dir)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| fi.relative_filename.clone());
            p.to_string_lossy().replace('\\', "/")
        };
        let first_piece = fi.piece_range.start;
        let trackers: Vec<Url> = handle.shared().trackers.iter().cloned().collect();
        let loopback = Loopback::start().await.map_err(|_| EngineError::Engine)?;
        let api = self.api.clone().ok_or(EngineError::Engine)?;
        loopback.set_stream(fi.len, Arc::new(Opener { api, id, file }));
        tracing::info!(
            file_index = file,
            trackers = trackers.len(),
            "torrent added (selected file only)"
        );
        let meta_bytes = info.torrent.clone();
        self.active.insert(
            ih,
            Active {
                media: media.clone(),
                handle,
                id,
                file,
                first_piece,
                rel_path: rel,
                trackers,
                web_seeds: info.web_seeds,
                policy: PiecePolicy::new(self.cfg.policy, geo),
                juggler: Juggler::default(),
                bytes: ByteMap::new(fi.len, self.durations.get(&ih).copied()),
                last_read_seq: 0,
                last_read_end: 0,
                pending_seek_ms: None,
                ready: ReadyTracker::default(),
                loopback,
                playable_sent: false,
                no_peers_since: None,
                last_retry: Instant::now(),
                failed: false,
            },
        );
        let port = self
            .session
            .as_ref()
            .and_then(|s| s.listen_addr().map(|a| a.port()))
            .unwrap_or(0);
        self.emit(EngineEvent::Metadata {
            infohash: ih,
            torrent: meta_bytes,
            port,
        });
        Ok(())
    }

    fn fail(&self, ih: Infohash, error: EngineError) {
        tracing::info!(error = %error, "torrent failed");
        self.emit(EngineEvent::Failed {
            infohash: ih,
            error,
        });
    }

    fn haves(&self, a: &Active) -> Vec<bool> {
        let n = a.policy.geometry().pieces();
        let Some(api) = &self.api else {
            return vec![false; n];
        };
        match api.api_dump_haves(TorrentIdOrHash::Id(a.id)) {
            Ok((bf, _)) => (0..n)
                .map(|k| {
                    bf.get(a.first_piece as usize + k)
                        .map(|b| *b)
                        .unwrap_or(false)
                })
                .collect(),
            Err(_) => vec![false; n],
        }
    }

    async fn on_tick(&mut self) {
        let now = self.now_us();
        let ihs: Vec<Infohash> = self.active.keys().copied().collect();
        let mut finished = Vec::new();
        for ih in ihs {
            let have = match self.active.get(&ih) {
                Some(a) => self.haves(a),
                None => continue,
            };
            let api = self.api.clone();
            let cfg = self.cfg.clone();
            let Some(a) = self.active.get_mut(&ih) else {
                continue;
            };
            if a.failed {
                continue;
            }
            // The player's reads move the playhead; a new request far from the last read end is
            // the player's own seek.
            let r = *a.loopback.reads().borrow();
            if r.seq != a.last_read_seq {
                a.last_read_seq = r.seq;
                if r.new_request && r.pos.abs_diff(a.last_read_end) > 1024 * 1024 {
                    if let Some(ms) = a.pending_seek_ms.take() {
                        a.bytes.add_sample(ms, r.pos);
                    }
                    a.policy.on_seek(r.pos, now);
                } else {
                    a.policy.on_read(r.pos);
                }
                a.last_read_end = r.pos;
            }
            // The ROOM position (ADR 0012): its window goes first and decides readiness.
            let room = self.rooms.get_mut(&ih).map(|r| {
                let ms = r.ms_now();
                let retarget = std::mem::replace(&mut r.target.retarget, false);
                (ms, retarget)
            });
            let mut window = None;
            if let Some((ms, retarget)) = room {
                let rc = cfg.ready;
                let len = a.policy.geometry().len;
                if let Some((s, e)) = crate::ready::window(
                    &a.bytes,
                    len,
                    ms,
                    rc.window_ms,
                    rc.back_ms,
                    rc.min_bytes,
                    rc.max_bytes,
                ) {
                    if retarget {
                        a.policy.room_seek(s, e, now);
                        a.ready.reset();
                    } else {
                        a.policy.set_room(s, e);
                    }
                } else if retarget {
                    // No mapping yet: keep the re-target for when the duration arrives.
                    if let Some(r) = self.rooms.get_mut(&ih) {
                        r.target.retarget = true;
                    }
                }
                let geo = a.policy.geometry();
                window = crate::ready::evaluate(
                    &rc,
                    &mut a.ready,
                    &a.bytes,
                    &geo,
                    &have,
                    ms,
                    cfg.policy.head,
                    cfg.policy.tail,
                );
            }
            let plan = a.policy.plan(&have, now);
            if let Some(api) = api {
                let d = Driver {
                    api,
                    id: a.id,
                    file: a.file,
                };
                a.juggler.apply(&d, &plan.cursors).await;
            }
            let stats = a.handle.stats();
            if let Some(err) = &stats.error {
                a.failed = true;
                let e = if is_disk_full(err) {
                    EngineError::DiskFull
                } else {
                    EngineError::Engine
                };
                let _ = self.events.send(EngineEvent::Failed {
                    infohash: ih,
                    error: e,
                });
                continue;
            }
            let size = a.media.size;
            let have_bytes = stats.file_progress.get(a.file).copied().unwrap_or(0);
            let (down, up, peers, eta) = stats
                .live
                .as_ref()
                .map(|l| {
                    (
                        (l.download_speed.mbps * 1024.0 * 1024.0) as u64,
                        (l.upload_speed.mbps * 1024.0 * 1024.0) as u64,
                        l.snapshot.peer_stats.live,
                        None::<u64>,
                    )
                })
                .unwrap_or((0, 0, 0, None));
            // ETA from the remaining bytes of the selected file and the current speed.
            let eta =
                eta.or_else(|| (down > 0).then(|| size.saturating_sub(have_bytes) / down.max(1)));
            let complete = have_bytes >= size && plan.complete;
            if peers == 0 && !complete {
                let since = *a.no_peers_since.get_or_insert_with(Instant::now);
                // Waiting for peers: restart discovery periodically (pause + unpause).
                if since.elapsed() >= cfg.no_peers_after
                    && a.last_retry.elapsed() >= cfg.retry_every
                    && let Some(s) = &self.session
                {
                    a.last_retry = Instant::now();
                    tracing::info!("no peers: retrying peer discovery");
                    let _ = s.pause(&a.handle).await;
                    let _ = s.unpause(&a.handle).await;
                }
            } else {
                a.no_peers_since = None;
            }
            let no_peers = a
                .no_peers_since
                .is_some_and(|t| t.elapsed() >= cfg.no_peers_after);
            let phase = if complete {
                Phase::Complete
            } else if no_peers {
                Phase::NoPeers
            } else if a.playable_sent {
                Phase::Streaming
            } else {
                Phase::Downloading
            };
            if plan.playable && !a.playable_sent {
                a.playable_sent = true;
                let _ = self.events.send(EngineEvent::Playable {
                    infohash: ih,
                    url: a.loopback.url(),
                });
            }
            let _ = self.events.send(EngineEvent::Status {
                infohash: ih,
                status: TorrentStatus {
                    phase,
                    have_bytes,
                    size,
                    down_bps: down,
                    up_bps: up,
                    peers,
                    eta_secs: eta,
                    ranges: a.policy.geometry().ranges(&have),
                    window,
                    exclusive: plan.exclusive,
                    trackers: a.trackers.len() as u32,
                    dht: cfg.dht,
                    web_seeds: a.web_seeds as u32,
                },
            });
            if complete {
                finished.push(ih);
            }
        }
        for ih in finished {
            self.stop(ih, StopReason::Complete).await;
        }
        if !self.active.is_empty() || self.session.is_some() {
            self.shutdown_if_idle().await;
        }
    }

    /// The stop sequence (ADR 0010).
    async fn stop(&mut self, ih: Infohash, reason: StopReason) {
        self.rooms.remove(&ih);
        self.durations.remove(&ih);
        if let Some((media, h)) = self.starting.remove(&ih) {
            // Still resolving the magnet: abort it (drops its tracker/DHT lookups) and tell the
            // trackers we are gone.
            h.abort();
            tracing::info!(?reason, "torrent start cancelled");
            if let Ok(p) = syncwatch_protocol::magnet::parse(&media.magnet) {
                self.announce_stopped(ih, reason, magnet_trackers(&p), 0, 0, media.size);
            }
            self.cache.remove(&ih);
            return;
        }
        if let Some(s) = self.served.remove(&ih) {
            drop(s.loopback);
            if reason != StopReason::Complete && !self.cfg.keep_finished {
                self.cache.remove(&ih);
            }
            return;
        }
        let Some(mut a) = self.active.remove(&ih) else {
            return;
        };
        let path = self.cache.torrent_dir(&ih).join(&a.rel_path);
        if reason == StopReason::Complete {
            // 1. Handoff: the same URL now serves the finished file.
            a.loopback.set_file(a.media.size, path.clone());
        }
        // 2. Disconnect every peer: drop our streams, pause (cancels all peer tasks, PEX, the
        //    DHT get_peers/announce stream and the tracker loops).
        a.juggler.clear();
        let stats = a.handle.stats();
        if let Some(s) = &self.session {
            let _ = s.pause(&a.handle).await;
        }
        // 3. Our own "stopped" announce, in the background.
        let left = a
            .media
            .size
            .saturating_sub(stats.file_progress.get(a.file).copied().unwrap_or(0));
        self.announce_stopped(
            ih,
            reason,
            a.trackers.clone(),
            stats.uploaded_bytes,
            stats.progress_bytes,
            left,
        );
        // 4. Forget the torrent; keep the finished file, delete partial data.
        if let Some(s) = &self.session {
            let _ = s.delete(TorrentIdOrHash::Id(a.id), false).await;
        }
        drop(a.handle);
        if reason == StopReason::Complete {
            self.cache.prune_except(&ih, &a.rel_path);
            self.cache
                .record_finished(&ih, a.media.file_index, a.media.size, &a.rel_path);
            tracing::info!("{COMPLETE_MESSAGE}");
            self.served.insert(
                ih,
                Served {
                    loopback: a.loopback,
                },
            );
            self.emit(EngineEvent::Status {
                infohash: ih,
                status: complete_status(a.media.size),
            });
            self.emit(EngineEvent::Complete { infohash: ih });
        } else {
            drop(a.loopback);
            self.cache.remove(&ih);
            tracing::info!(?reason, "torrent stopped; partial download deleted");
        }
        // 5. Engine down when nothing else is active.
        self.shutdown_if_idle().await;
    }
}

fn complete_status(size: u64) -> TorrentStatus {
    TorrentStatus {
        phase: Phase::Complete,
        have_bytes: size,
        size,
        down_bps: 0,
        up_bps: 0,
        peers: 0,
        eta_secs: None,
        ranges: vec![(0, size)],
        window: Some(Readiness {
            ready: true,
            at_ms: 0,
            missing: 0,
            permille: 1000,
        }),
        exclusive: false,
        trackers: 0,
        dht: false,
        web_seeds: 0,
    }
}

fn is_disk_full(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("os error 112") || e.contains("not enough space") || e.contains("no space")
}

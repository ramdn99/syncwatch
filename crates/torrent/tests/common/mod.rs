//! Test swarm on 127.0.0.1 only: an in-process librqbit seeder, a fake HTTP tracker and a fake
//! UDP tracker that record every announce. Nothing touches a public network.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerMode, ListenerOptions, Session,
    SessionOptions, create_torrent, limits::LimitsConfig,
};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use syncwatch_protocol::{Infohash, TorrentMedia};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

pub const PIECE: u32 = 256 * 1024;

pub fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sw-torrent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Deterministic pseudo-random bytes (xorshift), different per seed.
pub fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Announce {
    pub at: Instant,
    pub event: Option<String>,
    pub peer_id: Vec<u8>,
    pub udp: bool,
}

#[derive(Clone, Default)]
pub struct TrackerLog(pub Arc<Mutex<Vec<Announce>>>);

impl TrackerLog {
    pub fn all(&self) -> Vec<Announce> {
        self.0.lock().unwrap().clone()
    }
    pub fn by_peer(&self, id: &[u8]) -> Vec<Announce> {
        self.all().into_iter().filter(|a| a.peer_id == id).collect()
    }
}

fn pct_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(0);
            out.push(h);
            i += 3;
        } else if b[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn compact(peers: &[SocketAddr]) -> Vec<u8> {
    let mut v = Vec::new();
    for p in peers {
        if let SocketAddr::V4(a) = p {
            v.extend_from_slice(&a.ip().octets());
            v.extend_from_slice(&a.port().to_be_bytes());
        }
    }
    v
}

/// HTTP tracker: records announces, answers with `peers`.
pub async fn http_tracker(peers: Arc<Mutex<Vec<SocketAddr>>>) -> (String, TrackerLog) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/announce", l.local_addr().unwrap());
    let log = TrackerLog::default();
    let lg = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let lg = lg.clone();
            let peers = peers.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                loop {
                    let Ok(k) = s.read(&mut buf[n..]).await else {
                        return;
                    };
                    if k == 0 {
                        return;
                    }
                    n += k;
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("");
                let query = path.split_once('?').map(|x| x.1).unwrap_or("");
                let mut event = None;
                let mut peer_id = Vec::new();
                for kv in query.split('&') {
                    if let Some((k, v)) = kv.split_once('=') {
                        match k {
                            "event" => event = Some(v.to_owned()),
                            "peer_id" => peer_id = pct_decode(v),
                            _ => {}
                        }
                    }
                }
                lg.0.lock().unwrap().push(Announce {
                    at: Instant::now(),
                    event,
                    peer_id,
                    udp: false,
                });
                let p = compact(&peers.lock().unwrap());
                let mut body = format!("d8:intervali1800e5:peers{}:", p.len()).into_bytes();
                body.extend_from_slice(&p);
                body.push(b'e');
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
                let _ = s.shutdown().await;
            });
        }
    });
    (url, log)
}

/// UDP tracker (BEP 15): records announces, answers with `peers`.
pub async fn udp_tracker(peers: Arc<Mutex<Vec<SocketAddr>>>) -> (String, TrackerLog) {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let url = format!("udp://{}/announce", s.local_addr().unwrap());
    let log = TrackerLog::default();
    let lg = log.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, from)) = s.recv_from(&mut buf).await else {
                return;
            };
            let b = &buf[..n];
            if n >= 16 && u32::from_be_bytes(b[8..12].try_into().unwrap()) == 0 && n == 16 {
                let mut r = Vec::new();
                r.extend_from_slice(&0u32.to_be_bytes());
                r.extend_from_slice(&b[12..16]);
                r.extend_from_slice(&0x1122_3344_5566_7788u64.to_be_bytes());
                let _ = s.send_to(&r, from).await;
            } else if n >= 98 && u32::from_be_bytes(b[8..12].try_into().unwrap()) == 1 {
                let ev = u32::from_be_bytes(b[80..84].try_into().unwrap());
                lg.0.lock().unwrap().push(Announce {
                    at: Instant::now(),
                    event: Some(
                        match ev {
                            0 => "none",
                            1 => "completed",
                            2 => "started",
                            3 => "stopped",
                            _ => "?",
                        }
                        .to_owned(),
                    ),
                    peer_id: b[36..56].to_vec(),
                    udp: true,
                });
                let mut r = Vec::new();
                r.extend_from_slice(&1u32.to_be_bytes());
                r.extend_from_slice(&b[12..16]);
                r.extend_from_slice(&1800u32.to_be_bytes());
                r.extend_from_slice(&0u32.to_be_bytes());
                r.extend_from_slice(&1u32.to_be_bytes());
                r.extend_from_slice(&compact(&peers.lock().unwrap()));
                let _ = s.send_to(&r, from).await;
            }
        }
    });
    (url, log)
}

pub struct Seeder {
    pub session: Arc<Session>,
    pub addr: SocketAddr,
    pub infohash: Infohash,
    pub torrent: bytes::Bytes,
    pub id: usize,
}

impl Seeder {
    pub fn api(&self) -> librqbit::Api {
        librqbit::Api::new(self.session.clone(), None)
    }
    pub fn uploaded(&self) -> u64 {
        self.api()
            .api_stats_v1(librqbit::api::TorrentIdOrHash::Id(self.id))
            .map(|s| s.uploaded_bytes)
            .unwrap_or(0)
    }
    pub fn live_peers(&self) -> u32 {
        self.api()
            .api_stats_v1(librqbit::api::TorrentIdOrHash::Id(self.id))
            .ok()
            .and_then(|s| s.live.map(|l| l.snapshot.peer_stats.live))
            .unwrap_or(0)
    }
}

/// A seeder for `root` (a file, or a folder for a multi-file torrent).
pub async fn seeder(root: &Path, trackers: Vec<String>, upload_bps: Option<u32>) -> Seeder {
    let spawner = librqbit::spawn_utils::BlockingSpawner::new(4);
    let created = create_torrent(
        root,
        CreateTorrentOptions {
            name: None,
            trackers,
            piece_length: Some(PIECE),
        },
        &spawner,
    )
    .await
    .unwrap();
    let ih = created.info_hash();
    let torrent = created.as_bytes().unwrap();
    let state = tmp(&format!("seeder-state-{}", ih.as_string()));
    let session = Session::new_with_opts(
        state,
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: "127.0.0.1:0".parse().unwrap(),
                ipv4_only: true,
                ..Default::default()
            }),
            ipv4_only: true,
            ratelimits: LimitsConfig {
                upload_bps: upload_bps.and_then(NonZeroU32::new),
                download_bps: None,
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let out = if root.is_dir() {
        root.to_path_buf()
    } else {
        root.parent().unwrap().to_path_buf()
    };
    let resp = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent.clone()),
            Some(AddTorrentOptions {
                output_folder: Some(out.to_string_lossy().into_owned()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    let (id, h) = match resp {
        librqbit::AddTorrentResponse::Added(id, h)
        | librqbit::AddTorrentResponse::AlreadyManaged(id, h) => (id, h),
        _ => panic!("list only"),
    };
    h.wait_until_initialized().await.unwrap();
    let addr = session.listen_addr().unwrap();
    Seeder {
        session,
        addr,
        infohash: Infohash(ih.0),
        torrent,
        id,
    }
}

pub fn magnet(ih: &Infohash, name: &str, trackers: &[String]) -> String {
    let mut m = format!("magnet:?xt=urn:btih:{}&dn={}", ih.to_hex(), name);
    for t in trackers {
        m.push_str("&tr=");
        for b in t.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                m.push(b as char);
            } else {
                m.push_str(&format!("%{b:02X}"));
            }
        }
    }
    m
}

pub fn media(ih: Infohash, file_index: u32, name: &str, size: u64, magnet: String) -> TorrentMedia {
    TorrentMedia {
        infohash: ih,
        file_index,
        name: name.to_owned(),
        size,
        magnet,
    }
}

/// Waits for an event matching `f` (others are kept in `seen`).
pub async fn wait_event<F: FnMut(&syncwatch_torrent::EngineEvent) -> bool>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<syncwatch_torrent::EngineEvent>,
    seen: &mut Vec<syncwatch_torrent::EngineEvent>,
    secs: u64,
    mut f: F,
) -> Option<syncwatch_torrent::EngineEvent> {
    // Already received (while waiting for something else)?
    if let Some(e) = seen.iter().find(|e| f(e)).cloned() {
        return Some(e);
    }
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let e = tokio::time::timeout_at(end, rx.recv()).await.ok()??;
        seen.push(e.clone());
        if f(&e) {
            return Some(e);
        }
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// GETs `[start, end)` from the loopback URL with a plain HTTP/1.1 request (one connection).
pub async fn http_get(url: &str, start: u64, end: Option<u64>) -> Vec<u8> {
    let u = url::Url::parse(url).unwrap();
    let mut s = tokio::net::TcpStream::connect((u.host_str().unwrap(), u.port().unwrap()))
        .await
        .unwrap();
    let range = match end {
        Some(e) => format!("bytes={start}-{}", e - 1),
        None => format!("bytes={start}-"),
    };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nRange: {range}\r\nConnection: close\r\n\r\n",
        u.path(),
        u.host_str().unwrap(),
        u.port().unwrap()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut all = Vec::new();
    s.read_to_end(&mut all).await.unwrap();
    let pos = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&all[..pos]).to_string();
    let body = all[pos + 4..].to_vec();
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(&body)
    } else {
        body
    }
}

pub fn dechunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some(nl) = b[i..].windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size =
            usize::from_str_radix(String::from_utf8_lossy(&b[i..i + nl]).trim(), 16).unwrap_or(0);
        i += nl + 2;
        if size == 0 {
            break;
        }
        out.extend_from_slice(&b[i..i + size]);
        i += size + 2;
    }
    out
}

pub fn engine_cfg(dir: PathBuf, port: u16) -> syncwatch_torrent::EngineConfig {
    let mut c = syncwatch_torrent::EngineConfig::new(dir);
    c.listen_port = port;
    c.bind_ip = Some("127.0.0.1".parse().unwrap());
    c.dht = false;
    c.metadata_timeout = Duration::from_secs(20);
    c.tick = Duration::from_millis(100);
    c
}

pub fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

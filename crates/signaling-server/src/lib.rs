//! `syncwatch-signaling`: a small WebSocket router for WebRTC signaling (spec section 1).
//!
//! - `GET /ws`: JSON signaling (see `syncwatch_protocol::signaling`).
//! - `GET /healthz`: "ok".
//!
//! Members can reach ONLY their room's host; the host can reach each of its members. The server
//! never sees passwords, credentials, chat or media, and never logs SDP or candidates. A host
//! that drops keeps its room for the grace period (30 s) and can resume with its token.

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use syncwatch_protocol::signaling::*;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct Config {
    pub max_rooms: usize,
    pub max_conns_per_ip: usize,
    pub host_grace: Duration,
    pub idle_timeout: Duration,
    /// Token bucket: refill per second and burst.
    pub rate_per_sec: f64,
    pub burst: f64,
    /// Upper bound for a room's size (including the host).
    pub max_members_cap: u8,
    /// Use the first X-Forwarded-For address (only behind a trusted reverse proxy).
    pub trust_proxy: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_rooms: 10_000,
            max_conns_per_ip: 32,
            host_grace: Duration::from_secs(HOST_GRACE_SECS as u64),
            idle_timeout: Duration::from_secs(60),
            rate_per_sec: 50.0,
            burst: 200.0,
            max_members_cap: 12,
            trust_proxy: false,
        }
    }
}

type Tx = mpsc::Sender<ServerMsg>;

struct Room {
    host: Option<(u64, Tx)>,
    token: String,
    members: BTreeMap<SigPeer, (u64, Tx)>,
    next_peer: SigPeer,
    max_members: u8,
}

#[derive(Default)]
struct Inner {
    rooms: HashMap<String, Room>,
    per_ip: HashMap<IpAddr, usize>,
    next_conn: u64,
}

pub struct Hub {
    cfg: Config,
    inner: Mutex<Inner>,
}

#[derive(Clone, Debug)]
enum Role {
    None,
    Host(String),
    Member(String, SigPeer),
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    // getrandom only fails on exotic platforms; fall back to time-based bytes.
    if getrandom::fill(&mut b).is_err() {
        let t = Instant::now().elapsed().as_nanos().to_le_bytes();
        for (i, x) in b.iter_mut().enumerate() {
            *x = t[i % t.len()] ^ (i as u8).wrapping_mul(151);
        }
    }
    b
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl Hub {
    pub fn new(cfg: Config) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            inner: Mutex::new(Inner::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn room_count(&self) -> usize {
        self.lock().rooms.len()
    }

    fn send(tx: &Tx, m: ServerMsg) {
        // A full queue means a stuck client; drop the message (it will time out).
        let _ = tx.try_send(m);
    }
}

pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .with_state(hub)
}

/// Serves until the listener fails or ctrl-c.
pub async fn serve(listener: tokio::net::TcpListener, hub: Arc<Hub>) -> std::io::Result<()> {
    axum::serve(
        listener,
        router(hub).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}

fn client_ip(hub: &Hub, addr: SocketAddr, headers: &HeaderMap) -> IpAddr {
    if hub.cfg.trust_proxy
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse().ok())
    {
        return ip;
    }
    addr.ip()
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(hub): State<Arc<Hub>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let ip = client_ip(&hub, addr, &headers);
    {
        let mut g = hub.lock();
        let n = g.per_ip.entry(ip).or_insert(0);
        if *n >= hub.cfg.max_conns_per_ip {
            return (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "too many connections",
            )
                .into_response();
        }
        *n += 1;
    }
    ws.max_message_size(MAX_SIGNAL_FRAME)
        .max_frame_size(MAX_SIGNAL_FRAME)
        .on_upgrade(move |socket| async move {
            run_connection(Arc::clone(&hub), socket).await;
            let mut g = hub.lock();
            if let Some(n) = g.per_ip.get_mut(&ip) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    g.per_ip.remove(&ip);
                }
            }
        })
        .into_response()
}

async fn run_connection(hub: Arc<Hub>, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(256);
    let conn_id = {
        let mut g = hub.lock();
        g.next_conn += 1;
        g.next_conn
    };
    let mut role = Role::None;
    let mut hello = false;
    let mut tokens = hub.cfg.burst;
    let mut last_refill = Instant::now();
    let mut violations = 0u32;
    let idle = hub.cfg.idle_timeout;
    let mut deadline = Instant::now() + idle;

    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(m) = out else { break };
                let close = matches!(m, ServerMsg::RoomClosed | ServerMsg::Error { code: SigError::Version });
                let Ok(text) = serde_json::to_string(&m) else { continue };
                if sink.send(Message::Text(text.into())).await.is_err() || close {
                    break;
                }
            }
            msg = stream.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => { deadline = Instant::now() + idle; continue }
                    Some(Ok(Message::Binary(_))) => { Hub::send(&tx, ServerMsg::Error { code: SigError::BadMessage }); continue }
                    _ => break,
                };
                deadline = Instant::now() + idle;
                // Token bucket.
                let now = Instant::now();
                tokens = (tokens + now.duration_since(last_refill).as_secs_f64() * hub.cfg.rate_per_sec).min(hub.cfg.burst);
                last_refill = now;
                if tokens < 1.0 {
                    violations += 1;
                    Hub::send(&tx, ServerMsg::Error { code: SigError::RateLimited });
                    if violations > 50 { break }
                    continue;
                }
                tokens -= 1.0;
                match parse_client(text.as_str()) {
                    Ok(m) => {
                        if !hello && !matches!(m, ClientMsg::Hello { .. }) {
                            Hub::send(&tx, ServerMsg::Error { code: SigError::BadMessage });
                            continue;
                        }
                        if handle(&hub, conn_id, &tx, &mut role, &mut hello, m) { break }
                    }
                    Err(code) => Hub::send(&tx, ServerMsg::Error { code }),
                }
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => break,
        }
    }
    disconnect(&hub, conn_id, &role);
}

/// Returns true when the connection should close.
fn handle(
    hub: &Arc<Hub>,
    conn: u64,
    tx: &Tx,
    role: &mut Role,
    hello: &mut bool,
    m: ClientMsg,
) -> bool {
    let reply = |m: ServerMsg| Hub::send(tx, m);
    match m {
        ClientMsg::Hello { v } => {
            if v != SIGNALING_VERSION {
                reply(ServerMsg::Error {
                    code: SigError::Version,
                });
                return false; // the Version error closes after sending
            }
            *hello = true;
            reply(ServerMsg::Welcome {
                v: SIGNALING_VERSION,
            });
        }
        ClientMsg::Ping => reply(ServerMsg::Pong),
        ClientMsg::Create { max_members } => {
            if !matches!(role, Role::None) {
                reply(ServerMsg::Error {
                    code: SigError::AlreadyInRoom,
                });
                return false;
            }
            let mut g = hub.lock();
            if g.rooms.len() >= hub.cfg.max_rooms {
                drop(g);
                reply(ServerMsg::Error {
                    code: SigError::ServerBusy,
                });
                return false;
            }
            let code = loop {
                let mut bytes = random_bytes::<32>().into_iter();
                let c = room_code_from_random(|| bytes.next().unwrap_or(0));
                if is_valid_room_code(&c) && !g.rooms.contains_key(&c) {
                    break c;
                }
            };
            let token: String = random_bytes::<16>()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            g.rooms.insert(
                code.clone(),
                Room {
                    host: Some((conn, tx.clone())),
                    token: token.clone(),
                    members: BTreeMap::new(),
                    next_peer: 1,
                    max_members: max_members.clamp(2, hub.cfg.max_members_cap),
                },
            );
            let rooms = g.rooms.len();
            drop(g);
            tracing::info!(rooms, "room created");
            *role = Role::Host(code.clone());
            reply(ServerMsg::Created { room: code, token });
        }
        ClientMsg::Resume { room, token } => {
            if !matches!(role, Role::None) {
                reply(ServerMsg::Error {
                    code: SigError::AlreadyInRoom,
                });
                return false;
            }
            let mut g = hub.lock();
            let Some(r) = g.rooms.get_mut(&room) else {
                drop(g);
                reply(ServerMsg::Error {
                    code: SigError::NoSuchRoom,
                });
                return false;
            };
            if r.host.is_some() || !ct_eq(&r.token, &token) {
                drop(g);
                reply(ServerMsg::Error {
                    code: SigError::BadToken,
                });
                return false;
            }
            r.host = Some((conn, tx.clone()));
            let peers: Vec<SigPeer> = r.members.keys().copied().collect();
            for (_, mtx) in r.members.values() {
                Hub::send(mtx, ServerMsg::HostBack);
            }
            drop(g);
            *role = Role::Host(room.clone());
            reply(ServerMsg::Resumed { room, peers });
        }
        ClientMsg::Join { room } => {
            if !matches!(role, Role::None) {
                reply(ServerMsg::Error {
                    code: SigError::AlreadyInRoom,
                });
                return false;
            }
            let mut g = hub.lock();
            let Some(r) = g.rooms.get_mut(&room).filter(|r| r.host.is_some()) else {
                drop(g);
                reply(ServerMsg::Error {
                    code: SigError::NoSuchRoom,
                });
                return false;
            };
            if r.members.len() + 1 >= r.max_members as usize {
                drop(g);
                reply(ServerMsg::Error {
                    code: SigError::RoomFull,
                });
                return false;
            }
            let peer = r.next_peer;
            r.next_peer += 1;
            r.members.insert(peer, (conn, tx.clone()));
            if let Some((_, htx)) = &r.host {
                Hub::send(htx, ServerMsg::PeerJoined { peer });
            }
            drop(g);
            *role = Role::Member(room.clone(), peer);
            reply(ServerMsg::Joined { room, peer });
        }
        ClientMsg::Signal { to, data } => {
            let g = hub.lock();
            match role.clone() {
                Role::Host(room) => {
                    let Some(r) = g.rooms.get(&room) else {
                        return true;
                    };
                    match to.and_then(|p| r.members.get(&p)) {
                        Some((_, mtx)) => Hub::send(mtx, ServerMsg::Signal { from: None, data }),
                        None => {
                            drop(g);
                            reply(ServerMsg::Error {
                                code: SigError::UnknownPeer,
                            });
                        }
                    }
                }
                Role::Member(room, peer) => {
                    // Members can only ever reach the host; `to` is ignored.
                    if let Some((_, htx)) = g.rooms.get(&room).and_then(|r| r.host.as_ref()) {
                        Hub::send(
                            htx,
                            ServerMsg::Signal {
                                from: Some(peer),
                                data,
                            },
                        );
                    }
                }
                Role::None => {
                    drop(g);
                    reply(ServerMsg::Error {
                        code: SigError::NotInRoom,
                    });
                }
            }
        }
        ClientMsg::Leave => {
            if let Role::Host(room) = role.clone() {
                close_room(hub, &room);
            } else {
                disconnect(hub, conn, role);
            }
            *role = Role::None;
        }
    }
    false
}

fn close_room(hub: &Hub, room: &str) {
    let mut g = hub.lock();
    if let Some(r) = g.rooms.remove(room) {
        for (_, mtx) in r.members.values() {
            Hub::send(mtx, ServerMsg::RoomClosed);
        }
        let rooms = g.rooms.len();
        drop(g);
        tracing::info!(rooms, "room closed");
    }
}

fn disconnect(hub: &Arc<Hub>, conn: u64, role: &Role) {
    match role {
        Role::None => {}
        Role::Member(room, peer) => {
            let mut g = hub.lock();
            if let Some(r) = g.rooms.get_mut(room)
                && r.members.get(peer).is_some_and(|(c, _)| *c == conn)
            {
                r.members.remove(peer);
                if let Some((_, htx)) = &r.host {
                    Hub::send(htx, ServerMsg::PeerLeft { peer: *peer });
                }
            }
        }
        Role::Host(room) => {
            let mut g = hub.lock();
            let Some(r) = g.rooms.get_mut(room) else {
                return;
            };
            if r.host.as_ref().is_some_and(|(c, _)| *c == conn) {
                r.host = None;
                for (_, mtx) in r.members.values() {
                    Hub::send(
                        mtx,
                        ServerMsg::HostGone {
                            grace_s: hub.cfg.host_grace.as_secs() as u16,
                        },
                    );
                }
                drop(g);
                // Grace period: close the room unless the host resumed.
                let hub2 = Arc::clone(hub);
                let room2 = room.clone();
                let grace = hub.cfg.host_grace;
                tokio::spawn(async move {
                    tokio::time::sleep(grace).await;
                    let still_gone = hub2
                        .lock()
                        .rooms
                        .get(&room2)
                        .is_some_and(|r| r.host.is_none());
                    if still_gone {
                        close_room(&hub2, &room2);
                    }
                });
            }
        }
    }
}

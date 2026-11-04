//! Standalone STUN/TURN connection test (spec section 5 "Connection test").
//!
//! Every URL is probed over its own transport (UDP, TCP or TLS). STUN URLs get a Binding
//! request (reachability + latency). TURN URLs get an Allocate with long-term authentication
//! (relay candidate + latency), and every allocation is RELEASED immediately with a Refresh
//! (LIFETIME=0), so the test relays no data and costs no quota. "Verified" needs at least one
//! relay candidate. Results never contain addresses or credentials.

use crate::model::{IceServer, IceUrl, Transport};
use crate::stun::{Message, attr, class, method};
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

pub const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
const UDP_RETRANSMIT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProbeKind {
    /// STUN reachability (server-reflexive candidate).
    Reflexive,
    /// TURN relay allocation.
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
pub enum ProbeError {
    #[error("host name could not be resolved")]
    Dns,
    #[error("connection refused")]
    Refused,
    #[error("no answer (port blocked or server down)")]
    Timeout,
    #[error("secure connection failed")]
    Tls,
    #[error("username or credential rejected")]
    BadCredentials,
    #[error("allocation quota reached")]
    Quota,
    #[error("server is at capacity")]
    Capacity,
    #[error("server error")]
    Server,
    #[error("unexpected answer")]
    Protocol,
    #[error("no network")]
    Offline,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlResult {
    /// e.g. "turn/tcp:443" (no hostname).
    pub label: String,
    pub host: String,
    pub kind: ProbeKind,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub error: Option<ProbeError>,
    /// TURN only: the allocation was released.
    pub released: bool,
    /// TURN only: STUN error code, if any (e.g. 401, 441, 486).
    pub stun_code: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub results: Vec<UrlResult>,
    /// At least one relay candidate was obtained.
    pub verified: bool,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub allocations: u32,
    pub allocations_released: u32,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    recv: AtomicU64,
}

enum Conn {
    Udp(UdpSocket, SocketAddr),
    Tcp(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

fn io_err(e: &std::io::Error) -> ProbeError {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset => {
            ProbeError::Refused
        }
        std::io::ErrorKind::TimedOut => ProbeError::Timeout,
        std::io::ErrorKind::NetworkUnreachable
        | std::io::ErrorKind::HostUnreachable
        | std::io::ErrorKind::AddrNotAvailable => ProbeError::Offline,
        _ => ProbeError::Refused,
    }
}

fn random_txid() -> [u8; 12] {
    // Not security sensitive; uniqueness per probe is enough.
    use std::hash::{BuildHasher, Hasher};
    let mut out = [0u8; 12];
    for chunk in out.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(Instant::now().elapsed().as_nanos());
        let v = h.finish().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    out
}

fn tls_connector() -> Result<tokio_rustls::TlsConnector, ProbeError> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| ProbeError::Tls)?;
    let config = builder
        .with_platform_verifier()
        .map_err(|_| ProbeError::Tls)?
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

impl Conn {
    async fn open(url: &IceUrl) -> Result<Self, ProbeError> {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((url.host.as_str(), url.port))
            .await
            .map_err(|_| ProbeError::Dns)?
            .collect();
        let addr = addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.first())
            .copied()
            .ok_or(ProbeError::Dns)?;
        match url.transport() {
            Transport::Udp => {
                let bind: SocketAddr = if addr.is_ipv4() {
                    "0.0.0.0:0".parse()
                } else {
                    "[::]:0".parse()
                }
                .map_err(|_| ProbeError::Offline)?;
                let s = UdpSocket::bind(bind).await.map_err(|e| io_err(&e))?;
                Ok(Conn::Udp(s, addr))
            }
            Transport::Tcp => Ok(Conn::Tcp(
                TcpStream::connect(addr).await.map_err(|e| io_err(&e))?,
            )),
            Transport::Tls => {
                let tcp = TcpStream::connect(addr).await.map_err(|e| io_err(&e))?;
                let name = rustls::pki_types::ServerName::try_from(url.host.clone())
                    .map_err(|_| ProbeError::Tls)?;
                let tls = tls_connector()?
                    .connect(name, tcp)
                    .await
                    .map_err(|_| ProbeError::Tls)?;
                Ok(Conn::Tls(Box::new(tls)))
            }
        }
    }

    async fn read_frame<S: AsyncReadExt + Unpin>(s: &mut S) -> Result<Vec<u8>, ProbeError> {
        let mut head = [0u8; crate::stun::HEADER_LEN];
        s.read_exact(&mut head).await.map_err(|e| io_err(&e))?;
        let total = Message::frame_len(&head).ok_or(ProbeError::Protocol)?;
        if total > crate::stun::MAX_MESSAGE {
            return Err(ProbeError::Protocol);
        }
        let mut buf = head.to_vec();
        buf.resize(total, 0);
        s.read_exact(&mut buf[crate::stun::HEADER_LEN..])
            .await
            .map_err(|e| io_err(&e))?;
        Ok(buf)
    }

    /// Sends a request and returns the matching response (by transaction id) and its RTT.
    async fn transact(
        &mut self,
        req: &[u8],
        txid: &[u8; 12],
        c: &Counters,
    ) -> Result<(Message, Vec<u8>, Duration), ProbeError> {
        let start = Instant::now();
        match self {
            Conn::Udp(sock, addr) => {
                let mut buf = vec![0u8; crate::stun::MAX_MESSAGE];
                loop {
                    sock.send_to(req, *addr).await.map_err(|e| io_err(&e))?;
                    c.sent.fetch_add(req.len() as u64, Ordering::Relaxed);
                    let deadline = Instant::now() + UDP_RETRANSMIT;
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() {
                            break;
                        }
                        match tokio::time::timeout(left, sock.recv_from(&mut buf)).await {
                            Err(_) => break,
                            Ok(Err(e)) => return Err(io_err(&e)),
                            Ok(Ok((n, from))) => {
                                c.recv.fetch_add(n as u64, Ordering::Relaxed);
                                if from != *addr {
                                    continue;
                                }
                                if let Ok(m) = Message::decode(&buf[..n])
                                    && m.txid == *txid
                                {
                                    return Ok((m, buf[..n].to_vec(), start.elapsed()));
                                }
                            }
                        }
                    }
                }
            }
            Conn::Tcp(s) => Self::stream_transact(s, req, txid, c, start).await,
            Conn::Tls(s) => Self::stream_transact(s.as_mut(), req, txid, c, start).await,
        }
    }

    async fn stream_transact<S: AsyncReadExt + AsyncWriteExt + Unpin>(
        s: &mut S,
        req: &[u8],
        txid: &[u8; 12],
        c: &Counters,
        start: Instant,
    ) -> Result<(Message, Vec<u8>, Duration), ProbeError> {
        s.write_all(req).await.map_err(|e| io_err(&e))?;
        s.flush().await.map_err(|e| io_err(&e))?;
        c.sent.fetch_add(req.len() as u64, Ordering::Relaxed);
        loop {
            let raw = Self::read_frame(s).await?;
            c.recv.fetch_add(raw.len() as u64, Ordering::Relaxed);
            let m = Message::decode(&raw).map_err(|_| ProbeError::Protocol)?;
            if m.txid == *txid {
                return Ok((m, raw, start.elapsed()));
            }
        }
    }
}

fn stun_error(code: u16) -> ProbeError {
    match code {
        401 | 441 => ProbeError::BadCredentials,
        486 => ProbeError::Quota,
        508 => ProbeError::Capacity,
        _ => ProbeError::Server,
    }
}

fn base_result(url: &IceUrl, kind: ProbeKind) -> UrlResult {
    UrlResult {
        label: url.short_label(),
        host: url.host.clone(),
        kind,
        ok: false,
        latency_ms: None,
        error: None,
        released: false,
        stun_code: None,
    }
}

async fn probe_stun(url: &IceUrl, c: &Counters) -> UrlResult {
    let mut r = base_result(url, ProbeKind::Reflexive);
    let res = async {
        let mut conn = Conn::open(url).await?;
        let txid = random_txid();
        let req = Message::new(method::BINDING | class::REQUEST, txid).encode(None, true);
        let (m, _, rtt) = conn.transact(&req, &txid, c).await?;
        if m.class() == class::SUCCESS
            && m.xor_address(attr::XOR_MAPPED_ADDRESS)
                .or_else(|| m.xor_address(attr::MAPPED_ADDRESS))
                .is_some()
        {
            Ok(rtt)
        } else {
            Err(ProbeError::Protocol)
        }
    };
    match tokio::time::timeout(PROBE_TIMEOUT, res).await {
        Ok(Ok(rtt)) => {
            r.ok = true;
            r.latency_ms = Some(rtt.as_millis().min(u32::MAX as u128) as u32);
        }
        Ok(Err(e)) => r.error = Some(e),
        Err(_) => r.error = Some(ProbeError::Timeout),
    }
    r
}

struct TurnAuth<'a> {
    username: &'a str,
    realm: Vec<u8>,
    nonce: Vec<u8>,
    key: Vec<u8>,
}

fn authed(typ: u16, a: &TurnAuth<'_>, extra: Vec<(u16, Vec<u8>)>) -> ([u8; 12], Vec<u8>) {
    let txid = random_txid();
    let mut m = Message::new(typ, txid);
    m.attrs.extend(extra);
    m = m
        .with(attr::USERNAME, a.username.as_bytes().to_vec())
        .with(attr::REALM, a.realm.clone())
        .with(attr::NONCE, a.nonce.clone());
    (txid, m.encode(Some(&a.key), true))
}

async fn probe_turn(url: &IceUrl, username: &str, credential: &str, c: &Counters) -> UrlResult {
    let mut r = base_result(url, ProbeKind::Relay);
    let res = async {
        let mut conn = Conn::open(url).await.map_err(|e| (e, None))?;
        let transport = vec![(
            attr::REQUESTED_TRANSPORT,
            vec![crate::stun::TRANSPORT_UDP, 0, 0, 0],
        )];
        // 1) Unauthenticated Allocate -> 401 with REALM + NONCE.
        let txid = random_txid();
        let mut first = Message::new(method::ALLOCATE | class::REQUEST, txid);
        first.attrs.extend(transport.clone());
        let (m, _, _) = conn
            .transact(&first.encode(None, true), &txid, c)
            .await
            .map_err(|e| (e, None))?;
        let realm = m.get(attr::REALM).map(<[u8]>::to_vec);
        let nonce = m.get(attr::NONCE).map(<[u8]>::to_vec);
        let (Some(realm), Some(nonce)) = (realm, nonce) else {
            return Err((ProbeError::Protocol, m.error_code().map(|e| e.0)));
        };
        let realm_s = String::from_utf8_lossy(&realm).into_owned();
        let mut auth = TurnAuth {
            username,
            key: crate::stun::long_term_key(username, &realm_s, credential),
            realm,
            nonce,
        };
        // 2) Authenticated Allocate (retry once on 438 Stale Nonce).
        let mut attempt = 0;
        let (allocated, rtt) = loop {
            let (txid, req) = authed(method::ALLOCATE | class::REQUEST, &auth, transport.clone());
            let (m, raw, rtt) = conn.transact(&req, &txid, c).await.map_err(|e| (e, None))?;
            if m.class() == class::SUCCESS {
                if !Message::verify_integrity(&raw, &auth.key) {
                    return Err((ProbeError::Protocol, None));
                }
                break (m, rtt);
            }
            let code = m.error_code().map(|e| e.0);
            if code == Some(438) && attempt == 0 {
                if let Some(n) = m.get(attr::NONCE) {
                    auth.nonce = n.to_vec();
                }
                attempt += 1;
                continue;
            }
            return Err((code.map(stun_error).unwrap_or(ProbeError::Protocol), code));
        };
        if allocated.xor_address(attr::XOR_RELAYED_ADDRESS).is_none() {
            return Err((ProbeError::Protocol, None));
        }
        // 3) Release immediately: Refresh with LIFETIME = 0.
        let mut released = false;
        for _ in 0..2 {
            let (txid, req) = authed(
                method::REFRESH | class::REQUEST,
                &auth,
                vec![(attr::LIFETIME, 0u32.to_be_bytes().to_vec())],
            );
            match conn.transact(&req, &txid, c).await {
                Ok((m, _, _)) if m.class() == class::SUCCESS => {
                    released = true;
                    break;
                }
                Ok((m, _, _)) if m.error_code().map(|e| e.0) == Some(438) => {
                    if let Some(n) = m.get(attr::NONCE) {
                        auth.nonce = n.to_vec();
                    }
                }
                _ => break,
            }
        }
        Ok((rtt, released))
    };
    match tokio::time::timeout(PROBE_TIMEOUT, res).await {
        Ok(Ok((rtt, released))) => {
            r.ok = true;
            r.latency_ms = Some(rtt.as_millis().min(u32::MAX as u128) as u32);
            r.released = released;
            if !released {
                tracing::warn!(url = %r.label, "TURN allocation could not be released explicitly; it expires on its own");
            }
        }
        Ok(Err((e, code))) => {
            r.error = Some(e);
            r.stun_code = code;
        }
        Err(_) => r.error = Some(ProbeError::Timeout),
    }
    r
}

/// Probes every URL of every server concurrently.
pub async fn test_servers(servers: &[IceServer]) -> TestReport {
    let counters = Arc::new(Counters::default());
    let mut tasks = Vec::new();
    for s in servers {
        for u in &s.urls {
            let u = u.clone();
            let c = Arc::clone(&counters);
            let creds = (s.username.clone(), s.credential.clone());
            tasks.push(tokio::spawn(async move {
                if u.scheme.is_turn() {
                    match creds {
                        (Some(user), Some(pass)) => probe_turn(&u, &user, &pass, &c).await,
                        _ => {
                            let mut r = base_result(&u, ProbeKind::Relay);
                            r.error = Some(ProbeError::BadCredentials);
                            r
                        }
                    }
                } else {
                    probe_stun(&u, &c).await
                }
            }));
        }
    }
    let mut results = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            results.push(r);
        }
    }
    let allocations = results
        .iter()
        .filter(|r| r.kind == ProbeKind::Relay && r.ok)
        .count() as u32;
    let released = results.iter().filter(|r| r.released).count() as u32;
    TestReport {
        verified: results.iter().any(|r| r.kind == ProbeKind::Relay && r.ok),
        results,
        bytes_sent: counters.sent.load(Ordering::Relaxed),
        bytes_received: counters.recv.load(Ordering::Relaxed),
        allocations,
        allocations_released: released,
    }
}

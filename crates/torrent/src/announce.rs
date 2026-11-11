//! Our own tracker "stopped" announce (ADR 0010): librqbit never sends one. Sent to EVERY
//! tracker of the torrent, in parallel, with a short timeout, on completion, leave, source
//! switch and cancel. Logs carry the tracker host only (URLs may contain private passkeys).

use std::net::SocketAddr;
use std::time::Duration;
use url::Url;

/// Per-tracker timeout for the whole exchange.
pub const STOPPED_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoppedAnnounce {
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
}

fn pct(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            s.push(*b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// The HTTP(S) announce URL with `event=stopped` (BEP 3). Existing query parameters (passkeys)
/// are kept.
pub fn http_url(tracker: &Url, a: &StoppedAnnounce) -> Option<Url> {
    if !matches!(tracker.scheme(), "http" | "https") {
        return None;
    }
    let ours = format!(
        "info_hash={}&peer_id={}&port={}&uploaded={}&downloaded={}&left={}&compact=1&event=stopped",
        pct(&a.info_hash),
        pct(&a.peer_id),
        a.port,
        a.uploaded,
        a.downloaded,
        a.left
    );
    let mut u = tracker.clone();
    let q = match tracker.query() {
        Some(q) if !q.is_empty() => format!("{q}&{ours}"),
        _ => ours,
    };
    u.set_query(Some(&q));
    Some(u)
}

const UDP_PROTOCOL_ID: u64 = 0x0417_2710_1980;
pub const UDP_EVENT_STOPPED: u32 = 3;

pub fn udp_connect_request(tx: u32) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&UDP_PROTOCOL_ID.to_be_bytes());
    b[8..12].copy_from_slice(&0u32.to_be_bytes());
    b[12..16].copy_from_slice(&tx.to_be_bytes());
    b
}

/// Connection id from a connect response for transaction `tx`.
pub fn udp_parse_connect(buf: &[u8], tx: u32) -> Option<u64> {
    if buf.len() < 16 {
        return None;
    }
    let action = u32::from_be_bytes(buf[0..4].try_into().ok()?);
    let rtx = u32::from_be_bytes(buf[4..8].try_into().ok()?);
    if action != 0 || rtx != tx {
        return None;
    }
    Some(u64::from_be_bytes(buf[8..16].try_into().ok()?))
}

/// BEP 15 announce with `event = 3` (stopped), `num_want = 0`.
pub fn udp_announce_request(conn: u64, tx: u32, key: u32, a: &StoppedAnnounce) -> [u8; 98] {
    let mut b = [0u8; 98];
    b[0..8].copy_from_slice(&conn.to_be_bytes());
    b[8..12].copy_from_slice(&1u32.to_be_bytes());
    b[12..16].copy_from_slice(&tx.to_be_bytes());
    b[16..36].copy_from_slice(&a.info_hash);
    b[36..56].copy_from_slice(&a.peer_id);
    b[56..64].copy_from_slice(&a.downloaded.to_be_bytes());
    b[64..72].copy_from_slice(&a.left.to_be_bytes());
    b[72..80].copy_from_slice(&a.uploaded.to_be_bytes());
    b[80..84].copy_from_slice(&UDP_EVENT_STOPPED.to_be_bytes());
    // ip = 0 (default), key, num_want = 0, port
    b[88..92].copy_from_slice(&key.to_be_bytes());
    b[92..96].copy_from_slice(&0i32.to_be_bytes());
    b[96..98].copy_from_slice(&a.port.to_be_bytes());
    b
}

fn rand_u32() -> u32 {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    u32::from_be_bytes(b)
}

/// Host of a tracker URL, for logs.
pub fn tracker_label(u: &Url) -> String {
    format!("{}://{}", u.scheme(), u.host_str().unwrap_or("?"))
}

async fn send_udp(tracker: &Url, a: &StoppedAnnounce) -> Result<(), &'static str> {
    let host = tracker.host_str().ok_or("no host")?;
    let port = tracker.port().ok_or("no port")?;
    let addr: SocketAddr = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "resolve")?
        .next()
        .ok_or("resolve")?;
    let bind: SocketAddr = if addr.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let sock = tokio::net::UdpSocket::bind(bind)
        .await
        .map_err(|_| "bind")?;
    sock.connect(addr).await.map_err(|_| "connect")?;
    let tx = rand_u32();
    sock.send(&udp_connect_request(tx))
        .await
        .map_err(|_| "send")?;
    let mut buf = [0u8; 64];
    let n = sock.recv(&mut buf).await.map_err(|_| "recv")?;
    let conn = udp_parse_connect(&buf[..n], tx).ok_or("bad connect reply")?;
    let tx2 = rand_u32();
    sock.send(&udp_announce_request(conn, tx2, rand_u32(), a))
        .await
        .map_err(|_| "send")?;
    // Wait for the tracker's reply (it confirms the announce arrived); the socket closes on drop.
    let mut reply = [0u8; 1500];
    let _ = sock.recv(&mut reply).await;
    Ok(())
}

async fn send_http(
    client: &reqwest::Client,
    tracker: &Url,
    a: &StoppedAnnounce,
) -> Result<(), &'static str> {
    let url = http_url(tracker, a).ok_or("scheme")?;
    let r = client.get(url).send().await.map_err(|_| "request")?;
    if r.status().is_success() {
        Ok(())
    } else {
        Err("status")
    }
}

/// Sends "stopped" to every tracker in parallel; each attempt is bounded by `timeout`. Returns
/// (tracker label, ok) per tracker. Never panics; errors are only logged.
pub async fn send_stopped(
    client: &reqwest::Client,
    trackers: &[Url],
    a: StoppedAnnounce,
    timeout: Duration,
) -> Vec<(String, bool)> {
    let futs = trackers.iter().map(|t| async move {
        let label = tracker_label(t);
        let r = tokio::time::timeout(timeout, async {
            match t.scheme() {
                "udp" => send_udp(t, &a).await,
                "http" | "https" => send_http(client, t, &a).await,
                _ => Err("unsupported scheme"),
            }
        })
        .await
        .unwrap_or(Err("timeout"));
        match r {
            Ok(()) => tracing::info!(tracker = %label, "tracker stopped announce sent"),
            Err(e) => {
                tracing::info!(tracker = %label, error = e, "tracker stopped announce failed")
            }
        }
        (label, r.is_ok())
    });
    futures_util::future::join_all(futs).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann() -> StoppedAnnounce {
        StoppedAnnounce {
            info_hash: [0xAB; 20],
            peer_id: *b"-SW0100-abcdefghijkl",
            port: 6881,
            uploaded: 10,
            downloaded: 2000,
            left: 0,
        }
    }

    #[test]
    fn http_url_keeps_passkeys_and_adds_event_stopped() {
        let t = Url::parse("http://tracker.test:8000/announce?passkey=XYZ")
            .unwrap_or_else(|e| panic!("{e}"));
        let u = http_url(&t, &ann()).unwrap_or_else(|| panic!("url"));
        let q = u.query().unwrap_or("");
        assert!(q.starts_with("passkey=XYZ&info_hash=%AB%AB"));
        assert!(q.contains(
            "&peer_id=-SW0100-abcdefghijkl&port=6881&uploaded=10&downloaded=2000&left=0"
        ));
        assert!(q.ends_with("&event=stopped"));
        assert!(
            http_url(
                &Url::parse("udp://x:1").unwrap_or_else(|e| panic!("{e}")),
                &ann()
            )
            .is_none()
        );
    }

    #[test]
    fn udp_packets_follow_bep15() {
        let c = udp_connect_request(7);
        assert_eq!(&c[..8], &0x0417_2710_1980u64.to_be_bytes());
        assert_eq!(&c[12..], &7u32.to_be_bytes());
        let mut reply = [0u8; 16];
        reply[4..8].copy_from_slice(&7u32.to_be_bytes());
        reply[8..].copy_from_slice(&99u64.to_be_bytes());
        assert_eq!(udp_parse_connect(&reply, 7), Some(99));
        assert_eq!(udp_parse_connect(&reply, 8), None);
        let a = udp_announce_request(99, 5, 1, &ann());
        assert_eq!(
            u32::from_be_bytes([a[8], a[9], a[10], a[11]]),
            1,
            "action announce"
        );
        assert_eq!(&a[16..36], &[0xAB; 20]);
        assert_eq!(
            u32::from_be_bytes([a[80], a[81], a[82], a[83]]),
            3,
            "event stopped"
        );
        assert_eq!(u16::from_be_bytes([a[96], a[97]]), 6881);
    }

    #[test]
    fn labels_never_contain_the_path_or_query() {
        let t = Url::parse("https://t.example/abc/announce?passkey=SECRET")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tracker_label(&t), "https://t.example");
    }
}

//! The standalone connection test against an in-process fake STUN/TURN server that checks
//! long-term authentication (spec section 5: "a fake TURN server that checks authentication;
//! release every relay allocation right after the test").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use syncwatch_ice_config::model::parse_ice_url;
use syncwatch_ice_config::stun::{Message, attr, class, long_term_key, method};
use syncwatch_ice_config::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const USER: &str = "test-user-1700000000";
const PASS: &str = "test-credential-xyz";
const REALM: &str = "syncwatch.test";
const NONCE: &str = "nonce-0001";

#[derive(Default)]
struct Stats {
    active: AtomicI64,
    allocations: AtomicU32,
    releases: AtomicU32,
    integrity_failures: AtomicU32,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    Quota,
}

fn handle(raw: &[u8], peer: SocketAddr, stats: &Stats, mode: Mode) -> Option<Vec<u8>> {
    let m = Message::decode(raw).ok()?;
    let key = long_term_key(USER, REALM, PASS);
    let reply = |typ: u16| Message::new(typ, m.txid);
    match m.method() {
        method::BINDING => Some(
            reply(method::BINDING | class::SUCCESS)
                .with(
                    attr::XOR_MAPPED_ADDRESS,
                    Message::xor_address_value(peer, &m.txid),
                )
                .encode(None, true),
        ),
        method::ALLOCATE | method::REFRESH => {
            let err = |code: u16, reason: &str| {
                reply(m.method() | class::ERROR)
                    .with(attr::ERROR_CODE, Message::error_value(code, reason))
                    .with(attr::REALM, REALM.as_bytes().to_vec())
                    .with(attr::NONCE, NONCE.as_bytes().to_vec())
                    .encode(None, true)
            };
            if m.get(attr::MESSAGE_INTEGRITY).is_none() {
                return Some(err(401, "Unauthorized"));
            }
            let user_ok = m.get(attr::USERNAME) == Some(USER.as_bytes());
            if !user_ok || !Message::verify_integrity(raw, &key) {
                stats.integrity_failures.fetch_add(1, Ordering::SeqCst);
                return Some(err(401, "Unauthorized"));
            }
            if m.method() == method::ALLOCATE {
                if mode == Mode::Quota {
                    return Some(err(486, "Allocation Quota Reached"));
                }
                stats.allocations.fetch_add(1, Ordering::SeqCst);
                stats.active.fetch_add(1, Ordering::SeqCst);
                let relay: SocketAddr = "203.0.113.50:50000".parse().ok()?;
                Some(
                    reply(method::ALLOCATE | class::SUCCESS)
                        .with(
                            attr::XOR_RELAYED_ADDRESS,
                            Message::xor_address_value(relay, &m.txid),
                        )
                        .with(
                            attr::XOR_MAPPED_ADDRESS,
                            Message::xor_address_value(peer, &m.txid),
                        )
                        .with(attr::LIFETIME, 600u32.to_be_bytes().to_vec())
                        .encode(Some(&key), true),
                )
            } else {
                if m.get(attr::LIFETIME) == Some(&0u32.to_be_bytes()[..]) {
                    stats.releases.fetch_add(1, Ordering::SeqCst);
                    stats.active.fetch_sub(1, Ordering::SeqCst);
                }
                Some(
                    reply(method::REFRESH | class::SUCCESS)
                        .with(attr::LIFETIME, 0u32.to_be_bytes().to_vec())
                        .encode(Some(&key), true),
                )
            }
        }
        _ => None,
    }
}

/// Starts a fake server on UDP and TCP (same port numbers not required). Returns ports.
async fn start(mode: Mode) -> (u16, u16, Arc<Stats>) {
    let stats = Arc::new(Stats::default());
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    let s = Arc::clone(&stats);
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Ok((n, peer)) = udp.recv_from(&mut buf).await {
            if let Some(r) = handle(&buf[..n], peer, &s, mode) {
                let _ = udp.send_to(&r, peer).await;
            }
        }
    });
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_port = tcp.local_addr().unwrap().port();
    let s = Arc::clone(&stats);
    tokio::spawn(async move {
        while let Ok((mut sock, peer)) = tcp.accept().await {
            let s = Arc::clone(&s);
            tokio::spawn(async move {
                loop {
                    let mut head = [0u8; 20];
                    if sock.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    let total = Message::frame_len(&head).unwrap();
                    let mut buf = head.to_vec();
                    buf.resize(total, 0);
                    if sock.read_exact(&mut buf[20..]).await.is_err() {
                        return;
                    }
                    if let Some(r) = handle(&buf, peer, &s, mode)
                        && sock.write_all(&r).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    (udp_port, tcp_port, stats)
}

fn server(urls: &[String], user: &str, pass: &str) -> IceServer {
    IceServer {
        urls: urls.iter().map(|u| parse_ice_url(u, 0).unwrap()).collect(),
        username: Some(user.into()),
        credential: Some(pass.into()),
    }
}

#[tokio::test]
async fn stun_and_turn_over_udp_and_tcp_and_every_allocation_is_released() {
    let (udp, tcp, stats) = start(Mode::Normal).await;
    let servers = vec![
        IceServer {
            urls: vec![parse_ice_url(&format!("stun:127.0.0.1:{udp}"), 0).unwrap()],
            username: None,
            credential: None,
        },
        server(
            &[
                format!("turn:127.0.0.1:{udp}"),
                format!("turn:127.0.0.1:{tcp}?transport=tcp"),
            ],
            USER,
            PASS,
        ),
    ];
    let report = test_servers(&servers).await;
    assert!(report.verified, "{report:?}");
    assert_eq!(report.results.len(), 3);
    for r in &report.results {
        assert!(r.ok, "{r:?}");
        assert!(r.latency_ms.is_some());
    }
    let relays: Vec<_> = report
        .results
        .iter()
        .filter(|r| r.kind == ProbeKind::Relay)
        .collect();
    assert_eq!(relays.len(), 2);
    assert!(
        relays.iter().all(|r| r.released),
        "every allocation released"
    );
    assert_eq!(report.allocations, 2);
    assert_eq!(report.allocations_released, 2);
    assert_eq!(stats.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(
        stats.active.load(Ordering::SeqCst),
        0,
        "no allocation left on the server"
    );
    assert_eq!(
        stats.integrity_failures.load(Ordering::SeqCst),
        0,
        "the server verified every authenticated message"
    );
    assert!(report.bytes_sent > 0 && report.bytes_received > 0);
    // Labels carry no hostname.
    assert!(
        report
            .results
            .iter()
            .all(|r| !r.label.contains("127.0.0.1"))
    );
}

#[tokio::test]
async fn wrong_credentials_are_reported_and_nothing_is_allocated() {
    let (udp, _tcp, stats) = start(Mode::Normal).await;
    let servers = vec![server(
        &[format!("turn:127.0.0.1:{udp}")],
        USER,
        "wrong-password",
    )];
    let report = test_servers(&servers).await;
    assert!(!report.verified);
    assert_eq!(report.results[0].error, Some(ProbeError::BadCredentials));
    assert_eq!(report.results[0].stun_code, Some(401));
    assert_eq!(stats.allocations.load(Ordering::SeqCst), 0);
    assert!(
        stats.integrity_failures.load(Ordering::SeqCst) >= 1,
        "the server really checked the integrity"
    );
}

#[tokio::test]
async fn quota_exhausted_is_reported() {
    let (udp, _tcp, _stats) = start(Mode::Quota).await;
    let report = test_servers(&[server(&[format!("turn:127.0.0.1:{udp}")], USER, PASS)]).await;
    assert!(!report.verified);
    assert_eq!(report.results[0].error, Some(ProbeError::Quota));
    assert_eq!(report.results[0].stun_code, Some(486));
}

#[tokio::test]
async fn stun_only_is_not_verified() {
    let (udp, _tcp, _stats) = start(Mode::Normal).await;
    let report = test_servers(&[IceServer {
        urls: vec![parse_ice_url(&format!("stun:127.0.0.1:{udp}"), 0).unwrap()],
        username: None,
        credential: None,
    }])
    .await;
    assert!(report.results[0].ok);
    assert!(!report.verified, "verified needs a relay candidate");
}

#[tokio::test]
async fn refused_and_dns_failures_are_classified() {
    // A TCP port nobody listens on.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let report = test_servers(&[
        server(
            &[format!("turn:127.0.0.1:{port}?transport=tcp")],
            USER,
            PASS,
        ),
        server(&["turn:does-not-exist.invalid:3478".to_owned()], USER, PASS),
    ])
    .await;
    assert_eq!(report.results[0].error, Some(ProbeError::Refused));
    assert_eq!(report.results[1].error, Some(ProbeError::Dns));
}

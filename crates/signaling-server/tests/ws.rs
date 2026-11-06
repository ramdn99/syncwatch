#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use syncwatch_protocol::signaling::*;
use syncwatch_signaling::{Config, Hub, serve};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(cfg: Config) -> std::net::SocketAddr {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(serve(l, Hub::new(cfg)));
    addr
}

async fn connect(addr: std::net::SocketAddr) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    send(
        &mut ws,
        &ClientMsg::Hello {
            v: SIGNALING_VERSION,
        },
    )
    .await;
    assert_eq!(
        recv(&mut ws).await,
        ServerMsg::Welcome {
            v: SIGNALING_VERSION
        }
    );
    ws
}

async fn send(ws: &mut Ws, m: &ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(m).unwrap().into()))
        .await
        .unwrap();
}

async fn recv(ws: &mut Ws) -> ServerMsg {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timeout")
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            return parse_server(t.as_str()).unwrap();
        }
    }
}

async fn create(ws: &mut Ws) -> (String, String) {
    send(ws, &ClientMsg::Create { max_members: 12 }).await;
    match recv(ws).await {
        ServerMsg::Created { room, token } => (room, token),
        other => panic!("{other:?}"),
    }
}

async fn join(ws: &mut Ws, room: &str) -> SigPeer {
    send(ws, &ClientMsg::Join { room: room.into() }).await;
    match recv(ws).await {
        ServerMsg::Joined { peer, .. } => peer,
        other => panic!("{other:?}"),
    }
}

fn offer(s: &str) -> SignalData {
    SignalData::Offer { sdp: s.into() }
}

#[tokio::test]
async fn healthz() {
    let addr = start(Config::default()).await;
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(
        out.starts_with("HTTP/1.1 200") && out.ends_with("ok"),
        "{out}"
    );
}

#[tokio::test]
async fn host_and_members_signal_and_members_cannot_reach_each_other() {
    let addr = start(Config::default()).await;
    let mut host = connect(addr).await;
    let (room, _) = create(&mut host).await;
    assert!(is_valid_room_code(&room));
    let mut a = connect(addr).await;
    let mut b = connect(addr).await;
    let pa = join(&mut a, &room).await;
    assert_eq!(recv(&mut host).await, ServerMsg::PeerJoined { peer: pa });
    let pb = join(&mut b, &room).await;
    assert_eq!(recv(&mut host).await, ServerMsg::PeerJoined { peer: pb });

    // Member -> host (even if it tries to address member b).
    send(
        &mut a,
        &ClientMsg::Signal {
            to: Some(pb),
            data: offer("from-a"),
        },
    )
    .await;
    assert_eq!(
        recv(&mut host).await,
        ServerMsg::Signal {
            from: Some(pa),
            data: offer("from-a")
        }
    );
    // Host -> a specific member.
    send(
        &mut host,
        &ClientMsg::Signal {
            to: Some(pb),
            data: SignalData::Answer { sdp: "to-b".into() },
        },
    )
    .await;
    assert_eq!(
        recv(&mut b).await,
        ServerMsg::Signal {
            from: None,
            data: SignalData::Answer { sdp: "to-b".into() }
        }
    );
    // b never received a's offer.
    send(&mut b, &ClientMsg::Ping).await;
    assert_eq!(recv(&mut b).await, ServerMsg::Pong);
    // Host addressing an unknown peer.
    send(
        &mut host,
        &ClientMsg::Signal {
            to: Some(99),
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut host).await,
        ServerMsg::Error {
            code: SigError::UnknownPeer
        }
    );
    // Member leaves -> host is told.
    drop(a);
    assert_eq!(recv(&mut host).await, ServerMsg::PeerLeft { peer: pa });
}

#[tokio::test]
async fn host_grace_resume_and_expiry() {
    let addr = start(Config {
        host_grace: Duration::from_millis(400),
        ..Config::default()
    })
    .await;
    let mut host = connect(addr).await;
    let (room, token) = create(&mut host).await;
    let mut m = connect(addr).await;
    let p = join(&mut m, &room).await;
    assert_eq!(recv(&mut host).await, ServerMsg::PeerJoined { peer: p });
    drop(host);
    assert_eq!(recv(&mut m).await, ServerMsg::HostGone { grace_s: 0 });
    // Wrong token is refused; the right one resumes.
    let mut h2 = connect(addr).await;
    send(
        &mut h2,
        &ClientMsg::Resume {
            room: room.clone(),
            token: "00".repeat(16),
        },
    )
    .await;
    assert_eq!(
        recv(&mut h2).await,
        ServerMsg::Error {
            code: SigError::BadToken
        }
    );
    send(
        &mut h2,
        &ClientMsg::Resume {
            room: room.clone(),
            token,
        },
    )
    .await;
    assert_eq!(
        recv(&mut h2).await,
        ServerMsg::Resumed {
            room: room.clone(),
            peers: vec![p]
        }
    );
    assert_eq!(recv(&mut m).await, ServerMsg::HostBack);
    // Drop again and let the grace period expire.
    drop(h2);
    assert_eq!(recv(&mut m).await, ServerMsg::HostGone { grace_s: 0 });
    assert_eq!(recv(&mut m).await, ServerMsg::RoomClosed);
    let mut late = connect(addr).await;
    send(&mut late, &ClientMsg::Join { room }).await;
    assert_eq!(
        recv(&mut late).await,
        ServerMsg::Error {
            code: SigError::NoSuchRoom
        }
    );
}

#[tokio::test]
async fn limits_version_full_and_rate() {
    let addr = start(Config {
        rate_per_sec: 1.0,
        burst: 5.0,
        ..Config::default()
    })
    .await;
    // Wrong version.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    send(&mut ws, &ClientMsg::Hello { v: 999 }).await;
    assert_eq!(
        recv(&mut ws).await,
        ServerMsg::Error {
            code: SigError::Version
        }
    );
    // Messages before Hello are refused.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    send(&mut ws, &ClientMsg::Ping).await;
    assert_eq!(
        recv(&mut ws).await,
        ServerMsg::Error {
            code: SigError::BadMessage
        }
    );
    // Room full (2 = host + 1).
    let mut host = connect(addr).await;
    send(&mut host, &ClientMsg::Create { max_members: 2 }).await;
    let ServerMsg::Created { room, .. } = recv(&mut host).await else {
        panic!()
    };
    let mut a = connect(addr).await;
    join(&mut a, &room).await;
    let mut b = connect(addr).await;
    send(&mut b, &ClientMsg::Join { room: room.clone() }).await;
    assert_eq!(
        recv(&mut b).await,
        ServerMsg::Error {
            code: SigError::RoomFull
        }
    );
    // Oversized signal.
    send(
        &mut a,
        &ClientMsg::Signal {
            to: None,
            data: offer(&"x".repeat(MAX_SDP + 10)),
        },
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Error {
            code: SigError::TooLarge
        }
    );
    // Rate limit: burst 5 at 1/s.
    let mut c = connect(addr).await;
    for _ in 0..10 {
        send(&mut c, &ClientMsg::Ping).await;
    }
    let mut limited = 0;
    for _ in 0..10 {
        if recv(&mut c).await
            == (ServerMsg::Error {
                code: SigError::RateLimited,
            })
        {
            limited += 1;
        }
    }
    assert!(limited >= 4, "{limited}");
}

#[tokio::test]
async fn host_transfer_swaps_roles_and_routes_to_the_new_host() {
    let addr = start(Config::default()).await;
    let mut host = connect(addr).await;
    let (room, _) = create(&mut host).await;
    let mut a = connect(addr).await;
    send(&mut a, &ClientMsg::Join { room: room.clone() }).await;
    let ServerMsg::Joined { peer: pa, .. } = recv(&mut a).await else {
        panic!()
    };
    let mut b = connect(addr).await;
    send(&mut b, &ClientMsg::Join { room: room.clone() }).await;
    let ServerMsg::Joined { peer: pb, .. } = recv(&mut b).await else {
        panic!()
    };
    assert!(matches!(
        recv(&mut host).await,
        ServerMsg::PeerJoined { .. }
    ));
    assert!(matches!(
        recv(&mut host).await,
        ServerMsg::PeerJoined { .. }
    ));

    // Only the host may transfer.
    send(&mut b, &ClientMsg::TransferHost { to: pa }).await;
    assert_eq!(
        recv(&mut b).await,
        ServerMsg::Error {
            code: SigError::NotHost
        }
    );

    send(&mut host, &ClientMsg::TransferHost { to: pa }).await;
    let ServerMsg::HostChanged {
        you: Some(old_host_peer),
    } = recv(&mut host).await
    else {
        panic!()
    };
    let ServerMsg::BecameHost { peers, .. } = recv(&mut a).await else {
        panic!()
    };
    assert!(peers.contains(&pb) && peers.contains(&old_host_peer) && !peers.contains(&pa));
    assert_eq!(recv(&mut b).await, ServerMsg::HostChanged { you: None });

    // Members (including the former host) now reach the new host only.
    send(
        &mut b,
        &ClientMsg::Signal {
            to: None,
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Signal {
            from: Some(pb),
            data: SignalData::EndOfCandidates
        }
    );
    send(
        &mut host,
        &ClientMsg::Signal {
            to: None,
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Signal {
            from: Some(old_host_peer),
            data: SignalData::EndOfCandidates
        }
    );
    // And the new host can address them.
    send(
        &mut a,
        &ClientMsg::Signal {
            to: Some(old_host_peer),
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut host).await,
        ServerMsg::Signal {
            from: None,
            data: SignalData::EndOfCandidates
        }
    );
}

#[tokio::test]
async fn a_member_can_take_over_only_while_the_host_is_gone() {
    let cfg = Config {
        host_grace: Duration::from_secs(5),
        ..Config::default()
    };
    let addr = start(cfg).await;
    let mut host = connect(addr).await;
    let (room, _) = create(&mut host).await;
    let mut a = connect(addr).await;
    send(&mut a, &ClientMsg::Join { room: room.clone() }).await;
    let ServerMsg::Joined { .. } = recv(&mut a).await else {
        panic!()
    };
    let mut b = connect(addr).await;
    send(&mut b, &ClientMsg::Join { room: room.clone() }).await;
    let ServerMsg::Joined { peer: pb, .. } = recv(&mut b).await else {
        panic!()
    };
    // While the host is there, a claim is refused.
    send(&mut a, &ClientMsg::ClaimHost).await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Error {
            code: SigError::NotHost
        }
    );
    // The host crashes.
    drop(host);
    assert!(matches!(recv(&mut a).await, ServerMsg::HostGone { .. }));
    assert!(matches!(recv(&mut b).await, ServerMsg::HostGone { .. }));
    send(&mut a, &ClientMsg::ClaimHost).await;
    let ServerMsg::BecameHost { peers, .. } = recv(&mut a).await else {
        panic!()
    };
    assert_eq!(peers, vec![pb]);
    assert_eq!(recv(&mut b).await, ServerMsg::HostChanged { you: None });
    // A second claim loses.
    send(&mut b, &ClientMsg::ClaimHost).await;
    assert_eq!(
        recv(&mut b).await,
        ServerMsg::Error {
            code: SigError::NotHost
        }
    );
    // The new host routes, and the room survives the old host's grace period.
    send(
        &mut b,
        &ClientMsg::Signal {
            to: None,
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Signal {
            from: Some(pb),
            data: SignalData::EndOfCandidates
        }
    );
    tokio::time::sleep(Duration::from_secs(6)).await;
    send(&mut a, &ClientMsg::Ping).await;
    assert_eq!(recv(&mut a).await, ServerMsg::Pong);
    send(
        &mut b,
        &ClientMsg::Signal {
            to: None,
            data: SignalData::EndOfCandidates,
        },
    )
    .await;
    assert_eq!(
        recv(&mut a).await,
        ServerMsg::Signal {
            from: Some(pb),
            data: SignalData::EndOfCandidates
        }
    );
}

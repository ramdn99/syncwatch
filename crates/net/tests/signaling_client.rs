#![allow(clippy::unwrap_used, clippy::expect_used)]

use syncwatch_net::signaling::{SignalingConn, SignalingError};
use syncwatch_protocol::signaling::*;

#[tokio::test]
async fn client_talks_to_the_real_server() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(syncwatch_signaling::serve(
        l,
        syncwatch_signaling::Hub::new(Default::default()),
    ));
    let url = format!("ws://127.0.0.1:{}/ws", addr.port());
    let mut host = SignalingConn::connect(&url).await.unwrap();
    host.send(&ClientMsg::Create { max_members: 4 })
        .await
        .unwrap();
    let ServerMsg::Created { room, .. } = host.recv().await.unwrap() else {
        panic!()
    };
    let mut m = SignalingConn::connect(&url).await.unwrap();
    m.send(&ClientMsg::Join { room }).await.unwrap();
    assert!(matches!(m.recv().await.unwrap(), ServerMsg::Joined { .. }));
    assert!(matches!(
        host.recv().await.unwrap(),
        ServerMsg::PeerJoined { .. }
    ));
}

#[tokio::test]
async fn refused_and_dns_errors_name_the_host() {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let e = SignalingConn::connect(&format!("ws://127.0.0.1:{port}/ws"))
        .await
        .err()
        .unwrap();
    assert_eq!(
        e,
        SignalingError::Refused {
            host: "127.0.0.1".into()
        }
    );
    let e = SignalingConn::connect("wss://does-not-exist.invalid/ws")
        .await
        .err()
        .unwrap();
    assert_eq!(
        e,
        SignalingError::Dns {
            host: "does-not-exist.invalid".into()
        }
    );
    assert!(e.to_string().contains("does-not-exist.invalid"));
}

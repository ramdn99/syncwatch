use syncwatch_protocol::magnet::{self, MagnetError};
use syncwatch_protocol::*;

fn sample_identity() -> FileIdentity {
    FileIdentity {
        sample_blake3: [7; 32],
        size: 1_288_181_962,
        duration_ms: Some(1_708_352),
    }
}

#[test]
fn roundtrip_and_version_check() {
    let msg = Message::Request(Request::Seek {
        position_ms: 95_000,
    });
    let frame = encode(&msg).unwrap();
    assert_eq!(decode(&frame).unwrap(), msg);

    // Same message with another protocol version is rejected.
    let env = Envelope {
        version: PROTOCOL_VERSION + 1,
        message: msg,
    };
    let other = postcard::to_allocvec(&env).unwrap();
    assert_eq!(
        decode(&other),
        Err(ProtocolError::Version(PROTOCOL_VERSION + 1))
    );
}

#[test]
fn garbage_and_oversized_frames_are_rejected() {
    assert_eq!(decode(&[0xff, 0xff, 0xff]), Err(ProtocolError::Malformed));
    let big = vec![0u8; limits::MAX_FRAME_BYTES + 1];
    assert!(matches!(decode(&big), Err(ProtocolError::FrameTooLarge(_))));
}

#[test]
fn field_limits_are_enforced_on_decode() {
    let long_chat = Message::ChatSend {
        client_id: 1,
        text: "x".repeat(limits::MAX_CHAT_BYTES + 1),
        claimed_media_ms: None,
    };
    assert!(encode(&long_chat).is_err());
    // Bypass encode() validation to make sure decode() validates too.
    let env = Envelope {
        version: PROTOCOL_VERSION,
        message: long_chat,
    };
    let frame = postcard::to_allocvec(&env).unwrap();
    assert_eq!(decode(&frame), Err(ProtocolError::Limit("chat text")));

    let chunk = Message::FileChunk {
        external_id: ExternalId(1),
        offset: 0,
        data: vec![0; limits::CHUNK_BYTES + 1],
    };
    assert!(encode(&chunk).is_err());
    let empty = Message::FileChunk {
        external_id: ExternalId(1),
        offset: 0,
        data: vec![],
    };
    assert!(encode(&empty).is_err());
}

/// No message type can carry main-media bytes. Exactly one kind carries opaque bytes
/// (FileChunk), it is size-limited to one 16 KiB chunk, and it refers to an ExternalId
/// (a manifest entry), never to the room media. Media descriptors carry only names, sizes and
/// hashes.
#[test]
fn no_media_over_datachannel_enumeration() {
    let bulk: Vec<_> = MessageKind::ALL
        .iter()
        .filter(|k| k.carries_bulk_bytes())
        .collect();
    assert_eq!(bulk, vec![&MessageKind::FileChunk]);
    assert_eq!(limits::CHUNK_BYTES, 16 * 1024);

    // A media descriptor serializes to a small, fixed-size-ish blob regardless of media size.
    let d = MediaDescriptor::LocalFile(LocalMedia {
        name: "test.mkv".into(),
        identity: sample_identity(),
    });
    let frame = encode(&Message::Request(Request::ChangeMedia {
        request_id: 1,
        descriptor: d,
    }))
    .unwrap();
    assert!(
        frame.len() < 128,
        "descriptor frame is {} bytes",
        frame.len()
    );

    // Every kind maps to a channel and priority; only FileChunk is Bulk priority.
    for k in MessageKind::ALL {
        let bulk_priority = k == MessageKind::FileChunk;
        assert_eq!(k.carries_bulk_bytes(), bulk_priority);
    }
}

#[test]
fn channels_and_priorities() {
    let hb = Message::Heartbeat {
        playback: RoomPlayback::initial(MediaEpoch(1), 0),
        host_now_us: 0,
    };
    assert_eq!(hb.channel(), Channel::Unreliable);
    assert_eq!(hb.priority(), Priority::Heartbeat);
    let c = Message::Command(Command::RoomClosed);
    assert_eq!(c.channel(), Channel::Reliable);
    assert!(
        Priority::Control < Priority::Chat
            && Priority::Chat < Priority::Heartbeat
            && Priority::Heartbeat < Priority::Bulk
    );
}

#[test]
fn playback_extrapolation_holds_before_scheduled_anchor() {
    let p = RoomPlayback {
        media_epoch: MediaEpoch(1),
        epoch: Epoch(3),
        position_ms: 10_000,
        anchor_host_us: 5_000_000,
        paused: false,
        speed: 1000,
    };
    assert_eq!(
        p.position_at(4_000_000),
        10_000,
        "held before a scheduled start"
    );
    assert!(!p.is_playing_at(4_999_999));
    assert_eq!(p.position_at(6_000_000), 11_000);
    let fast = RoomPlayback { speed: 1500, ..p };
    assert_eq!(fast.position_at(7_000_000), 13_000);
    let paused = RoomPlayback { paused: true, ..p };
    assert_eq!(paused.position_at(9_000_000), 10_000);
}

#[test]
fn torrent_descriptor_must_match_its_magnet() {
    let m = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x.mkv";
    let ih = magnet::parse(m).unwrap().infohash;
    let good = MediaDescriptor::Torrent(TorrentMedia {
        infohash: ih,
        file_index: 0,
        name: "x.mkv".into(),
        size: 1,
        magnet: m.into(),
    });
    assert!(good.validate().is_ok());
    let bad = MediaDescriptor::Torrent(TorrentMedia {
        infohash: Infohash([0; 20]),
        file_index: 0,
        name: "x.mkv".into(),
        size: 1,
        magnet: m.into(),
    });
    assert!(bad.validate().is_err());
}

#[test]
fn magnet_vectors() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/magnet-vectors.json"
    ))
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    for case in v["valid"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let m = magnet::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
        assert_eq!(
            m.infohash.to_hex(),
            case["infohash"].as_str().unwrap(),
            "{input}"
        );
        assert_eq!(m.display_name.as_deref(), case["dn"].as_str(), "{input}");
        let trackers: Vec<String> = case["trackers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(m.trackers, trackers, "{input}");
    }
    for case in v["invalid"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let err = magnet::parse(input).expect_err(input);
        let name = match err {
            MagnetError::NotMagnet => "NotMagnet",
            MagnetError::TooLong => "TooLong",
            MagnetError::NoInfohash => "NoInfohash",
            MagnetError::BadInfohash => "BadInfohash",
            MagnetError::ConflictingInfohash => "ConflictingInfohash",
            MagnetError::V2Only => "V2Only",
            MagnetError::BadEncoding => "BadEncoding",
        };
        assert_eq!(name, case["error"].as_str().unwrap(), "{input}");
    }
    assert_eq!(
        magnet::parse(&format!("magnet:?xt=urn:btih:{}", "a".repeat(9000))),
        Err(MagnetError::TooLong)
    );
}

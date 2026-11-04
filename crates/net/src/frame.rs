//! Binary framing for the bridge IPC (no base64) and for the data-channel wire.
//!
//! Rust -> bridge (a Tauri `Channel` with raw bodies) and bridge -> Rust (a raw-body `invoke`):
//!   DATA:    [0x01][peer: u32 LE][lane: u8][payload]
//!   CONTROL: [0x02][UTF-8 JSON]          (Rust -> bridge commands)
//!   EVENT:   [0x02][UTF-8 JSON]          (bridge -> Rust events)
//!   BATCH:   [0x03]([len u32 LE][frame])*  (both directions; whatever queued up while the
//!            previous IPC message was in flight)
//!
//! Data-channel payloads start with a kind byte: 0x00 application frame, 0x01 keepalive
//! (generated and consumed by Rust; the bridge never creates traffic on its own).

use crate::transport::{Lane, PeerId};
use serde::{Deserialize, Serialize};

pub const OP_DATA: u8 = 1;
pub const OP_JSON: u8 = 2;
pub const WIRE_APP: u8 = 0;
pub const WIRE_KEEPALIVE: u8 = 1;
/// Largest frame crossing the IPC (a 64 KiB protocol frame plus headers).
pub const MAX_IPC_FRAME: usize = 70 * 1024;
pub const OP_BATCH: u8 = 3;
/// Target size of one batched IPC message.
pub const MAX_BATCH: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Data {
        peer: PeerId,
        lane: Lane,
        payload: Vec<u8>,
    },
    Json(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("empty frame")]
    Empty,
    #[error("unknown opcode")]
    Opcode,
    #[error("truncated frame")]
    Truncated,
    #[error("bad lane")]
    Lane,
    #[error("frame too large")]
    TooLarge,
}

pub fn encode_data(peer: PeerId, lane: Lane, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + payload.len());
    v.push(OP_DATA);
    v.extend_from_slice(&peer.to_le_bytes());
    v.push(lane as u8);
    v.extend_from_slice(payload);
    v
}

pub fn encode_json<T: Serialize>(value: &T) -> Vec<u8> {
    let mut v = vec![OP_JSON];
    // Serializing our own plain structs cannot fail; fall back to "null".
    v.extend(serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec()));
    v
}

/// Several frames in one IPC message: [0x03] then repeated [len u32 LE][frame].
/// Batching is what makes bulk transfer fast: one IPC round trip carries up to
/// [`MAX_BATCH`] bytes instead of one 16 KiB chunk.
pub fn encode_batch(frames: &[Vec<u8>]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + frames.iter().map(|f| f.len() + 4).sum::<usize>());
    v.push(OP_BATCH);
    for f in frames {
        v.extend_from_slice(&(f.len() as u32).to_le_bytes());
        v.extend_from_slice(f);
    }
    v
}

/// Decodes a single frame or a batch into its frames.
pub fn decode_all(buf: &[u8]) -> Result<Vec<Frame>, FrameError> {
    if buf.first() != Some(&OP_BATCH) {
        return decode(buf).map(|f| vec![f]);
    }
    if buf.len() > MAX_BATCH + 4096 {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::new();
    let mut rest = &buf[1..];
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(FrameError::Truncated);
        }
        let n = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let body = rest.get(4..4 + n).ok_or(FrameError::Truncated)?;
        out.push(decode(body)?);
        rest = &rest[4 + n..];
    }
    Ok(out)
}

pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
    if buf.len() > MAX_IPC_FRAME {
        return Err(FrameError::TooLarge);
    }
    match buf.first() {
        None => Err(FrameError::Empty),
        Some(&OP_DATA) => {
            if buf.len() < 6 {
                return Err(FrameError::Truncated);
            }
            let peer = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
            let lane = Lane::from_u8(buf[5]).ok_or(FrameError::Lane)?;
            Ok(Frame::Data {
                peer,
                lane,
                payload: buf[6..].to_vec(),
            })
        }
        Some(&OP_JSON) => Ok(Frame::Json(buf[1..].to_vec())),
        Some(_) => Err(FrameError::Opcode),
    }
}

/// Wraps an application frame for the data channel.
pub fn wire_app(data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(data.len() + 1);
    v.push(WIRE_APP);
    v.extend_from_slice(data);
    v
}

/// A keepalive: kind byte + sequence + sender monotonic time (for gap/RTT stats).
pub fn wire_keepalive(seq: u32, sent_us: i64) -> Vec<u8> {
    let mut v = vec![WIRE_KEEPALIVE];
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(&sent_us.to_le_bytes());
    v
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    App(Vec<u8>),
    Keepalive { seq: u32, sent_us: i64 },
}

pub fn decode_wire(p: &[u8]) -> Option<Wire> {
    match *p.first()? {
        WIRE_APP => Some(Wire::App(p[1..].to_vec())),
        WIRE_KEEPALIVE if p.len() == 13 => Some(Wire::Keepalive {
            seq: u32::from_le_bytes([p[1], p[2], p[3], p[4]]),
            sent_us: i64::from_le_bytes([p[5], p[6], p[7], p[8], p[9], p[10], p[11], p[12]]),
        }),
        _ => None,
    }
}

/// Commands Rust sends to the bridge page.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum BridgeCommand {
    CreatePeer {
        peer: PeerId,
        offerer: bool,
        ice: crate::transport::IceConfig,
    },
    RemoteSignal {
        peer: PeerId,
        data: syncwatch_protocol::signaling::SignalData,
    },
    ClosePeer {
        peer: PeerId,
    },
    Ping {
        seq: u32,
    },
    /// Dev only: stop forwarding traffic for a peer (simulates a silent link).
    DevDropTraffic {
        peer: PeerId,
    },
    /// Dev only: block the page's event loop (simulates a hung bridge).
    DevFreeze,
    /// Dev only: in-page loopback data-channel benchmark (two local peer connections).
    DevBench {
        bytes: u64,
        chunk: u32,
    },
}

/// Events the bridge page sends to Rust.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "camelCase")]
pub enum BridgeEvent {
    Ready,
    Pong {
        seq: u32,
    },
    LocalSignal {
        peer: PeerId,
        data: syncwatch_protocol::signaling::SignalData,
    },
    Open {
        peer: PeerId,
        relayed: bool,
        #[serde(rename = "localSdp")]
        local_sdp: String,
        #[serde(rename = "remoteSdp")]
        remote_sdp: String,
    },
    State {
        peer: PeerId,
        ice: String,
        connection: String,
    },
    /// Flow-control credit: `rx` = cumulative wire bytes the bridge accepted from Rust for this
    /// peer, `buffered` = current `bufferedAmount` of both channels. Sent after each batch.
    Buffered {
        peer: PeerId,
        rx: u64,
        buffered: u64,
    },
    Closed {
        peer: PeerId,
        reason: String,
    },
    Error {
        peer: Option<PeerId>,
        message: String,
    },
    /// Dev only: the loopback benchmark finished (Rust measures the time).
    BenchDone {
        bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_frames_round_trip_without_base64() {
        let payload: Vec<u8> = (0..=255).collect();
        let f = encode_data(7, Lane::Unreliable, &payload);
        assert_eq!(f.len(), 6 + 256, "raw bytes, no encoding overhead");
        assert_eq!(
            decode(&f).unwrap(),
            Frame::Data {
                peer: 7,
                lane: Lane::Unreliable,
                payload
            }
        );
        assert_eq!(decode(&[]), Err(FrameError::Empty));
        assert_eq!(decode(&[1, 0, 0]), Err(FrameError::Truncated));
        assert_eq!(decode(&[1, 0, 0, 0, 0, 9]), Err(FrameError::Lane));
        assert_eq!(decode(&[9]), Err(FrameError::Opcode));
        assert_eq!(
            decode(&vec![1; MAX_IPC_FRAME + 1]),
            Err(FrameError::TooLarge)
        );
    }

    #[test]
    fn batches_round_trip() {
        let a = encode_data(1, Lane::Reliable, b"abc");
        let b = encode_json(&BridgeCommand::Ping { seq: 1 });
        let all = decode_all(&encode_batch(&[a.clone(), b])).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], decode(&a).unwrap());
        assert!(matches!(all[1], Frame::Json(_)));
        assert_eq!(
            decode_all(&a).unwrap().len(),
            1,
            "single frames still decode"
        );
        assert_eq!(
            decode_all(&[OP_BATCH, 9, 0, 0, 0, 1]),
            Err(FrameError::Truncated)
        );
        assert_eq!(decode_all(&[OP_BATCH]).unwrap(), vec![]);
    }

    #[test]
    fn json_frames() {
        let f = encode_json(&BridgeCommand::Ping { seq: 3 });
        let Frame::Json(j) = decode(&f).unwrap() else {
            panic!()
        };
        assert_eq!(
            std::str::from_utf8(&j).unwrap(),
            r#"{"cmd":"ping","seq":3}"#
        );
        let ev: BridgeEvent = serde_json::from_str(
            r#"{"ev":"open","peer":1,"relayed":true,"localSdp":"a","remoteSdp":"b"}"#,
        )
        .unwrap();
        assert!(matches!(ev, BridgeEvent::Open { relayed: true, .. }));
    }

    #[test]
    fn wire_kinds() {
        assert_eq!(
            decode_wire(&wire_app(b"hi")),
            Some(Wire::App(b"hi".to_vec()))
        );
        assert_eq!(
            decode_wire(&wire_keepalive(9, -5)),
            Some(Wire::Keepalive {
                seq: 9,
                sent_us: -5
            })
        );
        assert_eq!(decode_wire(&[1, 2]), None);
        assert_eq!(decode_wire(&[]), None);
    }
}

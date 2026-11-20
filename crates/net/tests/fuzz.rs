//! Fuzz-style tests (Milestone 9 security review): the bridge IPC frames, the data-channel
//! wire, bridge events and SDP fingerprints with seeded random mutations. Invites and replies
//! have their own mutation test in src/invite.rs. Run longer with SYNCWATCH_FUZZ_ITERS.

use syncwatch_net::frame::{self, BridgeEvent, Frame};
use syncwatch_net::sdp::dtls_fingerprint;
use syncwatch_net::transport::Lane;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn iters() -> usize {
    std::env::var("SYNCWATCH_FUZZ_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4000)
}

/// One to four random edits: flip, set to a boundary byte, insert, delete, truncate, duplicate.
fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut v = src.to_vec();
    for _ in 0..=rng.below(4) {
        let at = rng.below(v.len() + 1);
        match rng.below(6) {
            0 if at < v.len() => v[at] ^= 1 << rng.below(8),
            1 if at < v.len() => v[at] = [0, 1, 0x7f, 0x80, 0xff][rng.below(5)],
            2 => v.insert(at, rng.next() as u8),
            3 if at < v.len() => {
                v.remove(at);
            }
            4 => v.truncate(at),
            5 if !v.is_empty() => {
                let from = rng.below(v.len());
                let len = rng.below(16).min(v.len() - from);
                let chunk = v[from..from + len].to_vec();
                v.splice(at..at, chunk);
            }
            _ => {}
        }
    }
    v
}

#[test]
fn ipc_frames_survive_mutations() {
    let mut rng = Rng(0x4e45_5401);
    let one = frame::encode_data(7, Lane::Bulk, &[5; 64]);
    let json = frame::encode_json(&serde_json::json!({"ev": "pong", "seq": 3}));
    let batch = frame::encode_batch(&[one.clone(), json.clone(), one.clone()]);
    let seeds = [one, json, batch];
    for i in 0..iters() {
        let m = mutate(&mut rng, &seeds[i % seeds.len()]);
        if let Ok(frames) = frame::decode_all(&m) {
            // Every decoded frame came from inside the input: no amplification.
            let total: usize = frames
                .iter()
                .map(|f| match f {
                    Frame::Data { payload, .. } => payload.len(),
                    Frame::Json(j) => j.len(),
                })
                .sum();
            assert!(total <= m.len());
        }
        let _ = frame::decode_wire(&m);
    }
}

#[test]
fn bridge_events_and_sdp_survive_mutations() {
    let mut rng = Rng(0x4e45_5402);
    let ev = br#"{"ev":"localSignal","peer":3,"data":{"type":"candidate","candidate":"candidate:1 1 udp 2122260223 192.0.2.1 54400 typ host","sdpMid":"0","sdpMLineIndex":0}}"#;
    let sdp = b"v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\na=fingerprint:sha-256 AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89\r\n";
    for _ in 0..iters() {
        let m = mutate(&mut rng, ev);
        let _ = serde_json::from_slice::<BridgeEvent>(&m);
        let s = mutate(&mut rng, sdp);
        if let Some(fp) = dtls_fingerprint(&String::from_utf8_lossy(&s)) {
            assert!(fp.len() <= 200, "fingerprint too long: {}", fp.len());
        }
    }
}

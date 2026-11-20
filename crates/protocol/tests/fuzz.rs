//! Fuzz-style tests (Milestone 9 security review): seeded random mutations of valid inputs and
//! random bytes against every decoder that reads peer or user input. Each must return an error
//! or a value that passes its own validation, and never panic. Deterministic (fixed seeds), so a
//! failure is reproducible; run longer with SYNCWATCH_FUZZ_ITERS.

use syncwatch_protocol::*;

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

fn samples() -> Vec<Message> {
    vec![
        Message::Request(Request::Seek {
            position_ms: 95_000,
        }),
        Message::ChatSend {
            client_id: 7,
            text: "hello zqx \u{1F600}".into(),
            claimed_media_ms: Some(12_345),
        },
        Message::FileChunk {
            external_id: ExternalId(3),
            offset: 4096,
            data: vec![9; 300],
        },
    ]
}

#[test]
fn protocol_decode_survives_mutations_and_random_bytes() {
    let mut rng = Rng(0x5157_4e43_0001);
    let frames: Vec<Vec<u8>> = samples().iter().map(|m| encode(m).unwrap()).collect();
    let (mut ok, mut err) = (0, 0);
    for i in 0..iters() {
        let input = if i % 5 == 0 {
            (0..rng.below(200)).map(|_| rng.next() as u8).collect()
        } else {
            mutate(&mut rng, &frames[i % frames.len()])
        };
        match decode(&input) {
            // Whatever decodes must also pass validation again (decode validates).
            Ok(m) => {
                ok += 1;
                assert!(encode(&m).is_ok(), "decoded but not re-encodable: {m:?}");
            }
            Err(_) => err += 1,
        }
        let _ = auth::decode_auth(&input);
    }
    assert!(err > 0 && ok + err == iters());
}

#[test]
fn signaling_parsers_survive_mutations() {
    let mut rng = Rng(0x5157_4e43_0002);
    let seeds = [
        r#"{"type":"join","room":"ABCD-EFGH","peer":"p1"}"#,
        r#"{"type":"welcome","peers":["a","b"],"you":"c"}"#,
        r#"{"type":"signal","to":"x","data":{"sdp":"v=0"}}"#,
    ];
    for i in 0..iters() {
        let m = mutate(&mut rng, seeds[i % seeds.len()].as_bytes());
        let text = String::from_utf8_lossy(&m);
        let _ = signaling::parse_client(&text);
        let _ = signaling::parse_server(&text);
        let _ = signaling::normalize_room_code(&text);
    }
}

#[test]
fn magnet_parser_survives_mutations() {
    let mut rng = Rng(0x5157_4e43_0003);
    let seed = "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x.mkv&tr=udp%3A%2F%2Ftracker.example%3A80&xl=1000";
    for _ in 0..iters() {
        let m = mutate(&mut rng, seed.as_bytes());
        let _ = magnet::parse(&String::from_utf8_lossy(&m));
    }
}

#[test]
fn file_names_from_peers_are_checked() {
    let mut rng = Rng(0x5157_4e43_0004);
    for _ in 0..iters() {
        let m = mutate(&mut rng, b"episode 01.ass");
        let name = String::from_utf8_lossy(&m);
        if check_file_name(&name).is_ok() {
            assert!(!name.contains(['/', '\\', ':', '\0']), "accepted {name:?}");
            assert!(name != "." && name != "..");
        }
    }
}

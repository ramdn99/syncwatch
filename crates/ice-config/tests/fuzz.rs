//! Fuzz-style tests (Milestone 9 security review): the pasted ICE-servers parser and the STUN
//! decoder (answers from the network) with seeded random mutations and random bytes.
//! Placeholder credentials only. Run longer with SYNCWATCH_FUZZ_ITERS.

use syncwatch_ice_config::parse_ice_servers;
use syncwatch_ice_config::stun::Message;

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
fn pasted_ice_servers_survive_mutations() {
    let mut rng = Rng(0x4943_4501);
    let seed = br#"const iceServers = [
  { urls: "stun:stun.relay.example:80" },
  { urls: "turn:<app-name>.relay.example:443?transport=tcp", username: "<USERNAME>", credential: "<CREDENTIAL>" },
  { urls: "turns:<app-name>.relay.example:443?transport=tcp", username: "<USERNAME>", credential: "<CREDENTIAL>" },
];"#;
    for _ in 0..iters() {
        let m = mutate(&mut rng, seed);
        if let Ok(list) = parse_ice_servers(&String::from_utf8_lossy(&m)) {
            assert!(!list.is_empty());
        }
    }
}

#[test]
fn stun_decoder_survives_mutations_and_random_bytes() {
    let mut rng = Rng(0x4943_4502);
    let seed = Message {
        typ: 0x0113,
        txid: [7; 12],
        attrs: vec![
            (0x0009, vec![0, 0, 4, 1, b'x', b'y']),
            (0x0014, b"realm".to_vec()),
            (0x0015, vec![1; 16]),
        ],
    }
    .encode(Some(&[3; 16]), true);
    for i in 0..iters() {
        let m = if i % 4 == 0 {
            (0..rng.below(120)).map(|_| rng.next() as u8).collect()
        } else {
            mutate(&mut rng, &seed)
        };
        if let Ok(d) = Message::decode(&m) {
            let n: usize = d.attrs.iter().map(|a| a.1.len()).sum();
            assert!(n <= m.len());
        }
        let _ = Message::verify_integrity(&m, &[3; 16]);
        let _ = Message::verify_fingerprint(&m);
    }
}

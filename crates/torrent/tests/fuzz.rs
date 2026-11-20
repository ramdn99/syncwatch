//! Fuzz-style tests (Milestone 9 security review): the loopback server's Range header and the
//! torrent path checks with seeded random mutations. The .torrent parser has its own mutation
//! test next to its builders (src/meta.rs). Run longer with SYNCWATCH_FUZZ_ITERS.

use syncwatch_torrent::server::parse_range;
use syncwatch_torrent::validate::{check_component, check_path};

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
fn range_header_survives_mutations() {
    let mut rng = Rng(0x544f_5201);
    let seeds: [&[u8]; 4] = [
        b"bytes=0-1023",
        b"bytes=500-",
        b"bytes=-200",
        b"bytes=0-0,5-9",
    ];
    for i in 0..iters() {
        let m = mutate(&mut rng, seeds[i % seeds.len()]);
        let h = String::from_utf8_lossy(&m);
        let len = [0u64, 1, 1000, u64::MAX][rng.below(4)];
        if let Ok(Some((a, b))) = parse_range(Some(&h), len) {
            // Half-open range [a, b) inside the file, never empty.
            assert!(a < b && b <= len, "{h:?} len {len} gave {a}..{b}");
        }
    }
}

#[test]
fn torrent_paths_survive_mutations() {
    let mut rng = Rng(0x544f_5202);
    for _ in 0..iters() {
        let parts: Vec<String> = (0..=rng.below(3))
            .map(|_| String::from_utf8_lossy(&mutate(&mut rng, b"Season 1")).into_owned())
            .collect();
        if check_path(&parts).is_ok() {
            for p in &parts {
                assert!(check_component(p).is_ok());
                assert!(
                    !p.contains(['/', '\\', ':', '\0']) && p != ".." && p != ".",
                    "accepted {p:?}"
                );
            }
        }
    }
}

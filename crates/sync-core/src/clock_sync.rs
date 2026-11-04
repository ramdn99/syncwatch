//! NTP-style clock offset estimation (member side): median offset of the lowest-RTT samples.

use std::collections::VecDeque;

const WINDOW: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub offset_us: i64,
    pub rtt_us: i64,
}

/// Estimates `host_time - local_mono`.
#[derive(Debug, Default, Clone)]
pub struct ClockSync {
    samples: VecDeque<Sample>,
    /// Coarse fallback from one-way heartbeats until the first ping/pong completes.
    coarse_offset_us: Option<i64>,
}

impl ClockSync {
    /// t0/t3: local send/receive time; t1/t2: host receive/send time.
    pub fn add_pong(&mut self, t0: i64, t1: i64, t2: i64, t3: i64) {
        let rtt = (t3 - t0) - (t2 - t1);
        if !(0..=10_000_000).contains(&rtt) {
            return; // nonsense or > 10 s: discard
        }
        let offset = ((t1 - t0) + (t2 - t3)) / 2;
        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            offset_us: offset,
            rtt_us: rtt,
        });
    }

    /// One-way hint (ignores latency); only used before any pong arrived.
    pub fn add_coarse(&mut self, host_now: i64, local_now: i64) {
        if self.samples.is_empty() {
            self.coarse_offset_us = Some(host_now - local_now);
        }
    }

    pub fn has_estimate(&self) -> bool {
        !self.samples.is_empty() || self.coarse_offset_us.is_some()
    }

    pub fn is_precise(&self) -> bool {
        !self.samples.is_empty()
    }

    /// Median offset of the lowest-RTT third (at least one sample).
    pub fn offset_us(&self) -> Option<i64> {
        if self.samples.is_empty() {
            return self.coarse_offset_us;
        }
        let mut s: Vec<Sample> = self.samples.iter().copied().collect();
        s.sort_by_key(|x| x.rtt_us);
        let n = (s.len() / 3).max(1);
        let mut best: Vec<i64> = s[..n].iter().map(|x| x.offset_us).collect();
        best.sort_unstable();
        Some(best[best.len() / 2])
    }

    pub fn rtt_us(&self) -> Option<i64> {
        self.samples.iter().map(|s| s.rtt_us).min()
    }

    pub fn host_now(&self, local_mono: i64) -> Option<i64> {
        self.offset_us().map(|o| local_mono + o)
    }

    pub fn local_for_host(&self, host_time: i64) -> Option<i64> {
        self.offset_us().map(|o| host_time - o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_path_recovers_offset() {
        let mut c = ClockSync::default();
        // host = local + 5 s; 40 ms each way; host processing 1 ms.
        let off = 5_000_000;
        for i in 0..8 {
            let t0 = i * 1_000_000;
            let t1 = t0 + 40_000 + off;
            let t2 = t1 + 1_000;
            let t3 = t2 - off + 40_000;
            c.add_pong(t0, t1, t2, t3);
        }
        assert_eq!(c.offset_us(), Some(off));
        assert_eq!(c.rtt_us(), Some(80_000));
    }

    #[test]
    fn low_rtt_samples_win_over_congested_ones() {
        let mut c = ClockSync::default();
        let off = -2_000_000;
        // Good samples: 20 ms symmetric.
        for i in 0..4 {
            let t0 = i * 1_000_000;
            let t1 = t0 + 10_000 + off;
            c.add_pong(t0, t1, t1, t1 - off + 10_000);
        }
        // Bad samples: 600 ms asymmetric (all of it on the return path).
        for i in 4..16 {
            let t0 = i * 1_000_000;
            let t1 = t0 + 10_000 + off;
            c.add_pong(t0, t1, t1, t1 - off + 590_000);
        }
        let est = c.offset_us().unwrap();
        assert!((est - off).abs() < 1_000, "estimate {est}");
    }

    #[test]
    fn coarse_until_precise() {
        let mut c = ClockSync::default();
        assert!(!c.has_estimate());
        c.add_coarse(10_000, 1_000);
        assert_eq!(c.offset_us(), Some(9_000));
        assert!(!c.is_precise());
        c.add_pong(0, 100, 100, 200);
        assert!(c.is_precise());
    }
}

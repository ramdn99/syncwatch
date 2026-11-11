//! Pure piece policy (ADR 0009): which byte cursors the engine keeps open, whether the file is
//! playable, and whether playback must wait (Buffering). No I/O, no clock: the caller passes
//! `now` in microseconds and the file's piece map.
//!
//! librqbit downloads, first, the 32 MiB after every open stream's position (all streams
//! shuffled equally), then its own background order. So a "cursor" here is one open stream.
//! Strict priority for playback is built by keeping ONLY the lead cursor while the read-ahead
//! window is short ("exclusive"), and bringing the tail and fill cursors back once the window
//! is full and has stayed calm for a while (hysteresis against rapid seeks).

use std::collections::BTreeMap;

const MIB: u64 = 1024 * 1024;

/// Where the selected file sits in the torrent's pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileGeometry {
    pub len: u64,
    pub piece_len: u64,
    /// Bytes of the file's first piece that belong to earlier files.
    pub head_skew: u64,
}

impl FileGeometry {
    /// Geometry from the torrent layout: the file's absolute offset in the torrent.
    pub fn new(len: u64, piece_len: u64, abs_offset: u64) -> Self {
        let piece_len = piece_len.max(1);
        Self {
            len,
            piece_len,
            head_skew: abs_offset % piece_len,
        }
    }

    /// Number of pieces the file touches.
    pub fn pieces(&self) -> usize {
        if self.len == 0 {
            0
        } else {
            ((self.head_skew + self.len - 1) / self.piece_len + 1) as usize
        }
    }

    /// File-local piece index holding byte `b` (clamped into the file).
    pub fn piece_of(&self, b: u64) -> usize {
        ((b.min(self.len.saturating_sub(1)) + self.head_skew) / self.piece_len) as usize
    }

    /// First file byte of file-local piece `p`.
    pub fn piece_start(&self, p: usize) -> u64 {
        (p as u64 * self.piece_len)
            .saturating_sub(self.head_skew)
            .min(self.len)
    }

    /// File bytes `[a, b)` covered by piece `p`.
    fn piece_span(&self, p: usize) -> (u64, u64) {
        (self.piece_start(p), self.piece_start(p + 1).min(self.len))
    }

    /// Whether every piece touching bytes `[a, b)` is downloaded. Empty ranges are complete.
    pub fn complete(&self, have: &[bool], a: u64, b: u64) -> bool {
        let b = b.min(self.len);
        if a >= b {
            return true;
        }
        (self.piece_of(a)..=self.piece_of(b - 1)).all(|p| have.get(p).copied().unwrap_or(false))
    }

    /// Bytes of `[a, b)` that are downloaded.
    pub fn have_bytes(&self, have: &[bool], a: u64, b: u64) -> u64 {
        let b = b.min(self.len);
        if a >= b {
            return 0;
        }
        (self.piece_of(a)..=self.piece_of(b - 1))
            .filter(|p| have.get(*p).copied().unwrap_or(false))
            .map(|p| {
                let (s, e) = self.piece_span(p);
                e.min(b).saturating_sub(s.max(a))
            })
            .sum()
    }

    /// Downloaded byte ranges, merged (for the seek bar).
    pub fn ranges(&self, have: &[bool]) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        for p in 0..self.pieces() {
            if !have.get(p).copied().unwrap_or(false) {
                continue;
            }
            let (s, e) = self.piece_span(p);
            match out.last_mut() {
                Some(last) if last.1 >= s => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Read-ahead target after the playhead; while short, only the lead cursor runs.
    pub read_ahead: u64,
    /// What must be downloaded after the playhead to (re)start playback.
    pub prebuffer: u64,
    /// File head needed to open the file (container header).
    pub head: u64,
    /// File tail fetched early (MKV Cues, MP4 moov at the end).
    pub tail: u64,
    /// librqbit's fixed per-stream look-ahead (`PER_STREAM_BUF_DEFAULT`).
    pub stream_window: u64,
    /// Hysteresis: auxiliary cursors return only after the read-ahead window has been full
    /// and no seek happened for this long.
    pub calm_us: i64,
    /// Keep only the lead cursor while the window is short (ADR 0009). Off only in the
    /// comparison test that proves its effect.
    pub exclusive_playback: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            read_ahead: 16 * MIB,
            prebuffer: 4 * MIB,
            head: 2 * MIB,
            tail: 8 * MIB,
            stream_window: 32 * MIB,
            calm_us: 2_000_000,
            exclusive_playback: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CursorKind {
    /// Follows the playhead (also pre-targets a room seek before the player asks).
    Lead,
    /// The file head, only before the first playable moment if the playhead is elsewhere.
    Head,
    /// The file tail, until complete.
    Tail,
    /// Background fill: first missing piece after the lead's window, wrapping around.
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub kind: CursorKind,
    pub pos: u64,
}

/// Playback must wait: `missing` of `total` bytes after the playhead are not downloaded yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Buffering {
    pub target_end: u64,
    pub total: u64,
    pub have: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub cursors: Vec<Cursor>,
    pub playable: bool,
    pub buffering: Option<Buffering>,
    pub complete: bool,
    /// Only the lead cursor runs (playback window short or a seek just happened).
    pub exclusive: bool,
}

#[derive(Debug, Clone)]
pub struct PiecePolicy {
    cfg: PolicyConfig,
    geo: FileGeometry,
    playhead: u64,
    exclusive: bool,
    started: bool,
    last_seek_us: Option<i64>,
    satisfied_since: Option<i64>,
}

impl PiecePolicy {
    pub fn new(cfg: PolicyConfig, geo: FileGeometry) -> Self {
        Self {
            cfg,
            geo,
            playhead: 0,
            exclusive: true,
            started: false,
            last_seek_us: None,
            satisfied_since: None,
        }
    }

    pub fn geometry(&self) -> FileGeometry {
        self.geo
    }

    pub fn playhead(&self) -> u64 {
        self.playhead
    }

    /// The player read at `pos` (normal progress; no hysteresis reset).
    pub fn on_read(&mut self, pos: u64) {
        self.playhead = pos.min(self.geo.len);
    }

    /// A seek (room-wide or the player's own jump): re-target at once, playback first.
    pub fn on_seek(&mut self, pos: u64, now: i64) {
        self.playhead = pos.min(self.geo.len);
        self.last_seek_us = Some(now);
        self.satisfied_since = None;
        self.exclusive = true;
    }

    fn tail_start(&self) -> u64 {
        self.geo.len.saturating_sub(self.cfg.tail)
    }

    pub fn plan(&mut self, have: &[bool], now: i64) -> Plan {
        let g = self.geo;
        let len = g.len;
        let complete = g.complete(have, 0, len);
        let head_ok = g.complete(have, 0, self.cfg.head);
        let tail_ok = g.complete(have, self.tail_start(), len);
        let pre_end = self.playhead.saturating_add(self.cfg.prebuffer).min(len);
        let pre_ok = g.complete(have, self.playhead, pre_end);
        let ahead_ok = g.complete(
            have,
            self.playhead,
            self.playhead.saturating_add(self.cfg.read_ahead),
        );

        if ahead_ok {
            let since = *self.satisfied_since.get_or_insert(now);
            let calm_seek = self
                .last_seek_us
                .is_none_or(|t| now.saturating_sub(t) >= self.cfg.calm_us);
            if self.exclusive && now.saturating_sub(since) >= self.cfg.calm_us && calm_seek {
                self.exclusive = false;
            }
        } else {
            self.satisfied_since = None;
            self.exclusive = true;
        }

        if !self.cfg.exclusive_playback {
            self.exclusive = false;
        }
        let playable = complete || (head_ok && tail_ok && pre_ok);
        let mut cursors = Vec::new();
        if !complete {
            // The lead follows the playhead whenever something is missing.
            cursors.push(Cursor {
                kind: CursorKind::Lead,
                pos: self.playhead,
            });
            if !self.started {
                // Opening the file needs its head and tail before anything else (the lead is at
                // the playhead, which a late joiner may have moved away from 0).
                if !head_ok && self.playhead >= self.cfg.head {
                    cursors.push(Cursor {
                        kind: CursorKind::Head,
                        pos: 0,
                    });
                }
                if !tail_ok {
                    cursors.push(Cursor {
                        kind: CursorKind::Tail,
                        pos: self.tail_start(),
                    });
                }
            } else if !self.exclusive {
                if !tail_ok {
                    cursors.push(Cursor {
                        kind: CursorKind::Tail,
                        pos: self.tail_start(),
                    });
                }
                if let Some(pos) = self.fill_pos(have) {
                    cursors.push(Cursor {
                        kind: CursorKind::Fill,
                        pos,
                    });
                }
            }
        }
        self.started |= playable;
        let buffering = (self.started && !pre_ok && !complete).then(|| Buffering {
            target_end: pre_end,
            total: pre_end - self.playhead,
            have: g.have_bytes(have, self.playhead, pre_end),
        });
        Plan {
            cursors,
            playable,
            buffering,
            complete,
            exclusive: self.exclusive,
        }
    }

    /// Background fill: the first missing piece after the lead's look-ahead window, wrapping
    /// around to the file start; never inside the lead's window.
    fn fill_pos(&self, have: &[bool]) -> Option<u64> {
        let g = self.geo;
        let n = g.pieces();
        if n == 0 {
            return None;
        }
        let win_end = self.playhead.saturating_add(self.cfg.stream_window);
        let lead_first = g.piece_of(self.playhead);
        let lead_last = if win_end >= g.len {
            n - 1
        } else {
            g.piece_of(win_end)
        };
        let start = if win_end >= g.len { 0 } else { lead_last + 1 };
        (0..n)
            .map(|i| (start + i) % n)
            .find(|p| {
                let in_lead = *p >= lead_first && *p <= lead_last;
                !in_lead && !have.get(*p).copied().unwrap_or(false)
            })
            .map(|p| g.piece_start(p))
    }
}

/// Time -> byte mapping for pre-targeting a room seek (VBR-aware): learns (media ms, byte)
/// pairs from playback (the player's position and the loopback server's read offset) and
/// interpolates between them; falls back to `ms / duration x len`.
#[derive(Debug, Clone, Default)]
pub struct ByteMap {
    len: u64,
    duration_ms: Option<u64>,
    samples: BTreeMap<u64, u64>,
}

impl ByteMap {
    pub fn new(len: u64, duration_ms: Option<u64>) -> Self {
        Self {
            len,
            duration_ms,
            samples: BTreeMap::new(),
        }
    }

    pub fn set_duration(&mut self, d: Option<u64>) {
        self.duration_ms = d.filter(|d| *d > 0);
    }

    /// A playback observation. Non-monotonic samples (a byte offset lower than an earlier
    /// time's) are ignored: readers jump around (index reads).
    pub fn add_sample(&mut self, ms: u64, byte: u64) {
        let byte = byte.min(self.len);
        let below_ok = self
            .samples
            .range(..ms)
            .next_back()
            .is_none_or(|(_, b)| *b <= byte);
        let above_ok = self
            .samples
            .range(ms + 1..)
            .next()
            .is_none_or(|(_, b)| *b >= byte);
        if below_ok && above_ok {
            self.samples.insert(ms, byte);
        }
        if self.samples.len() > 4096
            && let Some(k) = self.samples.keys().nth(1).copied()
        {
            self.samples.remove(&k);
        }
    }

    fn proportional(&self, ms: u64) -> u64 {
        match self.duration_ms {
            Some(d) => ((ms.min(d) as u128 * self.len as u128) / d as u128) as u64,
            None => 0,
        }
    }

    pub fn byte_for(&self, ms: u64) -> u64 {
        let below = self.samples.range(..=ms).next_back();
        let above = self.samples.range(ms..).next();
        let b = match (below, above) {
            (Some((&t0, &b0)), Some((&t1, &b1))) if t1 > t0 => {
                b0 + ((b1 - b0) as u128 * (ms - t0) as u128 / (t1 - t0) as u128) as u64
            }
            (Some((_, &b0)), Some(_)) => b0,
            // Past the last sample: continue at the file's average rate.
            (Some((&t0, &b0)), None) => match self.duration_ms {
                Some(d) if d > 0 => b0 + ((ms - t0) as u128 * self.len as u128 / d as u128) as u64,
                _ => b0,
            },
            (None, Some((&t1, &b1))) if t1 > 0 => (b1 as u128 * ms as u128 / t1 as u128) as u64,
            _ => self.proportional(ms),
        };
        b.min(self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const PL: u64 = MIB; // 1 MiB pieces
    const LEN: u64 = 200 * MIB;

    fn geo() -> FileGeometry {
        FileGeometry::new(LEN, PL, 0)
    }

    fn cfg() -> PolicyConfig {
        PolicyConfig {
            read_ahead: 16 * MIB,
            prebuffer: 4 * MIB,
            head: 2 * MIB,
            tail: 8 * MIB,
            stream_window: 32 * MIB,
            calm_us: 2_000_000,
            exclusive_playback: true,
        }
    }

    fn have_ranges(ranges: &[(u64, u64)]) -> Vec<bool> {
        let g = geo();
        (0..g.pieces())
            .map(|p| {
                let s = g.piece_start(p);
                ranges.iter().any(|(a, b)| s >= *a && s < *b)
            })
            .collect()
    }

    fn kinds(p: &Plan) -> Vec<CursorKind> {
        p.cursors.iter().map(|c| c.kind).collect()
    }

    fn pos(p: &Plan, k: CursorKind) -> Option<u64> {
        p.cursors.iter().find(|c| c.kind == k).map(|c| c.pos)
    }

    #[test]
    fn startup_fetches_head_and_tail_first_then_is_playable() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let p = pol.plan(&have_ranges(&[]), 0);
        assert_eq!(kinds(&p), vec![CursorKind::Lead, CursorKind::Tail]);
        assert_eq!(
            pos(&p, CursorKind::Tail),
            Some(LEN - 8 * MIB),
            "MKV Cues / MP4 moov"
        );
        assert!(!p.playable && p.buffering.is_none());
        // Head and prebuffer done, tail not: still not playable (the container index is at the end).
        let p = pol.plan(&have_ranges(&[(0, 4 * MIB)]), 1);
        assert!(!p.playable);
        let p = pol.plan(&have_ranges(&[(0, 4 * MIB), (LEN - 8 * MIB, LEN)]), 2);
        assert!(p.playable);
    }

    #[test]
    fn a_late_joiner_at_the_middle_still_fetches_the_head() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.on_seek(100 * MIB, 0);
        let p = pol.plan(&have_ranges(&[]), 0);
        assert_eq!(
            kinds(&p),
            vec![CursorKind::Lead, CursorKind::Head, CursorKind::Tail]
        );
        assert_eq!(pos(&p, CursorKind::Lead), Some(100 * MIB));
    }

    #[test]
    fn seek_to_the_middle_is_exclusive_and_buffers_until_the_window_is_there() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let base = [(0, 40 * MIB), (LEN - 8 * MIB, LEN)];
        let p = pol.plan(&have_ranges(&base), 0);
        assert!(p.playable);
        pol.on_seek(100 * MIB, 10_000_000);
        let p = pol.plan(&have_ranges(&base), 10_000_000);
        assert_eq!(
            kinds(&p),
            vec![CursorKind::Lead],
            "only playback while short"
        );
        assert_eq!(pos(&p, CursorKind::Lead), Some(100 * MIB));
        let b = p.buffering.expect("buffering");
        assert_eq!((b.target_end, b.total, b.have), (104 * MIB, 4 * MIB, 0));
        // Prebuffer arrives: playback resumes (no buffering) but stays exclusive until the
        // read-ahead window is full.
        let mid = [(0, 40 * MIB), (100 * MIB, 104 * MIB), (LEN - 8 * MIB, LEN)];
        let p = pol.plan(&have_ranges(&mid), 11_000_000);
        assert!(p.buffering.is_none() && p.exclusive);
        assert_eq!(kinds(&p), vec![CursorKind::Lead]);
        // Window full, but not yet calm: still exclusive (hysteresis).
        let full = [(0, 40 * MIB), (100 * MIB, 120 * MIB), (LEN - 8 * MIB, LEN)];
        let p = pol.plan(&have_ranges(&full), 11_500_000);
        assert!(p.exclusive);
        let p = pol.plan(&have_ranges(&full), 13_600_000);
        assert!(!p.exclusive);
        assert_eq!(kinds(&p), vec![CursorKind::Lead, CursorKind::Fill]);
    }

    #[test]
    fn rapid_repeated_seeks_keep_one_cursor_on_the_latest_target() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let base = [(0, 60 * MIB), (LEN - 8 * MIB, LEN)];
        pol.plan(&have_ranges(&base), 0);
        let mut now = 5_000_000;
        for target in [10, 150, 70, 120, 30, 90] {
            pol.on_seek(target * MIB, now);
            let p = pol.plan(&have_ranges(&base), now + 100_000);
            if target < 44 {
                // Downloaded: no buffering, but the aux cursors stay away (calm not reached).
                assert!(
                    p.buffering.is_none(),
                    "seek back to {target} MiB is instant"
                );
            } else {
                assert!(p.buffering.is_some());
            }
            assert_eq!(kinds(&p), vec![CursorKind::Lead], "seek to {target} MiB");
            assert_eq!(pos(&p, CursorKind::Lead), Some(target * MIB));
            now += 300_000;
        }
    }

    #[test]
    fn seek_back_to_downloaded_data_is_instant() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let have = have_ranges(&[(0, 80 * MIB), (LEN - 8 * MIB, LEN)]);
        pol.plan(&have, 0);
        pol.on_seek(60 * MIB, 1_000_000);
        let p = pol.plan(&have, 1_000_000);
        assert!(p.playable && p.buffering.is_none());
        pol.on_seek(5 * MIB, 1_200_000);
        let p = pol.plan(&have, 1_200_000);
        assert!(p.playable && p.buffering.is_none());
    }

    #[test]
    fn background_fill_starts_after_the_lead_window_and_wraps_around() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        // Played to 150 MiB; [150, 200) is downloaded (incl. tail), the start has a hole
        // after the head.
        let have = have_ranges(&[(0, 2 * MIB), (20 * MIB, 50 * MIB), (150 * MIB, LEN)]);
        pol.on_seek(150 * MIB, 0);
        pol.plan(&have, 0);
        let p = pol.plan(&have, 5_000_000);
        assert!(!p.exclusive);
        // Nothing missing after the playhead: wrap to the first hole at the start.
        assert_eq!(pos(&p, CursorKind::Fill), Some(2 * MIB));
        // A hole after the lead window comes first.
        let have = have_ranges(&[(0, 50 * MIB), (150 * MIB, 190 * MIB), (195 * MIB, LEN)]);
        pol.on_read(150 * MIB);
        let p = pol.plan(&have, 6_000_000);
        assert_eq!(pos(&p, CursorKind::Fill), Some(190 * MIB));
        // Then the rest from the start after wrapping.
        let have = have_ranges(&[(0, 50 * MIB), (150 * MIB, LEN)]);
        let p = pol.plan(&have, 7_000_000);
        assert_eq!(pos(&p, CursorKind::Fill), Some(50 * MIB));
    }

    #[test]
    fn complete_file_needs_no_cursors() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let p = pol.plan(&have_ranges(&[(0, LEN)]), 0);
        assert!(p.complete && p.playable && p.cursors.is_empty() && p.buffering.is_none());
    }

    #[test]
    fn geometry_with_a_skewed_file() {
        // File starts 300 KiB into its first piece.
        let g = FileGeometry::new(5 * MIB, MIB, 7 * MIB + 300 * 1024);
        assert_eq!(g.pieces(), 6);
        assert_eq!(g.piece_of(0), 0);
        assert_eq!(g.piece_of(MIB - 300 * 1024), 1);
        assert_eq!(g.piece_start(1), MIB - 300 * 1024);
        let have = [true, false, true, true, true, true];
        assert_eq!(
            g.ranges(&have),
            vec![(0, MIB - 300 * 1024), (2 * MIB - 300 * 1024, 5 * MIB)]
        );
        assert!(!g.complete(&have, 0, 5 * MIB));
        assert!(g.complete(&have, 2 * MIB, 5 * MIB));
    }

    #[test]
    fn byte_map_is_vbr_aware_with_a_proportional_fallback() {
        let mut m = ByteMap::new(1000, Some(100_000));
        assert_eq!(m.byte_for(50_000), 500, "fallback: proportional");
        // A dense first half (VBR): 10 s -> 300 bytes.
        m.add_sample(0, 0);
        m.add_sample(10_000, 300);
        assert_eq!(m.byte_for(5_000), 150);
        // Past the last sample: average rate from there.
        assert_eq!(m.byte_for(20_000), 400);
        // A non-monotonic sample (index read) is ignored.
        m.add_sample(15_000, 10);
        assert_eq!(m.byte_for(20_000), 400);
        assert_eq!(m.byte_for(10_000_000), 1000, "clamped");
    }

    proptest! {
        #[test]
        fn cursors_stay_inside_the_file_and_are_few(
            seeks in proptest::collection::vec((0u64..LEN, 0i64..10_000_000), 1..20),
            holes in proptest::collection::vec((0u64..200, 1u64..30), 0..8),
        ) {
            let g = geo();
            let mut have = vec![true; g.pieces()];
            for (s, l) in holes {
                for p in s..(s + l).min(200) {
                    have[p as usize] = false;
                }
            }
            let mut pol = PiecePolicy::new(cfg(), g);
            let mut now = 0;
            for (pos, dt) in seeks {
                now += dt;
                pol.on_seek(pos, now);
                let p = pol.plan(&have, now);
                prop_assert!(p.cursors.len() <= 4);
                for c in &p.cursors {
                    prop_assert!(c.pos <= LEN);
                }
                if let Some(f) = pos_of(&p, CursorKind::Fill) {
                    prop_assert!(!have[g.piece_of(f)], "fill targets a missing piece");
                }
            }
        }
    }

    fn pos_of(p: &Plan, k: CursorKind) -> Option<u64> {
        pos(p, k)
    }
}

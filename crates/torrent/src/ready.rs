//! Readiness at the ROOM position (ADR 0012): a participant is ready when the data it needs
//! NOW is on its disk: the room position through a read-ahead window, plus the container pieces
//! needed to play there (file head and tail). Overall progress ("downloading 40%") never
//! matters. Pure: the engine feeds it the have-bitfield and the room position.
//!
//! Hysteresis: a participant becomes ready only when the FULL window is there, and stops being
//! ready only when even the short KEEP window is missing. Data arriving at about the playback
//! rate therefore keeps it ready instead of flapping.

use crate::policy::{ByteMap, FileGeometry};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyConfig {
    /// Media time after the room position that must be on disk to become ready.
    pub window_ms: u64,
    /// Smaller window: losing it ends "ready" (hysteresis).
    pub keep_ms: u64,
    /// Media time before the position also needed (the demuxer starts at the keyframe before).
    pub back_ms: u64,
    /// Byte bounds of the full window (a few MB whatever the bitrate).
    pub min_bytes: u64,
    pub max_bytes: u64,
    /// Lower byte bound of the keep window.
    pub keep_min_bytes: u64,
}

impl Default for ReadyConfig {
    fn default() -> Self {
        Self {
            window_ms: 5_000,
            keep_ms: 1_500,
            back_ms: 1_000,
            min_bytes: 2 * MIB,
            max_bytes: 24 * MIB,
            keep_min_bytes: 512 * 1024,
        }
    }
}

/// The byte window [start, end) for media time `at_ms` .. `at_ms + ms`; `None` while the
/// time -> byte mapping is unknown (no duration and no samples yet).
pub fn window(
    map: &ByteMap,
    len: u64,
    at_ms: u64,
    ms: u64,
    back_ms: u64,
    min_bytes: u64,
    max_bytes: u64,
) -> Option<(u64, u64)> {
    if !map.known() || len == 0 {
        return None;
    }
    let start = map.byte_for(at_ms.saturating_sub(back_ms)).min(len);
    let mapped = map.byte_for(at_ms.saturating_add(ms));
    let lo = start.saturating_add(min_bytes);
    let hi = start.saturating_add(max_bytes.max(min_bytes));
    let end = mapped.clamp(lo, hi).min(len);
    Some((start, end.max(start)))
}

/// What is on disk of the window plus the container pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub have: u64,
    pub total: u64,
    pub head_ok: bool,
    pub tail_ok: bool,
}

impl Coverage {
    pub fn complete(&self) -> bool {
        self.head_ok && self.tail_ok && self.have >= self.total
    }
    pub fn missing(&self) -> u64 {
        self.total.saturating_sub(self.have)
    }
    pub fn permille(&self) -> u16 {
        (self.have.min(self.total) * 1000)
            .checked_div(self.total)
            .unwrap_or(1000) as u16
    }
}

/// Coverage of the window [start, end) plus the first `head` and last `tail` bytes, counted
/// over the union of the three ranges (overlaps counted once).
pub fn coverage(
    geo: &FileGeometry,
    have: &[bool],
    (start, end): (u64, u64),
    head: u64,
    tail: u64,
) -> Coverage {
    let len = geo.len;
    let head_r = (0, head.min(len));
    let tail_r = (len.saturating_sub(tail), len);
    let mut rs = vec![head_r, tail_r, (start.min(len), end.min(len))];
    rs.retain(|(a, b)| b > a);
    rs.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (a, b) in rs {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let (mut have_b, mut total) = (0, 0);
    for (a, b) in merged {
        total += b - a;
        have_b += geo.have_bytes(have, a, b);
    }
    Coverage {
        have: have_b,
        total,
        head_ok: geo.complete(have, head_r.0, head_r.1),
        tail_ok: geo.complete(have, tail_r.0, tail_r.1),
    }
}

/// The readiness report sent to sync-core with every status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// The room position the window starts at.
    pub at_ms: u64,
    /// Missing bytes of the full window (head and tail included).
    pub missing: u64,
    pub permille: u16,
}

/// Ready/buffering with hysteresis (enter on the full window, leave on the keep window).
#[derive(Debug, Clone, Default)]
pub struct ReadyTracker {
    ready: bool,
}

impl ReadyTracker {
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// A room seek, media change or resume: the window moved, judge it afresh (a member whose
    /// new window is already there is ready at once).
    pub fn reset(&mut self) {
        self.ready = false;
    }

    pub fn update(&mut self, at_ms: u64, full: Coverage, keep: Coverage) -> Readiness {
        self.ready = if self.ready {
            keep.complete() || full.complete()
        } else {
            full.complete()
        };
        Readiness {
            ready: self.ready,
            at_ms,
            missing: full.missing(),
            permille: full.permille(),
        }
    }
}

/// Everything at once: window bytes from the room position, coverage and the tracker.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    cfg: &ReadyConfig,
    tracker: &mut ReadyTracker,
    map: &ByteMap,
    geo: &FileGeometry,
    have: &[bool],
    at_ms: u64,
    head: u64,
    tail: u64,
) -> Option<Readiness> {
    let len = geo.len;
    let full_w = window(
        map,
        len,
        at_ms,
        cfg.window_ms,
        cfg.back_ms,
        cfg.min_bytes,
        cfg.max_bytes,
    )?;
    let keep_w = window(
        map,
        len,
        at_ms,
        cfg.keep_ms,
        cfg.back_ms,
        cfg.keep_min_bytes,
        cfg.max_bytes,
    )?;
    let full = coverage(geo, have, full_w, head, tail);
    let keep = coverage(geo, have, keep_w, head, tail);
    Some(tracker.update(at_ms, full, keep))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: u64 = 200 * MIB;
    const HEAD: u64 = 2 * MIB;
    const TAIL: u64 = 8 * MIB;
    /// 200 MiB over 200 s: 1 MiB per second (proportional mapping).
    const DUR: u64 = 200_000;

    fn geo() -> FileGeometry {
        FileGeometry::new(LEN, MIB, 0)
    }
    fn have(ranges: &[(u64, u64)]) -> Vec<bool> {
        let g = geo();
        (0..g.pieces())
            .map(|p| {
                let s = g.piece_start(p);
                ranges.iter().any(|(a, b)| s >= *a && s < *b)
            })
            .collect()
    }
    fn map() -> ByteMap {
        ByteMap::new(LEN, Some(DUR))
    }
    fn eval(t: &mut ReadyTracker, h: &[bool], at_ms: u64) -> Option<Readiness> {
        evaluate(
            &ReadyConfig::default(),
            t,
            &map(),
            &geo(),
            h,
            at_ms,
            HEAD,
            TAIL,
        )
    }
    const CONTAINER: [(u64, u64); 2] = [(0, HEAD), (LEN - TAIL, LEN)];

    #[test]
    fn covered_window_is_ready_even_at_low_overall_progress() {
        // ~13% overall: head, tail and 100..110 s (100..110 MiB).
        let mut rs = CONTAINER.to_vec();
        rs.push((99 * MIB, 110 * MIB));
        let h = have(&rs);
        let r = eval(&mut ReadyTracker::default(), &h, 100_000).unwrap();
        assert!(r.ready, "{r:?}");
        assert_eq!(r.missing, 0);
        assert_eq!(r.permille, 1000);
    }

    #[test]
    fn partly_covered_window_is_buffering_with_the_missing_size() {
        // Window 99..105 MiB (1 s back, 5 s ahead); 99..102 there, 102..105 missing.
        let mut rs = CONTAINER.to_vec();
        rs.push((99 * MIB, 102 * MIB));
        let h = have(&rs);
        let r = eval(&mut ReadyTracker::default(), &h, 100_000).unwrap();
        assert!(!r.ready);
        assert_eq!(r.at_ms, 100_000);
        assert_eq!(r.missing, 3 * MIB);
        // 2 + 8 + 3 of 2 + 8 + 6 MiB.
        assert_eq!(r.permille, (13 * 1000 / 16) as u16);
    }

    #[test]
    fn missing_head_or_tail_is_not_ready() {
        let win = (99 * MIB, 110 * MIB);
        for rs in [vec![(0, HEAD), win], vec![(LEN - TAIL, LEN), win]] {
            let h = have(&rs);
            let r = eval(&mut ReadyTracker::default(), &h, 100_000).unwrap();
            assert!(!r.ready, "{rs:?}");
            assert!(r.missing > 0);
        }
        // The whole file is never required: everything else may be missing.
        let mut rs = CONTAINER.to_vec();
        rs.push(win);
        assert!(
            eval(&mut ReadyTracker::default(), &have(&rs), 100_000)
                .unwrap()
                .ready
        );
    }

    #[test]
    fn hysteresis_keeps_ready_while_data_arrives_at_playback_rate() {
        let mut t = ReadyTracker::default();
        // Ready with the full window (5 s ahead).
        let mut rs = CONTAINER.to_vec();
        rs.push((99 * MIB, 105 * MIB));
        assert!(eval(&mut t, &have(&rs), 100_000).unwrap().ready);
        // Playback advanced 3 s, download only 1 s: 2 s left ahead, still >= keep (1.5 s).
        rs.push((105 * MIB, 106 * MIB));
        let r = eval(&mut t, &have(&rs), 103_000).unwrap();
        assert!(r.ready, "keeps ready inside the keep window: {r:?}");
        // A fresh tracker with the same data is NOT ready (it needs the full window first).
        assert!(
            !eval(&mut ReadyTracker::default(), &have(&rs), 103_000)
                .unwrap()
                .ready
        );
        // Keep window lost (position 105.5 s, data to 106 MiB): buffering.
        let r = eval(&mut t, &have(&rs), 105_500).unwrap();
        assert!(!r.ready);
        // Data back to 107 MiB: keep window there again but not the full window: still
        // buffering (no flapping back and forth).
        rs.push((106 * MIB, 107 * MIB));
        assert!(!eval(&mut t, &have(&rs), 105_500).unwrap().ready);
        rs.push((107 * MIB, 111 * MIB));
        assert!(eval(&mut t, &have(&rs), 105_500).unwrap().ready);
    }

    #[test]
    fn byte_mapping_is_vbr_aware_and_falls_back_to_proportional() {
        // No duration and no samples: unknown, never a guess at byte 0.
        let none = ByteMap::new(LEN, None);
        assert_eq!(
            window(&none, LEN, 100_000, 5_000, 1_000, MIB, 24 * MIB),
            None
        );
        // Proportional fallback.
        let w = window(&map(), LEN, 100_000, 5_000, 1_000, MIB, 24 * MIB).unwrap();
        assert_eq!(w, (99 * MIB, 105 * MIB));
        // VBR samples: 100 s sits at 150 MiB, 110 s at 160 MiB.
        let mut m = map();
        m.add_sample(100_000, 150 * MIB);
        m.add_sample(110_000, 160 * MIB);
        let (a, b) = window(&m, LEN, 101_000, 5_000, 1_000, MIB, 24 * MIB).unwrap();
        assert_eq!((a, b), (150 * MIB, 156 * MIB));
        // Byte bounds: a very low bitrate still asks for min_bytes, a very high one at most
        // max_bytes; never past the end of the file.
        let (a, b) = window(&m, LEN, 101_000, 5_000, 1_000, 8 * MIB, 24 * MIB).unwrap();
        assert_eq!(b - a, 8 * MIB);
        let (a, b) = window(&map(), LEN, 199_000, 5_000, 1_000, 8 * MIB, 24 * MIB).unwrap();
        assert_eq!((a, b), (198 * MIB, LEN));
    }

    #[test]
    fn complete_file_is_always_ready() {
        let h = vec![true; geo().pieces()];
        for at in [0, 50_000, 199_999, 400_000] {
            assert!(eval(&mut ReadyTracker::default(), &h, at).unwrap().ready);
        }
    }
}

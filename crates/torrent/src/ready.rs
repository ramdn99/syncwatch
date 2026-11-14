//! Readiness at the ROOM position (ADR 0012): a participant is ready when the data it needs
//! NOW is on its disk: the room position through a read-ahead window, plus the container ranges
//! its player actually requested for this file (header, MKV Cues, MP4 moov). Before the player
//! opened the file, the start-up range stands in for those. Nothing is assumed: no "file head
//! and tail" requirement, and overall progress ("downloading 40%") never matters. Pure: the
//! engine feeds it the have-bitfield, the room position and the recorded ranges.
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
    /// Before the player opened the file: the start-up range [0, startup_bytes) it needs to
    /// open it (fetched on demand when the player asks, never pre-fetched).
    pub startup_bytes: u64,
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
            // Measured (ADR 0012): mpv read at most 512 KiB at the start to open MKV (Cues at the
            // end or the start) and MP4 (moov at the start); 1 MiB leaves room.
            startup_bytes: MIB,
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

/// What is on disk of the window plus the required container ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub have: u64,
    pub total: u64,
}

impl Coverage {
    pub fn complete(&self) -> bool {
        self.have >= self.total
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

/// Merges ranges (sorted, overlaps and touching ranges joined, empty ones dropped).
pub fn merge(mut rs: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    rs.retain(|(a, b)| b > a);
    rs.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (a, b) in rs {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// Coverage of the window [start, end) plus `extra` ranges, counted over their union.
pub fn coverage(
    geo: &FileGeometry,
    have: &[bool],
    (start, end): (u64, u64),
    extra: &[(u64, u64)],
) -> Coverage {
    let len = geo.len;
    let mut rs = vec![(start.min(len), end.min(len))];
    rs.extend(extra.iter().map(|(a, b)| ((*a).min(len), (*b).min(len))));
    let (mut have_b, mut total) = (0, 0);
    for (a, b) in merge(rs) {
        total += b - a;
        have_b += geo.have_bytes(have, a, b);
    }
    Coverage {
        have: have_b,
        total,
    }
}

/// The readiness report sent to sync-core with every status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// The room position the window starts at.
    pub at_ms: u64,
    /// Missing bytes of the full window (requested container ranges included).
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
/// `container`: the ranges this file's player requested outside playback (recorded by the
/// engine). `opened`: the player has opened the file; before that, the start-up range counts.
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    cfg: &ReadyConfig,
    tracker: &mut ReadyTracker,
    map: &ByteMap,
    geo: &FileGeometry,
    have: &[bool],
    at_ms: u64,
    container: &[(u64, u64)],
    opened: bool,
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
    let mut extra = container.to_vec();
    if !opened {
        extra.push((0, cfg.startup_bytes.min(len)));
    }
    let full = coverage(geo, have, full_w, &extra);
    let keep = coverage(geo, have, keep_w, &extra);
    Some(tracker.update(at_ms, full, keep))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEN: u64 = 200 * MIB;
    /// 200 MiB over 200 s: 1 MiB per second (proportional mapping).
    const DUR: u64 = 200_000;
    const CUES: (u64, u64) = (LEN - 300 * 1024, LEN);
    const HEADER: (u64, u64) = (0, 40 * 1024);

    fn geo() -> FileGeometry {
        FileGeometry::new(LEN, MIB / 4, 0)
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
    fn eval(
        t: &mut ReadyTracker,
        h: &[bool],
        at_ms: u64,
        container: &[(u64, u64)],
        opened: bool,
    ) -> Option<Readiness> {
        evaluate(
            &ReadyConfig::default(),
            t,
            &map(),
            &geo(),
            h,
            at_ms,
            container,
            opened,
        )
    }
    /// The header and Cues as on disk (pieces of 256 KiB around them).
    const CONTAINER_ON_DISK: [(u64, u64); 2] = [(0, MIB / 4), (LEN - MIB / 2, LEN)];

    #[test]
    fn covered_window_is_ready_even_at_low_overall_progress() {
        // ~6% overall: the window 99..110 MiB plus what the player read.
        let mut rs = CONTAINER_ON_DISK.to_vec();
        rs.push((99 * MIB, 110 * MIB));
        let r = eval(
            &mut ReadyTracker::default(),
            &have(&rs),
            100_000,
            &[HEADER, CUES],
            true,
        )
        .unwrap();
        assert!(r.ready, "{r:?}");
        assert_eq!((r.missing, r.permille), (0, 1000));
    }

    #[test]
    fn no_head_and_tail_requirement_only_what_the_player_asked_for() {
        // Only the window on disk; the player never asked for anything at the end.
        let h = have(&[(99 * MIB, 106 * MIB)]);
        assert!(
            eval(&mut ReadyTracker::default(), &h, 100_000, &[], true)
                .unwrap()
                .ready
        );
        // The player did read the Cues at the end: they count, and here they are missing.
        let r = eval(&mut ReadyTracker::default(), &h, 100_000, &[CUES], true).unwrap();
        assert!(!r.ready);
        assert_eq!(r.missing, 300 * 1024);
        // Once on disk: ready.
        let h = have(&[(99 * MIB, 106 * MIB), (LEN - MIB / 2, LEN)]);
        assert!(
            eval(&mut ReadyTracker::default(), &h, 100_000, &[CUES], true)
                .unwrap()
                .ready
        );
    }

    #[test]
    fn a_member_that_has_not_opened_the_file_needs_only_the_start_up_range() {
        let win = (99 * MIB, 106 * MIB);
        // Window there, start-up range not yet: not ready, missing exactly the start-up range.
        let r = eval(
            &mut ReadyTracker::default(),
            &have(&[win]),
            100_000,
            &[],
            false,
        )
        .unwrap();
        assert!(!r.ready);
        assert_eq!(r.missing, ReadyConfig::default().startup_bytes);
        // The start-up range (1 MiB) arrives: ready, with the file's end still missing.
        let r = eval(
            &mut ReadyTracker::default(),
            &have(&[win, (0, MIB)]),
            100_000,
            &[],
            false,
        )
        .unwrap();
        assert!(r.ready);
    }

    #[test]
    fn partly_covered_window_is_buffering_with_the_missing_size() {
        // Window 99..105 MiB (1 s back, 5 s ahead); 99..102 there, 102..105 missing.
        let r = eval(
            &mut ReadyTracker::default(),
            &have(&[(99 * MIB, 102 * MIB)]),
            100_000,
            &[],
            true,
        )
        .unwrap();
        assert!(!r.ready);
        assert_eq!(r.at_ms, 100_000);
        assert_eq!(r.missing, 3 * MIB);
        assert_eq!(r.permille, 500);
    }

    #[test]
    fn hysteresis_keeps_ready_while_data_arrives_at_playback_rate() {
        let mut t = ReadyTracker::default();
        let mut rs = vec![(99 * MIB, 105 * MIB)];
        assert!(eval(&mut t, &have(&rs), 100_000, &[], true).unwrap().ready);
        // Playback advanced 3 s, download only 1 s: 2 s left ahead, still >= keep (1.5 s).
        rs.push((105 * MIB, 106 * MIB));
        let r = eval(&mut t, &have(&rs), 103_000, &[], true).unwrap();
        assert!(r.ready, "keeps ready inside the keep window: {r:?}");
        // A fresh tracker with the same data is NOT ready (it needs the full window first).
        assert!(
            !eval(&mut ReadyTracker::default(), &have(&rs), 103_000, &[], true)
                .unwrap()
                .ready
        );
        // Keep window lost: buffering; the keep window back alone does not flap to ready.
        assert!(!eval(&mut t, &have(&rs), 105_500, &[], true).unwrap().ready);
        rs.push((106 * MIB, 107 * MIB));
        assert!(!eval(&mut t, &have(&rs), 105_500, &[], true).unwrap().ready);
        rs.push((107 * MIB, 111 * MIB));
        assert!(eval(&mut t, &have(&rs), 105_500, &[], true).unwrap().ready);
    }

    #[test]
    fn byte_mapping_is_vbr_aware_and_falls_back_to_proportional() {
        // No duration and no samples: unknown, never a guess at byte 0.
        let none = ByteMap::new(LEN, None);
        assert_eq!(
            window(&none, LEN, 100_000, 5_000, 1_000, MIB, 24 * MIB),
            None
        );
        assert_eq!(
            evaluate(
                &ReadyConfig::default(),
                &mut ReadyTracker::default(),
                &none,
                &geo(),
                &have(&[]),
                100_000,
                &[],
                true
            ),
            None
        );
        let w = window(&map(), LEN, 100_000, 5_000, 1_000, MIB, 24 * MIB).unwrap();
        assert_eq!(w, (99 * MIB, 105 * MIB));
        let mut m = map();
        m.add_sample(100_000, 150 * MIB);
        m.add_sample(110_000, 160 * MIB);
        let (a, b) = window(&m, LEN, 101_000, 5_000, 1_000, MIB, 24 * MIB).unwrap();
        assert_eq!((a, b), (150 * MIB, 156 * MIB));
        let (a, b) = window(&m, LEN, 101_000, 5_000, 1_000, 8 * MIB, 24 * MIB).unwrap();
        assert_eq!(b - a, 8 * MIB);
        let (a, b) = window(&map(), LEN, 199_000, 5_000, 1_000, 8 * MIB, 24 * MIB).unwrap();
        assert_eq!((a, b), (198 * MIB, LEN));
    }

    #[test]
    fn complete_file_is_always_ready() {
        let h = vec![true; geo().pieces()];
        for at in [0, 50_000, 199_999, 400_000] {
            assert!(
                eval(&mut ReadyTracker::default(), &h, at, &[CUES], false)
                    .unwrap()
                    .ready
            );
        }
    }
}

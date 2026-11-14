//! Pure piece policy (ADRs 0009, 0012): which byte cursors the engine keeps open. No I/O, no
//! clock: the caller passes `now` in microseconds and the file's piece map.
//!
//! librqbit downloads, first, the 32 MiB after every open stream's position (all streams
//! shuffled and interleaved equally), then its own background order. So a "cursor" here is one
//! open stream. The order (ADR 0012): the ROOM's playing window and whatever the player is
//! blocked on, alone; then the player's read-ahead; then, once that has been full and calm for a
//! while (hysteresis against rapid seeks), the rest of the file from the playhead, wrapping
//! around. Nothing is pre-fetched blindly (no head, no tail).

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
    /// Read-ahead after the playhead; while it is short, only the lead cursor runs.
    pub read_ahead: u64,
    /// librqbit's fixed per-stream look-ahead (`PER_STREAM_BUF_DEFAULT`).
    pub stream_window: u64,
    /// Hysteresis: the background fill returns only after the read-ahead window has been full
    /// and no seek happened for this long.
    pub calm_us: i64,
    /// Keep only the urgent cursors (room window, player demand) or the lead while short
    /// (ADR 0009). Off only in the comparison test that proves its effect.
    pub exclusive_playback: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            read_ahead: 16 * MIB,
            stream_window: 32 * MIB,
            calm_us: 2_000_000,
            exclusive_playback: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CursorKind {
    /// The ROOM's playing part (ADR 0012): while its window is not on disk it runs first, with
    /// only the player's demand beside it; also before this player opened the file or while
    /// it waits at the gate.
    Room,
    /// The player is blocked on a range outside the room window (container header, MKV Cues,
    /// MP4 moov): fetched on demand, at once, and only while the player waits for it.
    Demand,
    /// The player's read-ahead (once the room window is there).
    Lead,
    /// Background fill: first missing piece after the lead's window, wrapping around.
    Fill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub kind: CursorKind,
    pub pos: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub cursors: Vec<Cursor>,
    pub complete: bool,
    /// Only the urgent cursors (or the lead) run: window short or a seek just happened.
    pub exclusive: bool,
}

/// No blind pre-fetch (ADR 0012, revised): nothing is fetched because "files usually need it".
/// The order is the room window and whatever the player is blocked on, then the player's
/// read-ahead, then the rest of the file from the playhead, wrapping around.
#[derive(Debug, Clone)]
pub struct PiecePolicy {
    cfg: PolicyConfig,
    geo: FileGeometry,
    /// Where the player reads; `None` until it read anything (then the room position stands in).
    playhead: Option<u64>,
    exclusive: bool,
    last_seek_us: Option<i64>,
    satisfied_since: Option<i64>,
    /// The room's read window [start, end) in bytes (extrapolated room position).
    room: Option<(u64, u64)>,
    /// The player's read is blocked at this byte (an open request waiting for data).
    demand: Option<u64>,
}

impl PiecePolicy {
    pub fn new(cfg: PolicyConfig, geo: FileGeometry) -> Self {
        Self {
            cfg,
            geo,
            playhead: None,
            exclusive: true,
            last_seek_us: None,
            satisfied_since: None,
            room: None,
            demand: None,
        }
    }

    pub fn geometry(&self) -> FileGeometry {
        self.geo
    }

    /// Where the player reads, else the room window's start, else 0.
    pub fn playhead(&self) -> u64 {
        self.lead_pos().unwrap_or(0)
    }

    fn lead_pos(&self) -> Option<u64> {
        self.playhead.or(self.room.map(|r| r.0))
    }

    /// The player read at `pos` (normal progress; no hysteresis reset).
    pub fn on_read(&mut self, pos: u64) {
        self.playhead = Some(pos.min(self.geo.len));
    }

    /// A seek (the player's own jump): re-target at once, playback first.
    pub fn on_seek(&mut self, pos: u64, now: i64) {
        self.playhead = Some(pos.min(self.geo.len));
        self.last_seek_us = Some(now);
        self.satisfied_since = None;
        self.exclusive = true;
    }

    /// The player's open request is blocked at `pos` (`None`: no request waiting).
    pub fn set_demand(&mut self, pos: Option<u64>) {
        self.demand = pos.map(|p| p.min(self.geo.len));
    }

    pub fn room(&self) -> Option<(u64, u64)> {
        self.room
    }

    /// The room position moved with playback (no re-target).
    pub fn set_room(&mut self, start: u64, end: u64) {
        let len = self.geo.len;
        self.room = Some((start.min(len), end.min(len).max(start.min(len))));
    }

    /// A room seek, media change, pause or resume: re-target at once. The old window drops to
    /// normal priority; the new one goes first.
    pub fn room_seek(&mut self, start: u64, end: u64, now: i64) {
        self.set_room(start, end);
        self.last_seek_us = Some(now);
        self.satisfied_since = None;
        self.exclusive = true;
    }

    fn in_room(&self, b: u64) -> bool {
        self.room.is_some_and(|(a, e)| b >= a && b < e)
    }

    /// First byte of the room window that is not downloaded yet.
    fn room_missing(&self, have: &[bool]) -> Option<u64> {
        let (a, b) = self.room?;
        let g = self.geo;
        if b <= a || g.complete(have, a, b) {
            return None;
        }
        let (first, last) = (g.piece_of(a), g.piece_of(b - 1));
        (first..=last)
            .find(|p| !have.get(*p).copied().unwrap_or(false))
            .map(|p| g.piece_start(p).max(a))
    }

    /// The player waits for a byte outside the room window that is not on disk.
    fn demand_missing(&self, have: &[bool]) -> Option<u64> {
        let p = self.demand?;
        let g = self.geo;
        (p < g.len && !self.in_room(p) && !have.get(g.piece_of(p)).copied().unwrap_or(false))
            .then_some(p)
    }

    pub fn plan(&mut self, have: &[bool], now: i64) -> Plan {
        let g = self.geo;
        let len = g.len;
        let complete = g.complete(have, 0, len);
        let room_missing = self.room_missing(have);
        let demand = self.demand_missing(have);
        let urgent = room_missing.is_some() || demand.is_some();
        let lead = self.lead_pos();
        let ahead_ok =
            lead.is_none_or(|l| g.complete(have, l, l.saturating_add(self.cfg.read_ahead)));
        if !urgent && ahead_ok {
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
        let mut cursors = Vec::new();
        if !complete {
            if let Some(pos) = room_missing {
                cursors.push(Cursor {
                    kind: CursorKind::Room,
                    pos,
                });
            }
            if let Some(pos) = demand {
                cursors.push(Cursor {
                    kind: CursorKind::Demand,
                    pos,
                });
            }
            // Nothing else ahead of the room window and the player's demand.
            if !urgent || !self.cfg.exclusive_playback {
                if let Some(pos) = lead {
                    cursors.push(Cursor {
                        kind: CursorKind::Lead,
                        pos,
                    });
                }
                if !self.exclusive
                    && let Some(pos) = self.fill_pos(have)
                {
                    cursors.push(Cursor {
                        kind: CursorKind::Fill,
                        pos,
                    });
                }
            }
        }
        Plan {
            cursors,
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
        let head = self.playhead();
        let win_end = head.saturating_add(self.cfg.stream_window);
        let lead_first = g.piece_of(head);
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

    /// A time -> byte mapping exists (a duration or at least one sample).
    pub fn known(&self) -> bool {
        self.duration_ms.is_some() || !self.samples.is_empty()
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
    fn nothing_is_fetched_blindly_no_head_and_no_tail() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        // No room position, no player: nothing to fetch.
        let p = pol.plan(&have_ranges(&[]), 0);
        assert!(p.cursors.is_empty(), "{p:?}");
        // The room is at 100 MiB: its window, and nothing at the file's start or end.
        pol.room_seek(100 * MIB, 106 * MIB, 0);
        let p = pol.plan(&have_ranges(&[]), 0);
        assert_eq!(kinds(&p), vec![CursorKind::Room]);
        assert_eq!(pos(&p, CursorKind::Room), Some(100 * MIB));
        for c in &p.cursors {
            assert!(
                c.pos >= 100 * MIB && c.pos < 106 * MIB,
                "no head or tail cursor: {c:?}"
            );
        }
    }

    #[test]
    fn the_players_demand_is_fetched_on_demand_beside_the_window() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.room_seek(100 * MIB, 106 * MIB, 0);
        // The player opens the stream: it is blocked on the header at byte 0.
        pol.set_demand(Some(0));
        let p = pol.plan(&have_ranges(&[]), 0);
        assert_eq!(kinds(&p), vec![CursorKind::Room, CursorKind::Demand]);
        assert_eq!(pos(&p, CursorKind::Demand), Some(0));
        // The header is there; the player now waits for the MKV Cues near the end.
        pol.set_demand(Some(197 * MIB + 5));
        let p = pol.plan(&have_ranges(&[(0, MIB)]), 1);
        assert_eq!(pos(&p, CursorKind::Demand), Some(197 * MIB + 5));
        // Once that piece is there, the demand ends (only what was asked for).
        let p = pol.plan(&have_ranges(&[(0, MIB), (197 * MIB, 198 * MIB)]), 2);
        assert_eq!(pos(&p, CursorKind::Demand), None);
        // A demand inside the room window is the room cursor's job.
        pol.set_demand(Some(101 * MIB));
        let p = pol.plan(&have_ranges(&[]), 3);
        assert_eq!(kinds(&p), vec![CursorKind::Room]);
        // No request waiting: nothing.
        pol.set_demand(None);
        assert_eq!(
            pos(&pol.plan(&have_ranges(&[]), 4), CursorKind::Demand),
            None
        );
    }

    #[test]
    fn the_window_follows_the_room_and_a_seek_demotes_the_old_one() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.room_seek(100 * MIB, 106 * MIB, 0);
        assert_eq!(
            pos(&pol.plan(&have_ranges(&[]), 0), CursorKind::Room),
            Some(100 * MIB)
        );
        // Playback moves the window (heartbeat refresh): the cursor follows.
        pol.set_room(103 * MIB, 109 * MIB);
        let p = pol.plan(&have_ranges(&[(100 * MIB, 104 * MIB)]), 1);
        assert_eq!(pos(&p, CursorKind::Room), Some(104 * MIB));
        // Room seek to 150 MiB: the new window first and alone; nothing for the old one.
        pol.room_seek(150 * MIB, 156 * MIB, 2);
        let p = pol.plan(&have_ranges(&[(100 * MIB, 104 * MIB)]), 2);
        assert_eq!(kinds(&p), vec![CursorKind::Room]);
        assert_eq!(pos(&p, CursorKind::Room), Some(150 * MIB));
        assert!(p.exclusive);
        // Seek back to downloaded data: nothing urgent.
        pol.room_seek(100 * MIB, 104 * MIB, 3);
        assert_eq!(
            pos(
                &pol.plan(&have_ranges(&[(100 * MIB, 104 * MIB)]), 3),
                CursorKind::Room
            ),
            None
        );
    }

    #[test]
    fn after_the_window_the_read_ahead_then_the_rest_from_the_playhead() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.room_seek(100 * MIB, 106 * MIB, 0);
        let win = [(100 * MIB, 106 * MIB)];
        // Window there, player not reading yet: the read-ahead goes on from the room position.
        let p = pol.plan(&have_ranges(&win), 1);
        assert_eq!(kinds(&p), vec![CursorKind::Lead]);
        assert_eq!(pos(&p, CursorKind::Lead), Some(100 * MIB));
        // The player reads at 101 MiB; read-ahead full and calm: the fill starts after the
        // lead's window and wraps around (it reaches the start and the end last, not first).
        pol.on_read(101 * MIB);
        let full = [(100 * MIB, 140 * MIB)];
        pol.plan(&have_ranges(&full), 1_000_000);
        let p = pol.plan(&have_ranges(&full), 3_100_000);
        assert_eq!(kinds(&p), vec![CursorKind::Lead, CursorKind::Fill]);
        assert_eq!(pos(&p, CursorKind::Fill), Some(140 * MIB));
        // Near the end: the fill wraps to the first missing piece of the file.
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.on_read(180 * MIB);
        let tail = [(180 * MIB, LEN)];
        pol.plan(&have_ranges(&tail), 0);
        let p = pol.plan(&have_ranges(&tail), 2_100_000);
        assert_eq!(pos(&p, CursorKind::Fill), Some(0));
    }

    #[test]
    fn rapid_room_seeks_keep_one_room_cursor_on_the_latest_target() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        let base = [(0, 40 * MIB)];
        pol.plan(&have_ranges(&base), 0);
        for (i, at) in [60u64, 150, 80, 170].iter().enumerate() {
            pol.room_seek(at * MIB, (at + 6) * MIB, i as i64 * 100_000);
            let p = pol.plan(&have_ranges(&base), i as i64 * 100_000);
            assert_eq!(kinds(&p), vec![CursorKind::Room]);
            assert_eq!(pos(&p, CursorKind::Room), Some(at * MIB));
        }
    }

    #[test]
    fn complete_file_needs_no_cursors() {
        let mut pol = PiecePolicy::new(cfg(), geo());
        pol.room_seek(10 * MIB, 16 * MIB, 0);
        pol.set_demand(Some(5));
        let p = pol.plan(&have_ranges(&[(0, LEN)]), 0);
        assert!(p.complete && p.cursors.is_empty());
    }

    /// librqbit's order, simulated: every open stream queues the 32 MiB after its position;
    /// the streams' next missing pieces are fetched in turn, `rate` pieces per tick.
    fn swarm_tick(
        have: &mut [bool],
        g: &FileGeometry,
        plan: &Plan,
        window: u64,
        rate: usize,
    ) -> Vec<usize> {
        let mut queues: Vec<Vec<usize>> = plan
            .cursors
            .iter()
            .map(|c| {
                let a = g.piece_of(c.pos);
                let b = g.piece_of((c.pos + window).min(g.len) - 1);
                (a..=b).filter(|p| !have[*p]).collect()
            })
            .collect();
        let mut got = Vec::new();
        while got.len() < rate && queues.iter().any(|q| !q.is_empty()) {
            for q in queues.iter_mut() {
                if let Some(p) = (!q.is_empty()).then(|| q.remove(0))
                    && !have[p]
                    && got.len() < rate
                {
                    have[p] = true;
                    got.push(p);
                }
            }
        }
        got
    }

    #[test]
    fn a_member_joining_a_playing_room_gets_the_playing_part_first() {
        // 200 MiB over 2000 s (0.1 MiB per second); the room plays from 1000 s (100 MiB).
        // 2 pieces (2 MiB) per 200 ms tick, i.e. 10 MiB/s: a fast swarm, so order matters.
        let g = geo();
        let rc = crate::ready::ReadyConfig::default();
        let map = ByteMap::new(LEN, Some(2_000_000));
        let mut tracker = crate::ready::ReadyTracker::default();
        let mut pol = PiecePolicy::new(cfg(), g);
        let mut have = vec![false; g.pieces()];
        let mut order = Vec::new();
        let mut ready_at = None;
        for tick in 0..50i64 {
            let now = tick * 200_000;
            let room_ms = 1_000_000 + (now / 1000) as u64;
            let (a, b) = crate::ready::window(
                &map,
                LEN,
                room_ms,
                rc.window_ms,
                rc.back_ms,
                rc.min_bytes,
                rc.max_bytes,
            )
            .unwrap();
            if tick == 0 {
                pol.room_seek(a, b, now);
            } else {
                pol.set_room(a, b);
            }
            // The player opens at once and reads the header first (on demand).
            pol.set_demand((tick < 2).then_some(0));
            let plan = pol.plan(&have, now);
            order.extend(swarm_tick(&mut have, &g, &plan, cfg().stream_window, 2));
            let r = crate::ready::evaluate(
                &rc,
                &mut tracker,
                &map,
                &g,
                &have,
                room_ms,
                &[(0, 64 * 1024)],
                true,
            )
            .unwrap();
            if r.ready && ready_at.is_none() {
                ready_at = Some(tick);
            }
        }
        let first: Vec<u64> = order
            .iter()
            .take(8)
            .map(|p| g.piece_start(*p) / MIB)
            .collect();
        assert!(
            ready_at.is_some_and(|t| t <= 3),
            "ready at the room position within 0.6 s: {ready_at:?}, first pieces MiB {first:?}"
        );
        // The first pieces: the header the player asked for and the playing part; never the
        // file's end.
        assert!(
            first
                .iter()
                .all(|m| *m == 0 || (99..=110).contains(m) || (32..40).contains(m)),
            "{first:?}"
        );
        assert!(
            !order.iter().take(20).any(|p| *p >= 190),
            "no tail fetched early"
        );
    }

    proptest! {
        #[test]
        fn cursors_stay_inside_the_file_and_are_few(
            seeks in proptest::collection::vec((0u64..LEN, 0i64..10_000_000, any::<bool>()), 1..20),
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
            for (i, (pos, dt, demand)) in seeks.into_iter().enumerate() {
                now += dt;
                if i % 2 == 0 {
                    pol.on_seek(pos, now);
                } else {
                    let r = (pos / 2).min(LEN);
                    pol.room_seek(r, (r + 6 * MIB).min(LEN), now);
                }
                pol.set_demand(demand.then_some(LEN - 1 - pos / 3));
                let p = pol.plan(&have, now);
                prop_assert!(p.cursors.len() <= 3);
                if let Some(r) = pos_of(&p, CursorKind::Room) {
                    prop_assert!(!have[g.piece_of(r)], "the room cursor targets a missing piece");
                    prop_assert_eq!(p.cursors[0].kind, CursorKind::Room, "room first");
                }
                if let Some(d) = pos_of(&p, CursorKind::Demand) {
                    prop_assert!(!have[g.piece_of(d)], "demand only while missing");
                }
                let urgent = pos_of(&p, CursorKind::Room).is_some() || pos_of(&p, CursorKind::Demand).is_some();
                if urgent {
                    prop_assert!(pos_of(&p, CursorKind::Lead).is_none() && pos_of(&p, CursorKind::Fill).is_none(), "nothing else ahead of the window");
                }
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
}

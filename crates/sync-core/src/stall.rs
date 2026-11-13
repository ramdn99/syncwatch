//! The buffering popup of THIS window's own player (ADR 0012). It shows only when this player
//! is really stalled on missing data, never because of the torrent's overall status, another
//! member's state or a brief cache toggle:
//!
//! - **Input** ("starving"): the player paused for cache while it should play, or this
//!   participant's read window at the room position is not on disk while the room plays or
//!   waits to play.
//! - **Show delay:** the condition must last `show_after_us` (default 700 ms).
//! - **Hide delay:** the condition must be gone for `hide_after_us`.
//! - **Guard:** while the position advances and the player is not paused for cache, it never
//!   shows (and an open popup closes at once).
//! - **Clear:** every transition that could leave it stale clears it (seek done, resume, media
//!   change, reconnect, torrent complete, stream -> finished file).

/// Largest forward step between two position reports that still counts as playback.
const MAX_PLAY_STEP_MS: i64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StallConfig {
    pub show_after_us: i64,
    pub hide_after_us: i64,
    /// The position counts as advancing when it moved within this time.
    pub advancing_us: i64,
}

impl Default for StallConfig {
    fn default() -> Self {
        Self {
            show_after_us: 700_000,
            hide_after_us: 400_000,
            advancing_us: 400_000,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StallTracker {
    pub cfg: StallConfig,
    since: Option<i64>,
    ok_since: Option<i64>,
    shown: bool,
    last_pos: Option<i64>,
    advanced_at: Option<i64>,
    cache_paused: bool,
}

impl StallTracker {
    pub fn new(cfg: StallConfig) -> Self {
        Self {
            cfg,
            ..Self::default()
        }
    }

    pub fn shown(&self) -> bool {
        self.shown
    }

    /// The player is playing on: its position moved recently and it is not paused for cache.
    pub fn advancing(&self, mono: i64) -> bool {
        !self.cache_paused
            && self
                .advanced_at
                .is_some_and(|t| mono - t <= self.cfg.advancing_us)
    }

    /// A position report of this window's player. Returns true when the popup state changed.
    /// Only playback counts as advancing: not paused, not paused for cache, and a small forward
    /// step (a seek jump is not playback).
    pub fn on_position(
        &mut self,
        pos_ms: i64,
        paused: bool,
        cache_paused: bool,
        mono: i64,
    ) -> bool {
        let step = self.last_pos.map(|p| pos_ms - p);
        if paused || cache_paused {
            self.advanced_at = None;
        } else if step.is_some_and(|d| d > 0 && d <= MAX_PLAY_STEP_MS) {
            self.advanced_at = Some(mono);
        }
        self.last_pos = Some(pos_ms);
        self.cache_paused = cache_paused;
        if self.advancing(mono) {
            // Guard: playing and moving means not stalled, whatever else says so.
            return self.clear();
        }
        false
    }

    /// Re-evaluates with the current condition. Returns true when the popup state changed.
    pub fn update(&mut self, starving: bool, mono: i64) -> bool {
        let starving = starving && !self.advancing(mono);
        if starving {
            self.ok_since = None;
            let s = *self.since.get_or_insert(mono);
            if !self.shown && mono - s >= self.cfg.show_after_us {
                self.shown = true;
                return true;
            }
        } else {
            self.since = None;
            if self.shown {
                let o = *self.ok_since.get_or_insert(mono);
                if mono - o >= self.cfg.hide_after_us {
                    self.shown = false;
                    self.ok_since = None;
                    return true;
                }
            }
        }
        false
    }

    /// A transition that could leave the popup stale: start over (a stall that persists shows
    /// again after the show delay). Returns true when it was shown.
    pub fn clear(&mut self) -> bool {
        self.since = None;
        self.ok_since = None;
        std::mem::replace(&mut self.shown, false)
    }

    /// Media change / reconnect: also forget the position history.
    pub fn reset(&mut self) -> bool {
        self.last_pos = None;
        self.advanced_at = None;
        self.cache_paused = false;
        self.clear()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1000;

    /// Feeds position reports every 100 ms from `t0` to `t1`; `starving` and the cache flag
    /// per step. Returns the shown state after each step.
    fn run(
        s: &mut StallTracker,
        t0: i64,
        t1: i64,
        pos: impl Fn(i64) -> i64,
        cache: impl Fn(i64) -> bool,
        starving: impl Fn(i64) -> bool,
    ) -> Vec<bool> {
        let mut v = Vec::new();
        let mut t = t0;
        while t <= t1 {
            s.on_position(pos(t), false, cache(t), t);
            s.update(starving(t), t);
            v.push(s.shown());
            t += 100 * MS;
        }
        v
    }

    #[test]
    fn never_shows_while_the_position_advances() {
        let mut s = StallTracker::default();
        // Even if something claims "starving" (e.g. a stale window report), a playing,
        // advancing player never gets the popup.
        let v = run(&mut s, 0, 5_000 * MS, |t| t / MS, |_| false, |_| true);
        assert!(v.iter().skip(1).all(|x| !x), "{v:?}");
    }

    #[test]
    fn a_cache_toggle_shorter_than_the_show_delay_never_shows() {
        let mut s = StallTracker::default();
        // Playing; 500 ms cache pause at 2 s (position frozen), then playing again.
        let stalled = |t: i64| (2_000 * MS..2_500 * MS).contains(&t);
        let pos = |t: i64| {
            if t < 2_000 * MS {
                t / MS
            } else if t < 2_500 * MS {
                2_000
            } else {
                2_000 + (t - 2_500 * MS) / MS
            }
        };
        let v = run(&mut s, 0, 4_000 * MS, pos, stalled, stalled);
        assert!(v.iter().all(|x| !x), "{v:?}");
    }

    #[test]
    fn a_real_stall_shows_after_the_delay_and_clears_after_recovery() {
        let mut s = StallTracker::default();
        let stalled = |t: i64| (1_000 * MS..4_000 * MS).contains(&t);
        let pos = |t: i64| {
            if t < 1_000 * MS {
                t / MS
            } else if t < 4_000 * MS {
                1_000
            } else {
                1_000 + (t - 4_000 * MS) / MS
            }
        };
        let v = run(&mut s, 0, 6_000 * MS, pos, stalled, stalled);
        // Index = t / 100 ms. Not before 1.7 s; shown from 1.7 s through the stall.
        assert!(!v[16], "{v:?}");
        assert!(v[17] && v[39], "{v:?}");
        // Recovery: the position advances again at 4.1 s -> cleared at once (guard).
        assert!(v[41..].iter().all(|x| !x), "{v:?}");
    }

    #[test]
    fn hide_delay_applies_when_data_is_back_but_playback_has_not_moved_yet() {
        let mut s = StallTracker::default();
        // Waiting at the gate (not playing, position fixed), window missing for 2 s.
        let v = run(
            &mut s,
            0,
            3_000 * MS,
            |_| 500,
            |_| false,
            |t| t < 2_000 * MS,
        );
        assert!(v[10], "shown while the window is missing");
        // Window there from 2.0 s: still shown until 2.4 s, hidden from then on.
        assert!(v[23] && !v[24] && !v[30], "{v:?}");
    }

    #[test]
    fn a_seek_jump_or_a_paused_player_is_not_advancing() {
        let mut s = StallTracker::default();
        s.on_position(1_000, false, false, 0);
        s.on_position(1_100, false, false, 100 * MS);
        assert!(s.advancing(100 * MS));
        // Paused (waiting at the gate): not advancing, whatever the position does.
        s.on_position(1_100, true, false, 200 * MS);
        assert!(!s.advancing(200 * MS));
        // A seek jump while playing is not playback either.
        s.on_position(90_000, false, false, 300 * MS);
        assert!(!s.advancing(300 * MS));
        // Real playback again.
        s.on_position(90_100, false, false, 400 * MS);
        assert!(s.advancing(400 * MS));
    }

    #[test]
    fn clear_and_reset_drop_a_stale_popup() {
        let mut s = StallTracker::default();
        run(&mut s, 0, 1_000 * MS, |_| 0, |_| false, |_| true);
        assert!(s.shown());
        assert!(s.clear());
        assert!(!s.shown());
        // A stall that persists comes back only after the full show delay again.
        assert!(!s.update(true, 1_100 * MS));
        assert!(!s.update(true, 1_700 * MS));
        assert!(s.update(true, 1_800 * MS));
        assert!(s.reset());
        assert!(!s.shown());
    }
}

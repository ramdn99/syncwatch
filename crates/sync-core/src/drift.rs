//! Drift controller (spec section 9): under 0.15 s ignore, 0.15-2 s speed nudge (0.97-1.03),
//! over 2 s hard seek.

use syncwatch_protocol::{MediaMs, SpeedPermille};

pub const IGNORE_MS: i64 = 150;
pub const HARD_SEEK_MS: i64 = 2_000;
/// Stop nudging once within this distance (hysteresis).
pub const SETTLED_MS: i64 = 40;
/// Correct the error over roughly this long.
const CORRECTION_HORIZON_MS: i64 = 4_000;
const NUDGE_MIN: f64 = 0.97;
const NUDGE_MAX: f64 = 1.03;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftAction {
    Keep,
    SetSpeed(SpeedPermille),
    HardSeek(MediaMs),
}

#[derive(Debug, Default, Clone)]
pub struct DriftController {
    nudging: bool,
}

impl DriftController {
    pub fn reset(&mut self) {
        self.nudging = false;
    }

    pub fn is_nudging(&self) -> bool {
        self.nudging
    }

    /// `diff_ms` = local - expected (positive: local is ahead).
    pub fn decide(
        &mut self,
        diff_ms: i64,
        expected_ms: MediaMs,
        room_speed: SpeedPermille,
    ) -> DriftAction {
        let a = diff_ms.abs();
        if a > HARD_SEEK_MS {
            self.nudging = false;
            return DriftAction::HardSeek(expected_ms.max(0));
        }
        if self.nudging && a <= SETTLED_MS {
            self.nudging = false;
            return DriftAction::SetSpeed(room_speed);
        }
        if !self.nudging && a < IGNORE_MS {
            return DriftAction::Keep;
        }
        // Proportional nudge: behind (diff < 0) => faster.
        let factor =
            (1.0 - diff_ms as f64 / CORRECTION_HORIZON_MS as f64).clamp(NUDGE_MIN, NUDGE_MAX);
        self.nudging = true;
        let speed = (room_speed as f64 * factor)
            .round()
            .clamp(1.0, u32::MAX as f64) as SpeedPermille;
        DriftAction::SetSpeed(speed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds() {
        let mut d = DriftController::default();
        assert_eq!(d.decide(100, 10_000, 1000), DriftAction::Keep);
        assert_eq!(d.decide(-100, 10_000, 1000), DriftAction::Keep);
        // 400 ms behind: speed up, within the cap.
        let DriftAction::SetSpeed(s) = d.decide(-400, 10_000, 1000) else {
            panic!()
        };
        assert!(s > 1000 && s <= 1030, "{s}");
        // Still nudging at 100 ms (hysteresis), then reset once settled.
        assert!(matches!(
            d.decide(-100, 10_000, 1000),
            DriftAction::SetSpeed(_)
        ));
        assert_eq!(d.decide(-20, 10_000, 1000), DriftAction::SetSpeed(1000));
        // Far ahead: capped slowdown.
        assert_eq!(d.decide(1_900, 10_000, 1000), DriftAction::SetSpeed(970));
        // Over 2 s: hard seek to the expected position.
        assert_eq!(
            d.decide(-2_500, 10_000, 1000),
            DriftAction::HardSeek(10_000)
        );
    }

    #[test]
    fn nudge_scales_with_room_speed() {
        let mut d = DriftController::default();
        assert_eq!(d.decide(-1_900, 0, 1500), DriftAction::SetSpeed(1545));
    }
}

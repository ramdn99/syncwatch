//! Watchdog (spec section 2, pitfall 2). Pure state machines driven by a monotonic clock.
//!
//! Peers: Rust sends a keepalive every second on each link. "Interrupted" is raised only when
//! NO inbound traffic at all (keepalives or data) arrived for `INTERRUPT_AFTER`; it clears on
//! the next inbound frame. After `RESTART_AFTER` of silence the link is restarted. A link that
//! is merely idle at the application level is never flagged, because keepalives keep flowing.
//!
//! Bridge: Rust pings the bridge page every second; no pong for `BRIDGE_HUNG_AFTER` means the
//! page is hung and is recreated.

pub const KEEPALIVE_EVERY_US: i64 = 1_000_000;
pub const INTERRUPT_AFTER_US: i64 = 4_000_000;
pub const RESTART_AFTER_US: i64 = 10_000_000;
pub const BRIDGE_PING_EVERY_US: i64 = 1_000_000;
pub const BRIDGE_HUNG_AFTER_US: i64 = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerAction {
    Interrupted { silence_us: i64 },
    Recovered { silence_us: i64 },
    Restart { silence_us: i64 },
}

#[derive(Debug, Clone)]
pub struct PeerWatch {
    last_rx_us: i64,
    interrupted: bool,
    restarted: bool,
    pub frames_rx: u64,
    pub frames_tx: u64,
    /// Largest gap between two inbound frames seen so far.
    pub worst_gap_us: i64,
}

impl PeerWatch {
    pub fn new(now: i64) -> Self {
        Self {
            last_rx_us: now,
            interrupted: false,
            restarted: false,
            frames_rx: 0,
            frames_tx: 0,
            worst_gap_us: 0,
        }
    }

    pub fn on_rx(&mut self, now: i64) -> Option<PeerAction> {
        let gap = now - self.last_rx_us;
        self.worst_gap_us = self.worst_gap_us.max(gap);
        self.last_rx_us = now;
        self.frames_rx += 1;
        if self.interrupted {
            self.interrupted = false;
            return Some(PeerAction::Recovered { silence_us: gap });
        }
        None
    }

    pub fn on_tx(&mut self) {
        self.frames_tx += 1;
    }

    pub fn silence_us(&self, now: i64) -> i64 {
        now - self.last_rx_us
    }

    pub fn tick(&mut self, now: i64) -> Option<PeerAction> {
        let silence = self.silence_us(now);
        if silence >= RESTART_AFTER_US && !self.restarted {
            self.restarted = true;
            return Some(PeerAction::Restart {
                silence_us: silence,
            });
        }
        if silence >= INTERRUPT_AFTER_US && !self.interrupted {
            self.interrupted = true;
            return Some(PeerAction::Interrupted {
                silence_us: silence,
            });
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct BridgeWatch {
    next_seq: u32,
    last_pong_us: i64,
    last_ping_us: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeAction {
    Ping { seq: u32 },
    Recreate { silent_us: i64 },
}

impl BridgeWatch {
    pub fn new(now: i64) -> Self {
        Self {
            next_seq: 1,
            last_pong_us: now,
            last_ping_us: None,
        }
    }

    pub fn on_pong(&mut self, now: i64) {
        self.last_pong_us = now;
    }

    /// Call often (e.g. every 250 ms).
    pub fn tick(&mut self, now: i64) -> Option<BridgeAction> {
        if now - self.last_pong_us >= BRIDGE_HUNG_AFTER_US {
            self.last_pong_us = now; // give the recreated page a fresh window
            return Some(BridgeAction::Recreate {
                silent_us: BRIDGE_HUNG_AFTER_US,
            });
        }
        if self
            .last_ping_us
            .is_none_or(|t| now - t >= BRIDGE_PING_EVERY_US)
        {
            self.last_ping_us = Some(now);
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            return Some(BridgeAction::Ping { seq });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000;

    #[test]
    fn regular_keepalives_never_trigger_an_interruption() {
        let mut w = PeerWatch::new(0);
        for i in 1..=900 {
            // 15 minutes of keepalives with up to 1.5 s jitter.
            let now = i * S + (i % 3) * S / 4;
            assert_eq!(w.on_rx(now), None);
            assert_eq!(w.tick(now + S / 2), None, "no false interruption at {i}");
        }
        assert!(w.worst_gap_us < 2 * S);
    }

    #[test]
    fn silence_interrupts_then_restarts_and_recovery_clears() {
        let mut w = PeerWatch::new(0);
        assert_eq!(w.tick(3 * S), None);
        assert_eq!(
            w.tick(4 * S),
            Some(PeerAction::Interrupted { silence_us: 4 * S })
        );
        assert_eq!(w.tick(5 * S), None, "reported once");
        assert_eq!(
            w.on_rx(6 * S),
            Some(PeerAction::Recovered { silence_us: 6 * S })
        );
        // A second, longer silence leads to a restart.
        assert_eq!(
            w.tick(10 * S),
            Some(PeerAction::Interrupted { silence_us: 4 * S })
        );
        assert_eq!(
            w.tick(16 * S),
            Some(PeerAction::Restart { silence_us: 10 * S })
        );
        assert_eq!(w.tick(30 * S), None, "restart requested once");
    }

    #[test]
    fn bridge_pings_and_recreates_when_hung() {
        let mut b = BridgeWatch::new(0);
        assert_eq!(b.tick(0), Some(BridgeAction::Ping { seq: 1 }));
        assert_eq!(b.tick(S / 2), None);
        assert_eq!(b.tick(S), Some(BridgeAction::Ping { seq: 2 }));
        b.on_pong(S + 1000);
        assert_eq!(b.tick(5 * S), Some(BridgeAction::Ping { seq: 3 }));
        assert_eq!(
            b.tick(6 * S + 1000),
            Some(BridgeAction::Recreate {
                silent_us: BRIDGE_HUNG_AFTER_US
            })
        );
    }
}

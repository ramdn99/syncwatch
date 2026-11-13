//! SyncWatch sync-core: a pure, deterministic state machine (spec section 8/9).
//!
//! `Peer::handle(clock, event) -> Vec<Effect>`. No I/O, no threads, no system time: every input
//! (network messages, player observations, UI actions, timer ticks, local-file results) is an
//! [`Event`], and every output (messages to send, player commands, UI notifications, file writes)
//! is an [`Effect`]. The app and the simulator (`sim` module) are the only places that perform
//! effects.

pub mod chat;
pub mod clock_sync;
pub mod drift;
pub mod gate;
pub mod host;
pub mod local;
pub mod member;
pub mod peer;
pub mod queue;
pub mod tracks;
pub mod transfer;
mod types;

#[cfg(feature = "sim")]
pub mod sim;
pub mod stall;

pub use host::{HostConfig, HostMachine};
pub use member::MemberMachine;
pub use peer::Peer;
pub use types::*;

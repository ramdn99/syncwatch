//! Identifiers and time units.

use serde::{Deserialize, Serialize};

/// A member's seat number, assigned by the host. `MemberId::HOST` is always the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MemberId(pub u32);

impl MemberId {
    pub const HOST: MemberId = MemberId(0);
}

/// Per-install ed25519 public key (authenticated in the handshake, Milestone 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity(pub [u8; 32]);

/// Playback command epoch: strictly increasing per media epoch.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct Epoch(pub u64);

/// Incremented on every MediaChange; commands for an older media epoch are dropped.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct MediaEpoch(pub u64);

/// Incremented on every track-state change.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
pub struct TrackEpoch(pub u64);

/// Manifest entry id of an external subtitle/audio file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalId(pub u32);

/// Host-assigned chat message id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChatId(pub u64);

/// Host monotonic time in microseconds.
pub type HostMicros = i64;
/// Media position in milliseconds.
pub type MediaMs = i64;
/// Playback speed in thousandths (1000 = 1.0x).
pub type SpeedPermille = u32;

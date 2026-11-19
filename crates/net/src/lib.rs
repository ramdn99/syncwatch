//! SyncWatch networking (spec section 1 "Transport", section 8).
//!
//! WebRTC data channels run in a hidden WebView2 window (the "bridge", implemented in the
//! desktop app) behind [`PeerTransport`]. Everything with timing (keepalives, silence
//! detection, restarts, clock sync, scheduling) lives in Rust; the bridge JavaScript only does
//! WebRTC plumbing. This crate holds the pure parts: the binary IPC framing, the wire framing,
//! SDP fingerprint extraction, the watchdog and the signaling client.

pub mod auth;
pub mod frame;
pub mod handshake;
pub mod invite;
pub mod sdp;
pub mod session;
pub mod signaling;
pub mod transport;
pub mod watchdog;

pub use transport::*;

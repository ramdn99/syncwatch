//! Handshake transports (ADR 0014): how the first WebRTC offer and answer reach the other side.
//!
//! - [`HandshakeMode::ManualInvite`]: the release default. No network at all: the offer and
//!   the answer travel as copy-pasted invite and reply texts ([`crate::invite`]). The session
//!   handles them itself; its transport is [`NoNetwork`].
//! - [`HandshakeMode::Signaling`]: the WebSocket signaling server ([`crate::signaling`]), kept
//!   for development and the in-process tests only. It is not part of release builds and needs
//!   no deployment.
//!
//! Another message bus (for example Metered Realtime Messaging, see ADR 0014) would implement
//! [`HandshakeTransport`] and run the session in `Signaling` mode.

use crate::signaling::SignalingConn;
use std::future::Future;
use std::pin::Pin;
use syncwatch_protocol::signaling::{ClientMsg, ServerMsg};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Which handshake the room session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeMode {
    /// Copy-paste invites and replies; nothing on the network until the host admits.
    ManualInvite {
        /// Validity of each invite.
        invite_ttl_secs: u64,
        /// How long a dropped member's seat is kept for a re-invite.
        seat_keep_us: i64,
    },
    /// A signaling server relays offers, answers and candidates (dev and tests).
    Signaling,
}

impl HandshakeMode {
    pub const fn manual_default() -> Self {
        Self::ManualInvite {
            invite_ttl_secs: crate::invite::DEFAULT_TTL_SECS,
            seat_keep_us: 5 * 60 * 1_000_000,
        }
    }

    pub fn is_manual(&self) -> bool {
        matches!(self, Self::ManualInvite { .. })
    }
}

/// Carries the session's signaling messages (`Output::Signaling` out, `Input::Signaling` in).
pub trait HandshakeTransport: Send {
    /// For logs and diagnostics ("manual invites", "signaling server").
    fn describe(&self) -> &'static str;
    fn send<'a>(&'a mut self, m: &'a ClientMsg) -> BoxFut<'a, Result<(), String>>;
    /// The next message, or `None` when the connection is lost (the driver may reconnect).
    fn recv(&mut self) -> BoxFut<'_, Option<ServerMsg>>;
}

/// The manual-invite backend: nothing on the network. `recv` never yields; `send` refuses
/// (a session in manual mode never emits signaling messages).
pub struct NoNetwork;

impl HandshakeTransport for NoNetwork {
    fn describe(&self) -> &'static str {
        "manual invites"
    }
    fn send<'a>(&'a mut self, _m: &'a ClientMsg) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async { Err("manual invites use no signaling server".to_owned()) })
    }
    fn recv(&mut self) -> BoxFut<'_, Option<ServerMsg>> {
        Box::pin(std::future::pending())
    }
}

impl HandshakeTransport for SignalingConn {
    fn describe(&self) -> &'static str {
        "signaling server"
    }
    fn send<'a>(&'a mut self, m: &'a ClientMsg) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            SignalingConn::send(self, m)
                .await
                .map_err(|e| e.to_string())
        })
    }
    fn recv(&mut self) -> BoxFut<'_, Option<ServerMsg>> {
        Box::pin(async move { SignalingConn::recv(self).await.ok() })
    }
}

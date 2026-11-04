//! The transport abstraction the room layer talks to.

use serde::{Deserialize, Serialize};
use syncwatch_protocol::signaling::SignalData;

/// Local handle of one peer connection inside the bridge.
pub type PeerId = u32;

/// Data channels per peer: reliable+ordered (control, chat, file chunks) and
/// unordered+no-retransmit (heartbeats, clock pings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Lane {
    Reliable = 0,
    Unreliable = 1,
}

impl Lane {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Lane::Reliable),
            1 => Some(Lane::Unreliable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IceRole {
    /// Creates the offer (the joining member).
    Offerer,
    /// Answers (the host).
    Answerer,
}

/// One `RTCIceServer` (credentials only travel from Rust to the bridge, never logged).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServerJs {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl std::fmt::Debug for IceServerJs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IceServerJs({} urls, <redacted>)", self.urls.len())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceConfig {
    pub ice_servers: Vec<IceServerJs>,
    /// "Always use relay (hide my IP address)".
    pub relay_only: bool,
}

/// Facts about an open peer connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    /// DTLS fingerprints from the local and remote SDP (bound into SPAKE2 in Milestone 5).
    pub local_fingerprint: Option<String>,
    pub remote_fingerprint: Option<String>,
    /// The selected candidate pair uses a TURN relay.
    pub relayed: bool,
}

/// Why a link was flagged; logged without secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub silence_ms: u64,
    pub frames_rx: u64,
    pub frames_tx: u64,
    pub ice_state: String,
    pub connection_state: String,
    pub cause: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// SDP or candidate to send to the remote side through signaling (trickle ICE).
    LocalSignal {
        peer: PeerId,
        data: SignalData,
    },
    Open {
        peer: PeerId,
        info: PeerInfo,
    },
    Frame {
        peer: PeerId,
        lane: Lane,
        data: Vec<u8>,
    },
    /// Traffic actually stopped (no keepalive for the interrupt threshold).
    Interrupted {
        peer: PeerId,
        diag: Diagnostics,
    },
    Recovered {
        peer: PeerId,
    },
    /// The watchdog gave up on the link; the session layer must renegotiate this peer.
    RestartNeeded {
        peer: PeerId,
        diag: Diagnostics,
    },
    Closed {
        peer: PeerId,
        reason: String,
    },
    /// The bridge page was reloaded (hung); every peer is gone and must be renegotiated.
    BridgeReloaded,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    #[error("the network bridge is not ready")]
    BridgeNotReady,
    #[error("unknown peer")]
    UnknownPeer,
    #[error("message too large")]
    TooLarge,
}

/// The transport the room layer uses. Implemented by the WebView2 bridge in the desktop app.
pub trait PeerTransport: Send + Sync {
    fn create_peer(&self, peer: PeerId, role: IceRole, ice: IceConfig) -> Result<(), NetError>;
    /// Remote SDP or candidate from signaling.
    fn remote_signal(&self, peer: PeerId, data: SignalData) -> Result<(), NetError>;
    fn send(&self, peer: PeerId, lane: Lane, data: Vec<u8>) -> Result<(), NetError>;
    fn close_peer(&self, peer: PeerId);
}

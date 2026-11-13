//! SyncWatch wire protocol (spec sections 6, 8, 9, 11). Pure: no I/O.
//!
//! Every frame is an [`Envelope`] `{ version, message }` encoded with postcard. Decoding checks
//! the frame size first, then the protocol version, then [`Message::validate`] (string, list and
//! chunk size limits). The only message type that carries bulk bytes is
//! [`Message::FileChunk`], and it always names a manifest `ExternalId`. The main media is never
//! transmitted; see [`MessageKind::carries_bulk_bytes`] and its exhaustive test.

pub mod auth;
pub mod ids;
pub mod limits;
pub mod magnet;
pub mod media;
pub mod messages;
pub mod permissions;
pub mod signaling;
pub mod tracks;

pub use ids::*;
pub use media::*;
pub use messages::*;
pub use permissions::*;
pub use tracks::*;

use serde::{Deserialize, Serialize};

/// Bumped on any incompatible change. Peers with a different version are rejected in Hello.
pub const PROTOCOL_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u16,
    pub message: Message,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("frame too large ({0} bytes)")]
    FrameTooLarge(usize),
    #[error("malformed frame")]
    Malformed,
    #[error("unsupported protocol version {0}")]
    Version(u16),
    #[error("field out of limits: {0}")]
    Limit(&'static str),
}

/// Encodes a message into a frame.
pub fn encode(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    message.validate()?;
    let env = Envelope {
        version: PROTOCOL_VERSION,
        message: message.clone(),
    };
    let bytes = postcard::to_allocvec(&env).map_err(|_| ProtocolError::Malformed)?;
    if bytes.len() > limits::MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Decodes and validates a frame received from the network (untrusted input).
pub fn decode(frame: &[u8]) -> Result<Message, ProtocolError> {
    if frame.len() > limits::MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge(frame.len()));
    }
    let env: Envelope = postcard::from_bytes(frame).map_err(|_| ProtocolError::Malformed)?;
    if env.version != PROTOCOL_VERSION {
        return Err(ProtocolError::Version(env.version));
    }
    env.message.validate()?;
    Ok(env.message)
}

//! Peer authentication handshake (spec section 9 "Handshake and auth", ADR 0006).
//!
//! Runs on the reliable data channel right after it opens, BEFORE any room [`Message`]
//! (crate::Message). It has its own small versioned postcard envelope so a peer that fails
//! authentication never reaches the room state machine.
//!
//! Order (the member proves first, so a wrong password is always detected by the host and can
//! be throttled; the host's confirmation is only sent after the member's checked out):
//!   host   -> member  HostHello
//!   member -> host    MemberHello (+ SPAKE2 message A when the room has a password)
//!   host   -> member  HostSpake (password rooms only)
//!   member -> host    MemberConfirm (MAC + ed25519 signature over the transcript)
//!   host   -> member  HostConfirm + Outcome
//! The transcript covers the room code, both DTLS fingerprints, both identities, the name, both
//! nonces and the SPAKE2 messages.

use crate::{Identity, ProtocolError, limits};
use serde::{Deserialize, Serialize};

pub const AUTH_VERSION: u16 = 1;
pub const MAX_AUTH_FRAME: usize = 4096;
pub const NONCE_LEN: usize = 32;
/// SPAKE2 messages over Ed25519 are 33 bytes (side byte + point).
pub const MAX_SPAKE_LEN: usize = 64;
pub const SIG_LEN: usize = 64;
pub const MAX_PASSWORD_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthReject {
    Version,
    BadPassword,
    /// Too many wrong passwords (identity temporarily banned or the room paused attempts).
    TooManyAttempts,
    /// The handshake itself failed (bad MAC or signature, malformed, unexpected message).
    Failed,
    Banned,
    Locked,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMsg {
    HostHello {
        version: u16,
        host_identity: Identity,
        password_required: bool,
        nonce: [u8; NONCE_LEN],
    },
    MemberHello {
        version: u16,
        identity: Identity,
        display_name: String,
        nonce: [u8; NONCE_LEN],
        spake: Option<Vec<u8>>,
    },
    HostSpake {
        spake: Vec<u8>,
    },
    MemberConfirm {
        mac: [u8; 32],
        sig: Vec<u8>,
    },
    HostConfirm {
        mac: [u8; 32],
        sig: Vec<u8>,
    },
    /// Authentication failed. `retry_after_ms`: the member may try another password after
    /// this delay on the same connection (0 = the connection is closed).
    Rejected {
        reason: AuthReject,
        retry_after_ms: u32,
    },
    /// Old host -> new host before a host transfer: the room password moves with the host
    /// role so the new host can authenticate members. Only sent over an authenticated link.
    HandoverSecret {
        password: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct AuthEnvelope {
    version: u16,
    msg: AuthMsg,
}

impl AuthMsg {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            AuthMsg::MemberHello {
                display_name,
                spake,
                ..
            } => {
                if display_name.len() > limits::MAX_NAME_BYTES {
                    return Err(ProtocolError::Limit("display_name"));
                }
                if spake.as_ref().is_some_and(|s| s.len() > MAX_SPAKE_LEN) {
                    return Err(ProtocolError::Limit("spake"));
                }
            }
            AuthMsg::HostSpake { spake } if spake.len() > MAX_SPAKE_LEN => {
                return Err(ProtocolError::Limit("spake"));
            }
            AuthMsg::MemberConfirm { sig, .. } | AuthMsg::HostConfirm { sig, .. }
                if sig.len() != SIG_LEN =>
            {
                return Err(ProtocolError::Limit("sig"));
            }
            AuthMsg::HandoverSecret { password: Some(p) } if p.len() > MAX_PASSWORD_BYTES => {
                return Err(ProtocolError::Limit("password"));
            }
            _ => {}
        }
        Ok(())
    }
}

pub fn encode_auth(msg: &AuthMsg) -> Result<Vec<u8>, ProtocolError> {
    msg.validate()?;
    let bytes = postcard::to_allocvec(&AuthEnvelope {
        version: AUTH_VERSION,
        msg: msg.clone(),
    })
    .map_err(|_| ProtocolError::Malformed)?;
    if bytes.len() > MAX_AUTH_FRAME {
        return Err(ProtocolError::FrameTooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Decodes untrusted input: size, version, then field limits.
pub fn decode_auth(frame: &[u8]) -> Result<AuthMsg, ProtocolError> {
    if frame.len() > MAX_AUTH_FRAME {
        return Err(ProtocolError::FrameTooLarge(frame.len()));
    }
    let env: AuthEnvelope = postcard::from_bytes(frame).map_err(|_| ProtocolError::Malformed)?;
    if env.version != AUTH_VERSION {
        return Err(ProtocolError::Version(env.version));
    }
    env.msg.validate()?;
    Ok(env.msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_limits() {
        let m = AuthMsg::MemberHello {
            version: AUTH_VERSION,
            identity: Identity([7; 32]),
            display_name: "Ana".into(),
            nonce: [1; 32],
            spake: Some(vec![0; 33]),
        };
        assert_eq!(decode_auth(&encode_auth(&m).unwrap()).unwrap(), m);
        let long = AuthMsg::MemberHello {
            version: AUTH_VERSION,
            identity: Identity([7; 32]),
            display_name: "x".repeat(500),
            nonce: [1; 32],
            spake: None,
        };
        assert_eq!(
            encode_auth(&long),
            Err(ProtocolError::Limit("display_name"))
        );
        let bad_sig = AuthMsg::MemberConfirm {
            mac: [0; 32],
            sig: vec![0; 10],
        };
        assert_eq!(encode_auth(&bad_sig), Err(ProtocolError::Limit("sig")));
        assert_eq!(
            decode_auth(&[0xff; 5000]),
            Err(ProtocolError::FrameTooLarge(5000))
        );
        assert_eq!(decode_auth(&[9, 9]), Err(ProtocolError::Malformed));
    }
}

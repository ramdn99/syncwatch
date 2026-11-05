//! Signaling messages between the app and `syncwatch-signaling` (spec section 1 "Signaling").
//!
//! JSON text frames over a WebSocket. The server only routes SDP and ICE candidates between a
//! room's host and its members: members can reach ONLY the host, never each other. It never
//! sees passwords, credentials, chat or media. Every field is size-limited.

use serde::{Deserialize, Serialize};

pub const SIGNALING_VERSION: u16 = 1;
/// Largest signaling frame accepted (an SDP with many candidates fits easily).
pub const MAX_SIGNAL_FRAME: usize = 32 * 1024;
pub const MAX_SDP: usize = 16 * 1024;
pub const MAX_CANDIDATE: usize = 1024;
pub const MAX_MID: usize = 32;
/// Room codes: 8 characters from an alphabet without look-alikes (no 0/O, 1/I/L).
pub const ROOM_CODE_LEN: usize = 8;
pub const ROOM_ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";
/// Host grace period (spec section 1).
pub const HOST_GRACE_SECS: u16 = 30;

/// Signaling-level peer number of a member inside a room (the host has none).
pub type SigPeer = u32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SignalData {
    Offer {
        sdp: String,
    },
    Answer {
        sdp: String,
    },
    Candidate {
        candidate: String,
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        #[serde(rename = "sdpMLineIndex")]
        sdp_m_line_index: Option<u16>,
    },
    EndOfCandidates,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientMsg {
    /// First message: protocol version.
    Hello {
        v: u16,
    },
    /// Become the host of a new room.
    Create {
        max_members: u8,
    },
    /// Host reconnects within the grace period.
    Resume {
        room: String,
        token: String,
    },
    /// Join an existing room as a member.
    Join {
        room: String,
    },
    /// Host: `to` names the member. Members: `to` is ignored (always the host).
    Signal {
        to: Option<SigPeer>,
        data: SignalData,
    },
    /// Host: hand the host role to member `to` (after the room-level handover was sent).
    TransferHost {
        to: SigPeer,
    },
    Leave,
    Ping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SigError {
    BadMessage,
    TooLarge,
    RateLimited,
    Version,
    NoSuchRoom,
    RoomFull,
    NotInRoom,
    NotHost,
    BadToken,
    AlreadyInRoom,
    ServerBusy,
    UnknownPeer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ServerMsg {
    Welcome {
        v: u16,
    },
    Created {
        room: String,
        token: String,
    },
    Resumed {
        room: String,
        peers: Vec<SigPeer>,
    },
    Joined {
        room: String,
        peer: SigPeer,
    },
    PeerJoined {
        peer: SigPeer,
    },
    PeerLeft {
        peer: SigPeer,
    },
    /// The host dropped; the room is held for `grace_s` seconds.
    HostGone {
        grace_s: u16,
    },
    HostBack,
    RoomClosed,
    /// `from` is the member (to the host) or None (from the host, to a member).
    Signal {
        from: Option<SigPeer>,
        data: SignalData,
    },
    Error {
        code: SigError,
    },
    Pong,
    /// You are the host now (after `TransferHost`); `token` resumes the room.
    BecameHost {
        token: String,
        peers: Vec<SigPeer>,
    },
    /// The host changed. The former host receives its new member number in `you`.
    HostChanged {
        you: Option<SigPeer>,
    },
}

impl SignalData {
    pub fn validate(&self) -> bool {
        match self {
            SignalData::Offer { sdp } | SignalData::Answer { sdp } => {
                sdp.len() <= MAX_SDP && !sdp.is_empty()
            }
            SignalData::Candidate {
                candidate, sdp_mid, ..
            } => {
                candidate.len() <= MAX_CANDIDATE
                    && sdp_mid.as_ref().is_none_or(|m| m.len() <= MAX_MID)
            }
            SignalData::EndOfCandidates => true,
        }
    }
}

pub fn is_valid_room_code(code: &str) -> bool {
    code.len() == ROOM_CODE_LEN && code.chars().all(|c| ROOM_ALPHABET.contains(c))
}

/// Normalizes user input ("k7qm 4xrp", "K7QM-4XRP") to a room code, if valid.
pub fn normalize_room_code(input: &str) -> Option<String> {
    let c: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    is_valid_room_code(&c).then_some(c)
}

/// Builds a room code from random bytes (rejection sampling keeps it uniform).
pub fn room_code_from_random(mut next_byte: impl FnMut() -> u8) -> String {
    let alphabet = ROOM_ALPHABET.as_bytes();
    let limit = (256 / alphabet.len()) * alphabet.len();
    let mut out = String::with_capacity(ROOM_CODE_LEN);
    while out.len() < ROOM_CODE_LEN {
        let b = next_byte() as usize;
        if b < limit {
            out.push(alphabet[b % alphabet.len()] as char);
        }
    }
    out
}

/// Parses a client frame with size and field limits.
pub fn parse_client(text: &str) -> Result<ClientMsg, SigError> {
    if text.len() > MAX_SIGNAL_FRAME {
        return Err(SigError::TooLarge);
    }
    let m: ClientMsg = serde_json::from_str(text).map_err(|_| SigError::BadMessage)?;
    match &m {
        ClientMsg::Signal { data, .. } if !data.validate() => Err(SigError::TooLarge),
        ClientMsg::Resume { room, token } if !is_valid_room_code(room) || token.len() > 128 => {
            Err(SigError::BadMessage)
        }
        ClientMsg::Join { room } if !is_valid_room_code(room) => Err(SigError::NoSuchRoom),
        _ => Ok(m),
    }
}

pub fn parse_server(text: &str) -> Result<ServerMsg, SigError> {
    if text.len() > MAX_SIGNAL_FRAME {
        return Err(SigError::TooLarge);
    }
    let m: ServerMsg = serde_json::from_str(text).map_err(|_| SigError::BadMessage)?;
    if let ServerMsg::Signal { data, .. } = &m
        && !data.validate()
    {
        return Err(SigError::TooLarge);
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_codes() {
        assert!(is_valid_room_code("K7QM4XRP"));
        assert!(!is_valid_room_code("K7QM4XR0"), "0 is ambiguous");
        assert!(!is_valid_room_code("K7QM4XRI"));
        assert_eq!(normalize_room_code(" k7qm-4xrp "), Some("K7QM4XRP".into()));
        assert_eq!(normalize_room_code("short"), None);
        let mut n = 0u8;
        let code = room_code_from_random(|| {
            n = n.wrapping_add(37);
            n
        });
        assert!(is_valid_room_code(&code), "{code}");
        // Bytes above the rejection limit are skipped.
        let mut seq = vec![255u8, 254, 0, 1, 2, 3, 4, 5, 6, 7].into_iter();
        assert_eq!(
            room_code_from_random(|| seq.next().unwrap_or(0)),
            "23456789"
        );
    }

    #[test]
    fn json_shapes_are_stable() {
        let m = ClientMsg::Signal {
            to: Some(3),
            data: SignalData::Candidate {
                candidate: "candidate:1 1 udp 1 192.0.2.1 5000 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
            },
        };
        let j = serde_json::to_string(&m).unwrap();
        assert_eq!(
            j,
            r#"{"t":"signal","to":3,"data":{"kind":"candidate","candidate":"candidate:1 1 udp 1 192.0.2.1 5000 typ host","sdpMid":"0","sdpMLineIndex":0}}"#
        );
        assert_eq!(parse_client(&j).unwrap(), m);
        assert_eq!(
            serde_json::to_string(&ServerMsg::Error {
                code: SigError::RoomFull
            })
            .unwrap(),
            r#"{"t":"error","code":"roomFull"}"#
        );
    }

    #[test]
    fn limits() {
        let big = ClientMsg::Signal {
            to: None,
            data: SignalData::Offer {
                sdp: "x".repeat(MAX_SDP + 1),
            },
        };
        assert_eq!(
            parse_client(&serde_json::to_string(&big).unwrap()),
            Err(SigError::TooLarge)
        );
        assert_eq!(
            parse_client(&"x".repeat(MAX_SIGNAL_FRAME + 1)),
            Err(SigError::TooLarge)
        );
        assert_eq!(parse_client("{\"t\":\"nope\"}"), Err(SigError::BadMessage));
        assert_eq!(
            parse_client(r#"{"t":"join","room":"bad"}"#),
            Err(SigError::NoSuchRoom)
        );
    }
}

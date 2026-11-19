//! Manual invites (ADR 0014): the serverless handshake for release builds.
//!
//! The host's app makes an INVITE text (its complete WebRTC offer with all ICE candidates, the
//! room id, a single-use invite id, its identity, the expiry). The member pastes it and the
//! member's app makes a REPLY text (its complete answer with all candidates). The host pastes
//! the reply, compares the four invite words with the member out loud and admits or refuses.
//! Only after Admit does the host apply the answer: an unadmitted reply never gets a
//! connection, let alone room data.
//!
//! Text format: `SWINV1-` or `SWREP1-` + base64url( deflate( postcard(Signed) ) + checksum4 ).
//! - `Signed` = the postcard body plus the sender's ed25519 signature over a domain-separated
//!   hash of it: a changed invite or reply is refused (tamper check). This does not prove WHO
//!   sent it (anyone can make a key): that is what the words, Admit and the password are for.
//! - The 4-byte checksum (blake3 of the compressed bytes) tells a paste error (a missing
//!   character, a cut-off text) apart from tampering, for a friendly message.
//!
//! No credentials: an invite or reply carries SDP (with its per-connection ICE username
//! fragment and password, which only authorize this one WebRTC session) and ICE candidates. It
//! never carries the Metered API key, TURN usernames or TURN credentials (tests check this).
//! Candidates contain IP addresses; with "Always use relay" only the relay's addresses appear.

use crate::auth::{IdentityKey, VerificationWords, verify, words_for};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use syncwatch_protocol::Identity;
use syncwatch_protocol::signaling::SignalData;

pub const INVITE_PREFIX: &str = "SWINV1-";
pub const REPLY_PREFIX: &str = "SWREP1-";
/// Format version of the invite and reply texts (the room protocol version is inside).
pub const FORMAT_VERSION: u8 = 1;
/// Longest text accepted from the clipboard (before anything else is done with it).
pub const MAX_TEXT: usize = 16 * 1024;
/// Longest decompressed body (protects against decompression bombs).
pub const MAX_BODY: usize = 32 * 1024;
pub const MAX_SDP: usize = 12 * 1024;
pub const MAX_CANDIDATES: usize = 40;
pub const MAX_CANDIDATE_LEN: usize = 512;
pub const MAX_NAME_CHARS: usize = 40;
/// Default validity of an invite (Settings may change it).
pub const DEFAULT_TTL_SECS: u64 = 600;
pub const MIN_TTL_SECS: u64 = 60;
pub const MAX_TTL_SECS: u64 = 24 * 3600;
const CHECKSUM_LEN: usize = 4;

/// One ICE candidate as the bridge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate: String,
    pub mid: Option<String>,
    pub mline: Option<u16>,
}

/// The host's invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Room protocol version (`syncwatch_protocol::PROTOCOL_VERSION`).
    pub protocol: u16,
    pub room_id: [u8; 16],
    /// Single use, unguessable (128 random bits).
    pub invite_id: [u8; 16],
    pub host: Identity,
    pub room_name: String,
    /// The room asks for a password (typed by the member before replying).
    pub password: bool,
    /// Unix seconds.
    pub expires_unix: u64,
    /// A re-invite for a member whose seat is kept: only this identity may reply.
    pub for_member: Option<Identity>,
    pub sdp: String,
    pub candidates: Vec<Candidate>,
}

/// The member's reply to one invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub protocol: u16,
    pub invite_id: [u8; 16],
    pub member: Identity,
    pub display_name: String,
    pub sdp: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    format: u8,
    kind: u8,
    body: Vec<u8>,
    sig: Vec<u8>,
}

const KIND_INVITE: u8 = 1;
const KIND_REPLY: u8 = 2;

/// Why a pasted text was refused. Each maps to a friendly message pointing at the right step.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InviteError {
    #[error("Nothing was pasted.")]
    Empty,
    #[error("This text is too long to be a SyncWatch invite or reply.")]
    TooLong,
    #[error("This is not a SyncWatch invite. Ask the host to copy the invite again.")]
    NotAnInvite,
    #[error("This is not a SyncWatch reply. Ask the member to copy their reply again.")]
    NotAReply,
    #[error("This is a reply, not an invite. Paste it in the host's window (Invites).")]
    ReplyNotInvite,
    #[error("This is an invite, not a reply. Paste it under Join on the member's computer.")]
    InviteNotReply,
    #[error("Part of the text is missing or was changed while copying. Copy it again in full.")]
    Damaged,
    #[error(
        "This was made by a different version of SyncWatch ({0}). Both of you need the same version."
    )]
    Version(String),
    #[error("This text was changed after it was made. Ask for a new one.")]
    Tampered,
    #[error("This invite has expired. Ask the host for a new invite.")]
    Expired,
    #[error("This invite was already used. Ask the host for a new invite.")]
    Used,
    #[error("This reply does not belong to any open invite here.")]
    UnknownInvite,
    #[error("This invite is for someone else (a re-invite for a returning member).")]
    WrongMember,
    #[error("The connection details inside are not valid.")]
    BadConnection,
}

impl Invite {
    /// Signs and encodes this invite as text.
    pub fn encode(&self, key: &IdentityKey) -> String {
        encode(INVITE_PREFIX, KIND_INVITE, self, key)
    }

    /// Decodes and checks a pasted invite: format, checksum, version, structure, signature and
    /// expiry (`now_unix`).
    pub fn decode(text: &str, now_unix: u64) -> Result<Self, InviteError> {
        let t = clean(text)?;
        if t.starts_with(REPLY_PREFIX) {
            return Err(InviteError::ReplyNotInvite);
        }
        let s = unwrap(t, INVITE_PREFIX, InviteError::NotAnInvite, KIND_INVITE)?;
        let inv: Invite = postcard::from_bytes(&s.body).map_err(|_| InviteError::Damaged)?;
        if inv.protocol != syncwatch_protocol::PROTOCOL_VERSION {
            return Err(InviteError::Version(format!("protocol {}", inv.protocol)));
        }
        if !verify(&inv.host, &signed_hash(KIND_INVITE, &s.body), &s.sig) {
            return Err(InviteError::Tampered);
        }
        inv.check_structure()?;
        if now_unix > inv.expires_unix {
            return Err(InviteError::Expired);
        }
        Ok(inv)
    }

    fn check_structure(&self) -> Result<(), InviteError> {
        check_connection(&self.sdp, &self.candidates)?;
        if self.room_name.chars().count() > MAX_NAME_CHARS || has_control(&self.room_name) {
            return Err(InviteError::BadConnection);
        }
        Ok(())
    }

    /// The host's DTLS fingerprint from the offer.
    pub fn fingerprint(&self) -> Option<String> {
        crate::sdp::dtls_fingerprint(&self.sdp)
    }

    /// The remote signals to feed the member's peer connection (offer, candidates, end).
    pub fn signals(&self) -> Vec<SignalData> {
        signals(
            SignalData::Offer {
                sdp: self.sdp.clone(),
            },
            &self.candidates,
        )
    }
}

impl Reply {
    pub fn encode(&self, key: &IdentityKey) -> String {
        encode(REPLY_PREFIX, KIND_REPLY, self, key)
    }

    /// Decodes and checks a pasted reply (format, checksum, version, structure, signature).
    /// Matching it to an open invite is the session's job.
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let t = clean(text)?;
        if t.starts_with(INVITE_PREFIX) {
            return Err(InviteError::InviteNotReply);
        }
        let s = unwrap(t, REPLY_PREFIX, InviteError::NotAReply, KIND_REPLY)?;
        let r: Reply = postcard::from_bytes(&s.body).map_err(|_| InviteError::Damaged)?;
        if r.protocol != syncwatch_protocol::PROTOCOL_VERSION {
            return Err(InviteError::Version(format!("protocol {}", r.protocol)));
        }
        if !verify(&r.member, &signed_hash(KIND_REPLY, &s.body), &s.sig) {
            return Err(InviteError::Tampered);
        }
        check_connection(&r.sdp, &r.candidates)?;
        let name = r.display_name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || has_control(name) {
            return Err(InviteError::BadConnection);
        }
        Ok(r)
    }

    /// The member's DTLS fingerprint from the answer.
    pub fn fingerprint(&self) -> Option<String> {
        crate::sdp::dtls_fingerprint(&self.sdp)
    }

    /// The remote signals to feed the host's peer connection (answer, candidates, end).
    pub fn signals(&self) -> Vec<SignalData> {
        signals(
            SignalData::Answer {
                sdp: self.sdp.clone(),
            },
            &self.candidates,
        )
    }
}

/// The four words both sides show before the host admits the member. They bind the room, the
/// invite, both DTLS certificates and both identities: someone who answers an invite in the
/// member's place, or changes the invite on its way, gets different words.
pub fn invite_words(
    room_id: &[u8; 16],
    invite_id: &[u8; 16],
    host_fp: &str,
    member_fp: &str,
    host: &Identity,
    member: &Identity,
) -> VerificationWords {
    let mut h = blake3::Hasher::new_derive_key("syncwatch invite v1 words");
    for f in [
        &room_id[..],
        &invite_id[..],
        host_fp.as_bytes(),
        member_fp.as_bytes(),
        &host.0,
        &member.0,
    ] {
        h.update(&(f.len() as u64).to_le_bytes());
        h.update(f);
    }
    words_for(h.finalize().as_bytes())
}

/// Lowercase hex, for room ids in logs and the authentication transcript.
pub fn hex16(b: &[u8; 16]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 16 random bytes (OS randomness).
pub fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    let n = crate::auth::random_nonce();
    b.copy_from_slice(&n[..16]);
    b
}

fn signals(first: SignalData, cands: &[Candidate]) -> Vec<SignalData> {
    let mut v = vec![first];
    v.extend(cands.iter().map(|c| SignalData::Candidate {
        candidate: c.candidate.clone(),
        sdp_mid: c.mid.clone(),
        sdp_m_line_index: c.mline,
    }));
    v.push(SignalData::EndOfCandidates);
    v
}

fn signed_hash(kind: u8, body: &[u8]) -> Vec<u8> {
    let ctx = if kind == KIND_INVITE {
        "syncwatch invite v1 signature"
    } else {
        "syncwatch reply v1 signature"
    };
    blake3::derive_key(ctx, body).to_vec()
}

fn encode<T: Serialize>(prefix: &str, kind: u8, v: &T, key: &IdentityKey) -> String {
    let body = postcard::to_allocvec(v).unwrap_or_default();
    let sig = key.sign(&signed_hash(kind, &body));
    let signed = postcard::to_allocvec(&Signed {
        format: FORMAT_VERSION,
        kind,
        body,
        sig,
    })
    .unwrap_or_default();
    let mut packed = miniz_oxide::deflate::compress_to_vec(&signed, 9);
    let sum = blake3::hash(&packed);
    packed.extend_from_slice(&sum.as_bytes()[..CHECKSUM_LEN]);
    format!(
        "{prefix}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed)
    )
}

/// Trims what chat apps add around a pasted text (spaces, line breaks, quotes, backticks).
fn clean(text: &str) -> Result<&str, InviteError> {
    if text.len() > MAX_TEXT {
        return Err(InviteError::TooLong);
    }
    let t =
        text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>'));
    if t.is_empty() {
        return Err(InviteError::Empty);
    }
    Ok(t)
}

fn unwrap(t: &str, prefix: &str, wrong: InviteError, kind: u8) -> Result<Signed, InviteError> {
    let Some(rest) = t.strip_prefix(prefix) else {
        // An older or newer text format of ours.
        if t.starts_with("SWINV") || t.starts_with("SWREP") {
            return Err(InviteError::Version("text format".into()));
        }
        return Err(wrong);
    };
    // Chat apps may wrap long lines: ignore whitespace inside.
    let compact: String = rest.chars().filter(|c| !c.is_whitespace()).collect();
    let packed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(compact.as_bytes())
        .map_err(|_| InviteError::Damaged)?;
    if packed.len() <= CHECKSUM_LEN {
        return Err(InviteError::Damaged);
    }
    let (data, sum) = packed.split_at(packed.len() - CHECKSUM_LEN);
    if &blake3::hash(data).as_bytes()[..CHECKSUM_LEN] != sum {
        return Err(InviteError::Damaged);
    }
    let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(data, MAX_BODY)
        .map_err(|_| InviteError::Damaged)?;
    let s: Signed = postcard::from_bytes(&raw).map_err(|_| InviteError::Damaged)?;
    if s.format != FORMAT_VERSION {
        return Err(InviteError::Version(format!("text format {}", s.format)));
    }
    if s.kind != kind {
        return Err(InviteError::Damaged);
    }
    Ok(s)
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

fn check_connection(sdp: &str, cands: &[Candidate]) -> Result<(), InviteError> {
    if sdp.is_empty() || sdp.len() > MAX_SDP || !sdp.starts_with("v=0") {
        return Err(InviteError::BadConnection);
    }
    if crate::sdp::dtls_fingerprint(sdp).is_none() {
        return Err(InviteError::BadConnection);
    }
    if cands.len() > MAX_CANDIDATES {
        return Err(InviteError::BadConnection);
    }
    for c in cands {
        let ok = c.candidate.len() <= MAX_CANDIDATE_LEN
            && (c.candidate.is_empty() || c.candidate.starts_with("candidate:"))
            && !has_control(&c.candidate)
            && c.mid
                .as_ref()
                .is_none_or(|m| m.len() <= 32 && !has_control(m));
        if !ok {
            return Err(InviteError::BadConnection);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SDP_OFFER: &str = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=ice-ufrag:abcd\r\na=ice-pwd:0123456789abcdefghijklmn\r\na=fingerprint:sha-256 AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99\r\na=setup:actpass\r\na=mid:0\r\na=sctp-port:5000\r\n";

    fn key(seed: u8) -> IdentityKey {
        IdentityKey::from_secret([seed; 32])
    }

    fn cands(n: usize) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                candidate: format!(
                    "candidate:{i} 1 udp 2122260223 192.0.2.{i} 5{i:04} typ host generation 0 ufrag abcd network-id 1"
                ),
                mid: Some("0".into()),
                mline: Some(0),
            })
            .collect()
    }

    fn invite(host: &IdentityKey) -> Invite {
        Invite {
            protocol: syncwatch_protocol::PROTOCOL_VERSION,
            room_id: [7; 16],
            invite_id: [9; 16],
            host: host.public(),
            room_name: "Movie night".into(),
            password: false,
            expires_unix: 2_000_000_000,
            for_member: None,
            sdp: SDP_OFFER.into(),
            candidates: cands(6),
        }
    }

    fn reply(member: &IdentityKey) -> Reply {
        Reply {
            protocol: syncwatch_protocol::PROTOCOL_VERSION,
            invite_id: [9; 16],
            member: member.public(),
            display_name: "Mo".into(),
            sdp: SDP_OFFER.replace("AA:BB", "11:22"),
            candidates: cands(4),
        }
    }

    #[test]
    fn round_trip_and_size() {
        let h = key(1);
        let inv = invite(&h);
        let text = inv.encode(&h);
        assert!(text.starts_with(INVITE_PREFIX));
        // A realistic invite stays small enough for any chat app.
        assert!(text.len() < 2_000, "{} chars", text.len());
        assert_eq!(Invite::decode(&text, 1_900_000_000), Ok(inv.clone()));
        // Chat apps add quotes, spaces and line breaks.
        let wrapped = format!("  \"{}\n{}\"  ", &text[..60], &text[60..]);
        assert_eq!(Invite::decode(&wrapped, 1_900_000_000), Ok(inv));
        let m = key(2);
        let r = reply(&m);
        let rt = r.encode(&m);
        assert!(rt.starts_with(REPLY_PREFIX));
        assert_eq!(Reply::decode(&rt), Ok(r));
    }

    #[test]
    fn expiry_kind_and_version_are_checked() {
        let h = key(1);
        let text = invite(&h).encode(&h);
        assert_eq!(
            Invite::decode(&text, 2_000_000_001),
            Err(InviteError::Expired)
        );
        assert_eq!(Reply::decode(&text), Err(InviteError::InviteNotReply));
        let m = key(2);
        assert_eq!(
            Invite::decode(&reply(&m).encode(&m), 0),
            Err(InviteError::ReplyNotInvite)
        );
        let mut old = invite(&h);
        old.protocol = syncwatch_protocol::PROTOCOL_VERSION - 1;
        assert!(matches!(
            Invite::decode(&old.encode(&h), 0),
            Err(InviteError::Version(_))
        ));
        assert!(matches!(
            Invite::decode("SWINV9-abc", 0),
            Err(InviteError::Version(_))
        ));
        assert_eq!(Invite::decode("hello", 0), Err(InviteError::NotAnInvite));
        assert_eq!(Invite::decode("   ", 0), Err(InviteError::Empty));
        assert_eq!(
            Invite::decode(&"A".repeat(MAX_TEXT + 1), 0),
            Err(InviteError::TooLong)
        );
    }

    #[test]
    fn paste_errors_and_tampering_are_told_apart() {
        let h = key(1);
        let text = invite(&h).encode(&h);
        // Cut off: a paste error.
        assert_eq!(
            Invite::decode(&text[..text.len() - 5], 0),
            Err(InviteError::Damaged)
        );
        // One character changed: the checksum catches it.
        let mut b: Vec<char> = text.chars().collect();
        let i = b.len() / 2;
        b[i] = if b[i] == 'A' { 'B' } else { 'A' };
        let one: String = b.into_iter().collect();
        assert_eq!(Invite::decode(&one, 0), Err(InviteError::Damaged));
        // A careful forger re-encodes a changed body with a valid checksum: the signature fails.
        let mut inv = invite(&h);
        inv.room_name = "Changed".into();
        let forged = re_encode_with_foreign_signature(&inv, &h, &key(3));
        assert_eq!(Invite::decode(&forged, 0), Err(InviteError::Tampered));
    }

    /// Signs `inv` (claiming host `claimed`) with someone else's key.
    fn re_encode_with_foreign_signature(
        inv: &Invite,
        claimed: &IdentityKey,
        signer: &IdentityKey,
    ) -> String {
        let mut v = inv.clone();
        v.host = claimed.public();
        encode(INVITE_PREFIX, KIND_INVITE, &v, signer)
    }

    #[test]
    fn decompression_bombs_and_bad_structure_are_refused() {
        let h = key(1);
        // A body that inflates beyond MAX_BODY.
        let bomb = miniz_oxide::deflate::compress_to_vec(&vec![0u8; MAX_BODY * 4], 9);
        let mut packed = bomb.clone();
        packed.extend_from_slice(&blake3::hash(&bomb).as_bytes()[..CHECKSUM_LEN]);
        let text = format!(
            "{INVITE_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(packed)
        );
        assert_eq!(Invite::decode(&text, 0), Err(InviteError::Damaged));
        // No DTLS fingerprint, too many candidates, a control character in the name.
        let mut a = invite(&h);
        a.sdp = "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n".into();
        assert_eq!(
            Invite::decode(&a.encode(&h), 0),
            Err(InviteError::BadConnection)
        );
        let mut b = invite(&h);
        b.candidates = cands(MAX_CANDIDATES + 1);
        assert_eq!(
            Invite::decode(&b.encode(&h), 0),
            Err(InviteError::BadConnection)
        );
        let mut c = invite(&h);
        c.room_name = "bad\u{7}name".into();
        assert_eq!(
            Invite::decode(&c.encode(&h), 0),
            Err(InviteError::BadConnection)
        );
        let m = key(2);
        let mut r = reply(&m);
        r.display_name = "   ".into();
        assert_eq!(
            Reply::decode(&r.encode(&m)),
            Err(InviteError::BadConnection)
        );
    }

    /// Random and mutated input never panics (fuzz-style).
    #[test]
    fn arbitrary_input_never_panics() {
        let h = key(1);
        let good = invite(&h).encode(&h);
        let m = key(2);
        let good_reply = reply(&m).encode(&m);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..3000 {
            let base = if round % 2 == 0 { &good } else { &good_reply };
            let mut b: Vec<u8> = base.as_bytes().to_vec();
            for _ in 0..(1 + next() % 4) {
                let i = (next() as usize) % b.len();
                match next() % 3 {
                    0 => b[i] = (next() % 128) as u8,
                    1 => {
                        b.remove(i);
                    }
                    _ => b.insert(i, b"-_ABCxyz019="[(next() % 12) as usize]),
                }
            }
            let s = String::from_utf8_lossy(&b);
            let _ = Invite::decode(&s, 0);
            let _ = Reply::decode(&s);
        }
        for len in [0usize, 1, 7, 8, 100, 5000] {
            let junk: String = (0..len)
                .map(|_| (b'A' + (next() % 26) as u8) as char)
                .collect();
            let _ = Invite::decode(&format!("{INVITE_PREFIX}{junk}"), 0);
            let _ = Reply::decode(&format!("{REPLY_PREFIX}{junk}"));
        }
    }

    #[test]
    fn words_depend_on_every_input() {
        let (h, m) = (key(1).public(), key(2).public());
        let base = invite_words(&[1; 16], &[2; 16], "fpH", "fpM", &h, &m);
        assert_eq!(base, invite_words(&[1; 16], &[2; 16], "fpH", "fpM", &h, &m));
        assert_ne!(base, invite_words(&[1; 16], &[2; 16], "fpH", "fpX", &h, &m));
        assert_ne!(base, invite_words(&[1; 16], &[3; 16], "fpH", "fpM", &h, &m));
        assert_ne!(
            base,
            invite_words(&[1; 16], &[2; 16], "fpH", "fpM", &h, &key(3).public())
        );
    }
}

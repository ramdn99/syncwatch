//! Minimal STUN (RFC 5389/8489) and TURN (RFC 5766/8656) message codec for the connection
//! test. Verified against the RFC 5769 test vectors (see tests).

use hmac::{Hmac, KeyInit, Mac};
use md5::{Digest, Md5};
use sha1::Sha1;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const MAGIC_COOKIE: u32 = 0x2112_A442;
const FINGERPRINT_XOR: u32 = 0x5354_554E;
pub const HEADER_LEN: usize = 20;
/// Largest message accepted from a server.
pub const MAX_MESSAGE: usize = 4096;

pub mod method {
    pub const BINDING: u16 = 0x0001;
    pub const ALLOCATE: u16 = 0x0003;
    pub const REFRESH: u16 = 0x0004;
}

pub mod class {
    pub const REQUEST: u16 = 0x0000;
    pub const SUCCESS: u16 = 0x0100;
    pub const ERROR: u16 = 0x0110;
}

pub mod attr {
    pub const MAPPED_ADDRESS: u16 = 0x0001;
    pub const USERNAME: u16 = 0x0006;
    pub const MESSAGE_INTEGRITY: u16 = 0x0008;
    pub const ERROR_CODE: u16 = 0x0009;
    pub const LIFETIME: u16 = 0x000D;
    pub const REALM: u16 = 0x0014;
    pub const NONCE: u16 = 0x0015;
    pub const XOR_RELAYED_ADDRESS: u16 = 0x0016;
    pub const REQUESTED_TRANSPORT: u16 = 0x0019;
    pub const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    pub const SOFTWARE: u16 = 0x8022;
    pub const FINGERPRINT: u16 = 0x8028;
}

/// Protocol number for UDP in REQUESTED-TRANSPORT.
pub const TRANSPORT_UDP: u8 = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StunError {
    #[error("message too short")]
    Short,
    #[error("not a STUN message")]
    NotStun,
    #[error("bad length")]
    BadLength,
    #[error("message too large")]
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Method | class bits, e.g. 0x0003 (Allocate request), 0x0113 (Allocate error).
    pub typ: u16,
    pub txid: [u8; 12],
    pub attrs: Vec<(u16, Vec<u8>)>,
}

/// MESSAGE-INTEGRITY key for the long-term credential mechanism: MD5(user ":" realm ":" pass).
pub fn long_term_key(username: &str, realm: &str, password: &str) -> Vec<u8> {
    Md5::digest(format!("{username}:{realm}:{password}").as_bytes()).to_vec()
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    // HMAC accepts keys of any length; `new_from_slice` cannot fail for HMAC.
    let mut mac = match <Hmac<Sha1> as KeyInit>::new_from_slice(key) {
        Ok(m) => m,
        Err(_) => return [0; 20],
    };
    mac.update(data);
    let out = mac.finalize().into_bytes();
    let mut r = [0u8; 20];
    r.copy_from_slice(&out);
    r
}

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

impl Message {
    pub fn new(typ: u16, txid: [u8; 12]) -> Self {
        Self {
            typ,
            txid,
            attrs: Vec::new(),
        }
    }

    pub fn with(mut self, a: u16, v: impl Into<Vec<u8>>) -> Self {
        self.attrs.push((a, v.into()));
        self
    }

    pub fn method(&self) -> u16 {
        self.typ & 0x3EEF
    }

    pub fn class(&self) -> u16 {
        self.typ & 0x0110
    }

    pub fn get(&self, a: u16) -> Option<&[u8]> {
        self.attrs
            .iter()
            .find(|(t, _)| *t == a)
            .map(|(_, v)| v.as_slice())
    }

    /// Serializes, optionally appending MESSAGE-INTEGRITY (HMAC-SHA1 with `key`) and
    /// FINGERPRINT.
    pub fn encode(&self, integrity_key: Option<&[u8]>, fingerprint: bool) -> Vec<u8> {
        let mut body = Vec::new();
        for (t, v) in &self.attrs {
            body.extend_from_slice(&t.to_be_bytes());
            body.extend_from_slice(&(v.len() as u16).to_be_bytes());
            body.extend_from_slice(v);
            body.resize(pad4(body.len()), 0);
        }
        let mut out = Vec::with_capacity(HEADER_LEN + body.len() + 32);
        out.extend_from_slice(&self.typ.to_be_bytes());
        out.extend_from_slice(&[0, 0]); // length, patched below
        out.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        out.extend_from_slice(&self.txid);
        out.extend_from_slice(&body);
        let set_len = |out: &mut Vec<u8>, len: usize| {
            let l = (len as u16).to_be_bytes();
            out[2] = l[0];
            out[3] = l[1];
        };
        if let Some(key) = integrity_key {
            let l = out.len() - HEADER_LEN + 24;
            set_len(&mut out, l);
            let mac = hmac_sha1(key, &out);
            out.extend_from_slice(&attr::MESSAGE_INTEGRITY.to_be_bytes());
            out.extend_from_slice(&20u16.to_be_bytes());
            out.extend_from_slice(&mac);
        }
        if fingerprint {
            let l = out.len() - HEADER_LEN + 8;
            set_len(&mut out, l);
            let crc = crc32fast::hash(&out) ^ FINGERPRINT_XOR;
            out.extend_from_slice(&attr::FINGERPRINT.to_be_bytes());
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&crc.to_be_bytes());
        }
        let l = out.len() - HEADER_LEN;
        set_len(&mut out, l);
        out
    }

    /// Length of the message at the start of `buf` (for stream framing), if the header is there.
    pub fn frame_len(buf: &[u8]) -> Option<usize> {
        if buf.len() < HEADER_LEN {
            return None;
        }
        Some(HEADER_LEN + u16::from_be_bytes([buf[2], buf[3]]) as usize)
    }

    pub fn decode(buf: &[u8]) -> Result<Self, StunError> {
        if buf.len() < HEADER_LEN {
            return Err(StunError::Short);
        }
        if buf.len() > MAX_MESSAGE {
            return Err(StunError::TooLarge);
        }
        let typ = u16::from_be_bytes([buf[0], buf[1]]);
        let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        let cookie = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if typ & 0xC000 != 0 || cookie != MAGIC_COOKIE || !len.is_multiple_of(4) {
            return Err(StunError::NotStun);
        }
        if HEADER_LEN + len != buf.len() {
            return Err(StunError::BadLength);
        }
        let mut txid = [0u8; 12];
        txid.copy_from_slice(&buf[8..20]);
        let mut attrs = Vec::new();
        let mut i = HEADER_LEN;
        while i < buf.len() {
            if i + 4 > buf.len() {
                return Err(StunError::BadLength);
            }
            let t = u16::from_be_bytes([buf[i], buf[i + 1]]);
            let l = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
            let start = i + 4;
            if start + l > buf.len() {
                return Err(StunError::BadLength);
            }
            attrs.push((t, buf[start..start + l].to_vec()));
            i = start + pad4(l);
        }
        Ok(Self { typ, txid, attrs })
    }

    /// Byte offset of attribute `a` within a raw message.
    fn attr_offset(raw: &[u8], a: u16) -> Option<usize> {
        let mut i = HEADER_LEN;
        while i + 4 <= raw.len() {
            let t = u16::from_be_bytes([raw[i], raw[i + 1]]);
            let l = u16::from_be_bytes([raw[i + 2], raw[i + 3]]) as usize;
            if t == a {
                return Some(i);
            }
            i += 4 + pad4(l);
        }
        None
    }

    /// Verifies MESSAGE-INTEGRITY of a raw message with `key`.
    pub fn verify_integrity(raw: &[u8], key: &[u8]) -> bool {
        let Some(off) = Self::attr_offset(raw, attr::MESSAGE_INTEGRITY) else {
            return false;
        };
        if off + 24 > raw.len() {
            return false;
        }
        let mut data = raw[..off].to_vec();
        let len = ((off - HEADER_LEN + 24) as u16).to_be_bytes();
        data[2] = len[0];
        data[3] = len[1];
        hmac_sha1(key, &data)[..] == raw[off + 4..off + 24]
    }

    /// Verifies FINGERPRINT of a raw message.
    pub fn verify_fingerprint(raw: &[u8]) -> bool {
        let Some(off) = Self::attr_offset(raw, attr::FINGERPRINT) else {
            return false;
        };
        if off + 8 != raw.len() {
            return false;
        }
        let want = crc32fast::hash(&raw[..off]) ^ FINGERPRINT_XOR;
        raw[off + 4..off + 8] == want.to_be_bytes()
    }

    /// Decodes an XOR-*-ADDRESS attribute.
    pub fn xor_address(&self, a: u16) -> Option<SocketAddr> {
        let v = self.get(a)?;
        if v.len() < 8 {
            return None;
        }
        let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
        let cookie = MAGIC_COOKIE.to_be_bytes();
        match v[1] {
            0x01 => {
                let mut ip = [0u8; 4];
                for i in 0..4 {
                    ip[i] = v[4 + i] ^ cookie[i];
                }
                Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), port))
            }
            0x02 if v.len() >= 20 => {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&cookie);
                key[4..].copy_from_slice(&self.txid);
                let mut ip = [0u8; 16];
                for i in 0..16 {
                    ip[i] = v[4 + i] ^ key[i];
                }
                Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::from(ip)), port))
            }
            _ => None,
        }
    }

    /// Encodes an address as an XOR-*-ADDRESS value (used by the test server).
    pub fn xor_address_value(addr: SocketAddr, txid: &[u8; 12]) -> Vec<u8> {
        let cookie = MAGIC_COOKIE.to_be_bytes();
        let port = addr.port() ^ (MAGIC_COOKIE >> 16) as u16;
        let mut v = vec![0, 0];
        v.extend_from_slice(&port.to_be_bytes());
        match addr.ip() {
            IpAddr::V4(ip) => {
                v[1] = 0x01;
                v.extend(ip.octets().iter().zip(cookie.iter()).map(|(a, b)| a ^ b));
            }
            IpAddr::V6(ip) => {
                v[1] = 0x02;
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&cookie);
                key[4..].copy_from_slice(txid);
                v.extend(ip.octets().iter().zip(key.iter()).map(|(a, b)| a ^ b));
            }
        }
        v
    }

    /// ERROR-CODE as (code, reason).
    pub fn error_code(&self) -> Option<(u16, String)> {
        let v = self.get(attr::ERROR_CODE)?;
        if v.len() < 4 {
            return None;
        }
        let code = (v[2] & 0x07) as u16 * 100 + v[3] as u16;
        Some((code, String::from_utf8_lossy(&v[4..]).into_owned()))
    }

    pub fn error_value(code: u16, reason: &str) -> Vec<u8> {
        let mut v = vec![0, 0, (code / 100) as u8, (code % 100) as u8];
        v.extend_from_slice(reason.as_bytes());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    /// RFC 5769 section 2.1: sample request, short-term credentials.
    const REQ: &str = "00 01 00 58 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
        80 22 00 10 53 54 55 4e 20 74 65 73 74 20 63 6c 69 65 6e 74
        00 24 00 04 6e 00 01 ff 80 29 00 08 93 2f f9 b1 51 26 3b 36
        00 06 00 09 65 76 74 6a 3a 68 36 76 59 20 20 20
        00 08 00 14 9a ea a7 0c bf d8 cb 56 78 1e f2 b5 b2 d3 f2 49 c1 b5 71 a2
        80 28 00 04 e5 7a 3b cf";
    /// RFC 5769 section 2.2: IPv4 response.
    const RESP4: &str = "01 01 00 3c 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
        80 22 00 0b 74 65 73 74 20 76 65 63 74 6f 72 20
        00 20 00 08 00 01 a1 47 e1 12 a6 43
        00 08 00 14 2b 91 f5 99 fd 9e 90 c3 8c 74 89 f9 2a f9 ba 53 f0 6b e7 d7
        80 28 00 04 c0 7d 4c 96";
    /// RFC 5769 section 2.3: IPv6 response.
    const RESP6: &str = "01 01 00 48 21 12 a4 42 b7 e7 a7 01 bc 34 d6 86 fa 87 df ae
        80 22 00 0b 74 65 73 74 20 76 65 63 74 6f 72 20
        00 20 00 14 00 02 a1 47 01 13 a9 fa a5 d3 f1 79 bc 25 f4 b5 be d2 b9 d9
        00 08 00 14 a3 82 95 4e 4b e6 7b f1 17 84 c9 7c 82 92 c2 75 bf e3 ed 41
        80 28 00 04 c8 fb 0b 4c";
    /// RFC 5769 section 2.4: request with long-term authentication (no FINGERPRINT).
    const REQ_LT: &str = "00 01 00 60 21 12 a4 42 78 ad 34 33 c6 ad 72 c0 29 da 41 2e
        00 06 00 12 e3 83 9e e3 83 88 e3 83 aa e3 83 83 e3 82 af e3 82 b9 00 00
        00 15 00 1c 66 2f 2f 34 39 39 6b 39 35 34 64 36 4f 4c 33 34 6f 4c 39 46 53 54 76 79 36 34 73 41
        00 14 00 0b 65 78 61 6d 70 6c 65 2e 6f 72 67 00
        00 08 00 14 f6 70 24 65 6d d6 4a 3e 02 b8 e0 71 2e 85 c9 a2 8c a8 96 66";
    const PASSWORD: &str = "VOkJxbRl1RmTxUk/WvJxBt";

    #[test]
    fn rfc5769_sample_request() {
        let raw = hex(REQ);
        let m = Message::decode(&raw).unwrap();
        assert_eq!(m.typ, 0x0001);
        assert_eq!(m.get(attr::USERNAME), Some(&b"evtj:h6vY"[..]));
        assert!(Message::verify_integrity(&raw, PASSWORD.as_bytes()));
        assert!(Message::verify_fingerprint(&raw));
        assert!(!Message::verify_integrity(&raw, b"wrong password"));
    }

    #[test]
    fn rfc5769_ipv4_and_ipv6_responses() {
        let raw = hex(RESP4);
        let m = Message::decode(&raw).unwrap();
        assert_eq!(
            m.xor_address(attr::XOR_MAPPED_ADDRESS),
            Some("192.0.2.1:32853".parse().unwrap())
        );
        assert!(Message::verify_integrity(&raw, PASSWORD.as_bytes()));
        assert!(Message::verify_fingerprint(&raw));

        let raw6 = hex(RESP6);
        let m6 = Message::decode(&raw6).unwrap();
        assert_eq!(
            m6.xor_address(attr::XOR_MAPPED_ADDRESS),
            Some(
                "[2001:db8:1234:5678:11:2233:4455:6677]:32853"
                    .parse()
                    .unwrap()
            )
        );
        assert!(Message::verify_integrity(&raw6, PASSWORD.as_bytes()));
        assert!(Message::verify_fingerprint(&raw6));
    }

    #[test]
    fn rfc5769_long_term_authentication() {
        let raw = hex(REQ_LT);
        let m = Message::decode(&raw).unwrap();
        let user = std::str::from_utf8(m.get(attr::USERNAME).unwrap()).unwrap();
        let realm = std::str::from_utf8(m.get(attr::REALM).unwrap()).unwrap();
        assert_eq!(realm, "example.org");
        // Password after SASLprep: "TheMatrIX".
        let key = long_term_key(user, realm, "TheMatrIX");
        assert!(Message::verify_integrity(&raw, &key));
    }

    #[test]
    fn encode_round_trip_with_integrity_and_fingerprint() {
        let txid = [7u8; 12];
        let m = Message::new(method::ALLOCATE, txid)
            .with(attr::REQUESTED_TRANSPORT, vec![TRANSPORT_UDP, 0, 0, 0])
            .with(attr::USERNAME, b"user".to_vec())
            .with(attr::REALM, b"realm".to_vec());
        let key = long_term_key("user", "realm", "pass");
        let raw = m.encode(Some(&key), true);
        let d = Message::decode(&raw).unwrap();
        assert_eq!(d.method(), method::ALLOCATE);
        assert_eq!(d.class(), class::REQUEST);
        assert!(Message::verify_integrity(&raw, &key));
        assert!(Message::verify_fingerprint(&raw));
        let addr: SocketAddr = "203.0.113.9:49152".parse().unwrap();
        let x = Message::new(0x0103, txid).with(
            attr::XOR_RELAYED_ADDRESS,
            Message::xor_address_value(addr, &txid),
        );
        assert_eq!(x.xor_address(attr::XOR_RELAYED_ADDRESS), Some(addr));
        let e = Message::new(0x0113, txid).with(
            attr::ERROR_CODE,
            Message::error_value(486, "Allocation Quota Reached"),
        );
        assert_eq!(
            e.error_code(),
            Some((486, "Allocation Quota Reached".into()))
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(Message::decode(&[0; 10]), Err(StunError::Short));
        let mut raw = hex(REQ);
        raw[4] = 0;
        assert_eq!(Message::decode(&raw), Err(StunError::NotStun));
        let mut raw = hex(REQ);
        raw.pop();
        assert!(Message::decode(&raw).is_err());
        let mut raw = hex(REQ);
        raw[3] = 0x10; // length lies
        assert_eq!(Message::decode(&raw), Err(StunError::BadLength));
    }
}

//! Authoritative magnet-link parser (spec section 6). The TypeScript mirror in the UI uses the
//! same test vectors (`crates/protocol/tests/magnet-vectors.json`).
//!
//! Accepts `magnet:?xt=urn:btih:<40 hex | 32 base32>` with optional `dn` and `tr`. Rejects
//! BitTorrent v2-only links (`btmh`), multiple different btih values and oversized input.

use crate::media::Infohash;
use crate::media::MAX_MAGNET_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Magnet {
    pub infohash: Infohash,
    pub display_name: Option<String>,
    pub trackers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MagnetError {
    #[error("not a magnet link")]
    NotMagnet,
    #[error("magnet link is too long")]
    TooLong,
    #[error("magnet link has no BitTorrent infohash (xt=urn:btih:...)")]
    NoInfohash,
    #[error("infohash is not 40 hex or 32 base32 characters")]
    BadInfohash,
    #[error("magnet link has conflicting infohashes")]
    ConflictingInfohash,
    #[error("BitTorrent v2-only magnets (btmh) are not supported")]
    V2Only,
    #[error("malformed percent-encoding")]
    BadEncoding,
}

const MAX_TRACKERS: usize = 32;
const MAX_DN_CHARS: usize = 255;

pub fn parse(input: &str) -> Result<Magnet, MagnetError> {
    let s = input.trim();
    if s.len() > MAX_MAGNET_BYTES {
        return Err(MagnetError::TooLong);
    }
    let rest = s
        .get(..8)
        .filter(|p| p.eq_ignore_ascii_case("magnet:?"))
        .map(|_| &s[8..])
        .ok_or(MagnetError::NotMagnet)?;

    let mut infohash: Option<Infohash> = None;
    let mut saw_v2 = false;
    let mut dn = None;
    let mut trackers = Vec::new();
    for pair in rest.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k.to_ascii_lowercase().as_str() {
            "xt" | "xt.1" | "xt.2" => {
                let v = percent_decode(v, false)?;
                let lower = v.to_ascii_lowercase();
                if let Some(h) = lower.strip_prefix("urn:btih:") {
                    let ih = parse_infohash(&v[v.len() - h.len()..])?;
                    if infohash.is_some_and(|x| x != ih) {
                        return Err(MagnetError::ConflictingInfohash);
                    }
                    infohash = Some(ih);
                } else if lower.starts_with("urn:btmh:") {
                    saw_v2 = true;
                }
            }
            "dn" => {
                let name = percent_decode(v, true)?;
                let clean: String = name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_DN_CHARS)
                    .collect();
                if !clean.trim().is_empty() {
                    dn = Some(clean.trim().to_owned());
                }
            }
            "tr" if trackers.len() < MAX_TRACKERS => {
                let t = percent_decode(v, false)?;
                if t.len() <= 512 && !trackers.contains(&t) {
                    trackers.push(t);
                }
            }
            _ => {}
        }
    }
    match infohash {
        Some(infohash) => Ok(Magnet {
            infohash,
            display_name: dn,
            trackers,
        }),
        None if saw_v2 => Err(MagnetError::V2Only),
        None => Err(MagnetError::NoInfohash),
    }
}

fn parse_infohash(s: &str) -> Result<Infohash, MagnetError> {
    let mut out = [0u8; 20];
    match s.len() {
        40 => {
            for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
                let hi = hex_val(chunk[0]).ok_or(MagnetError::BadInfohash)?;
                let lo = hex_val(chunk[1]).ok_or(MagnetError::BadInfohash)?;
                out[i] = (hi << 4) | lo;
            }
        }
        32 => {
            // RFC 4648 base32, no padding: 32 chars * 5 bits = 160 bits.
            let mut acc: u64 = 0;
            let mut bits = 0;
            let mut i = 0;
            for &c in s.as_bytes() {
                let v = match c.to_ascii_uppercase() {
                    b @ b'A'..=b'Z' => b - b'A',
                    b @ b'2'..=b'7' => b - b'2' + 26,
                    _ => return Err(MagnetError::BadInfohash),
                };
                acc = (acc << 5) | v as u64;
                bits += 5;
                if bits >= 8 {
                    bits -= 8;
                    out[i] = (acc >> bits) as u8;
                    i += 1;
                }
            }
        }
        _ => return Err(MagnetError::BadInfohash),
    }
    Ok(Infohash(out))
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str, plus_is_space: bool) -> Result<String, MagnetError> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hi = b
                    .get(i + 1)
                    .copied()
                    .and_then(hex_val)
                    .ok_or(MagnetError::BadEncoding)?;
                let lo = b
                    .get(i + 2)
                    .copied()
                    .and_then(hex_val)
                    .ok_or(MagnetError::BadEncoding)?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| MagnetError::BadEncoding)
}

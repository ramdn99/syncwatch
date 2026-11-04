//! Validated ICE server model (spec section 5 "Validation").
//!
//! Schemes stun/stuns/turn/turns only; turn(s) need username and credential; at most 16
//! entries; bounded string lengths. URIs follow RFC 7064/7065:
//! `scheme:host[:port][?transport=udp|tcp]`.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_SERVERS: usize = 16;
pub const MAX_URLS_PER_SERVER: usize = 16;
pub const MAX_URL_LEN: usize = 512;
pub const MAX_USERNAME_LEN: usize = 512;
pub const MAX_CREDENTIAL_LEN: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    Stun,
    Stuns,
    Turn,
    Turns,
}

impl Scheme {
    pub fn is_turn(self) -> bool {
        matches!(self, Scheme::Turn | Scheme::Turns)
    }
    pub fn is_tls(self) -> bool {
        matches!(self, Scheme::Stuns | Scheme::Turns)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Stun => "stun",
            Scheme::Stuns => "stuns",
            Scheme::Turn => "turn",
            Scheme::Turns => "turns",
        }
    }
}

/// The transport a URL is actually reached over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Udp,
    Tcp,
    Tls,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IceUrl {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    /// Explicit `?transport=` (udp/tcp), if any.
    pub transport_param: Option<Transport>,
}

impl IceUrl {
    /// Transport used to reach this URL: TLS for stuns/turns, else the explicit
    /// `?transport=tcp`, else UDP.
    pub fn transport(&self) -> Transport {
        if self.scheme.is_tls() {
            Transport::Tls
        } else {
            self.transport_param.unwrap_or(Transport::Udp)
        }
    }

    /// Short label without the hostname, e.g. "turn/tcp:443" (safe for summaries).
    pub fn short_label(&self) -> String {
        let t = match self.transport() {
            Transport::Udp => "udp",
            Transport::Tcp => "tcp",
            Transport::Tls => "tls",
        };
        format!("{}/{}:{}", self.scheme.as_str(), t, self.port)
    }
}

impl fmt::Display for IceUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.scheme.as_str(), self.host, self.port)?;
        if let Some(t) = self.transport_param {
            write!(
                f,
                "?transport={}",
                if t == Transport::Tcp { "tcp" } else { "udp" }
            )?;
        }
        Ok(())
    }
}

/// One validated ICE server entry. `Debug` never prints the credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<IceUrl>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl fmt::Debug for IceServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IceServer")
            .field(
                "urls",
                &self
                    .urls
                    .iter()
                    .map(IceUrl::short_label)
                    .collect::<Vec<_>>(),
            )
            .field("username", &self.username.as_ref().map(|_| "<USERNAME>"))
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<CREDENTIAL>"),
            )
            .finish()
    }
}

impl IceServer {
    /// The WebRTC `RTCIceServer` JSON shape (for the bridge, Milestone 4).
    pub fn to_webrtc_json(&self) -> serde_json::Value {
        serde_json::json!({
            "urls": self.urls.iter().map(|u| u.to_string()).collect::<Vec<_>>(),
            "username": self.username,
            "credential": self.credential,
        })
    }
}

/// Why an entry was rejected. Messages never contain credential values.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("no ICE servers found")]
    Empty,
    #[error("too many ICE servers (max {MAX_SERVERS})")]
    TooMany,
    #[error("entry {0} has no urls")]
    NoUrls(usize),
    #[error("entry {0}: unsupported scheme (only stun, stuns, turn, turns)")]
    BadScheme(usize),
    #[error("entry {0}: malformed URL")]
    BadUrl(usize),
    #[error("entry {0}: a TURN server needs a username and a credential")]
    MissingCredential(usize),
    #[error("entry {0}: value too long")]
    TooLong(usize),
}

/// Parses `scheme:host[:port][?transport=udp|tcp]` (RFC 7064/7065; IPv6 in brackets).
pub fn parse_ice_url(s: &str, idx: usize) -> Result<IceUrl, ValidationError> {
    let s = s.trim();
    if s.len() > MAX_URL_LEN {
        return Err(ValidationError::TooLong(idx));
    }
    let (scheme_str, rest) = s.split_once(':').ok_or(ValidationError::BadUrl(idx))?;
    let scheme = match scheme_str.to_ascii_lowercase().as_str() {
        "stun" => Scheme::Stun,
        "stuns" => Scheme::Stuns,
        "turn" => Scheme::Turn,
        "turns" => Scheme::Turns,
        _ => return Err(ValidationError::BadScheme(idx)),
    };
    // No "//" authority form, no userinfo, no paths (RFC 7064/7065).
    if rest.starts_with("//") || rest.contains('@') || rest.contains('/') {
        return Err(ValidationError::BadUrl(idx));
    }
    let (hostport, query) = match rest.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (rest, None),
    };
    let transport_param = match query {
        None => None,
        Some(q) => match q.to_ascii_lowercase().as_str() {
            "transport=udp" => Some(Transport::Udp),
            "transport=tcp" => Some(Transport::Tcp),
            _ => return Err(ValidationError::BadUrl(idx)),
        },
    };
    if transport_param.is_some() && !scheme.is_turn() {
        return Err(ValidationError::BadUrl(idx));
    }
    let (host, port_str) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, after) = v6.split_once(']').ok_or(ValidationError::BadUrl(idx))?;
        (h.to_owned(), after.strip_prefix(':'))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), Some(p)),
            None => (hostport.to_owned(), None),
        }
    };
    let host_ok = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':'));
    if !host_ok {
        return Err(ValidationError::BadUrl(idx));
    }
    let default_port = if scheme.is_tls() { 5349 } else { 3478 };
    let port = match port_str {
        None => default_port,
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(ValidationError::BadUrl(idx))?,
    };
    Ok(IceUrl {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        transport_param,
    })
}

/// Distinct hostnames (for the plain settings file and the UI; never secrets).
pub fn hostnames(servers: &[IceServer]) -> Vec<String> {
    let mut v: Vec<String> = servers
        .iter()
        .flat_map(|s| s.urls.iter().map(|u| u.host.clone()))
        .collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc_uris() {
        let u = parse_ice_url("turn:relay.example.invalid:443?transport=tcp", 0).unwrap();
        assert_eq!(u.transport(), Transport::Tcp);
        assert_eq!(u.port, 443);
        assert_eq!(u.short_label(), "turn/tcp:443");
        let s = parse_ice_url("STUN:stun.example.invalid", 0).unwrap();
        assert_eq!(
            (s.scheme, s.port, s.transport()),
            (Scheme::Stun, 3478, Transport::Udp)
        );
        let t = parse_ice_url("turns:relay.example.invalid:443?transport=tcp", 0).unwrap();
        assert_eq!(t.transport(), Transport::Tls);
        let v6 = parse_ice_url("stun:[2001:db8::1]:3478", 0).unwrap();
        assert_eq!(v6.host, "2001:db8::1");
        assert_eq!(
            parse_ice_url("turns:x.example.invalid", 0).unwrap().port,
            5349
        );
    }

    #[test]
    fn rejects_bad_uris() {
        for bad in [
            "http://x.example.invalid",
            "turn://x.example.invalid",
            "turn:user@x.example.invalid",
            "turn:x.example.invalid:0",
            "turn:x.example.invalid:99999",
            "turn:x.example.invalid?transport=sctp",
            "stun:x.example.invalid?transport=tcp",
            "turn:",
            "turn:x example",
            "turn:x.example.invalid/path",
        ] {
            assert!(parse_ice_url(bad, 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn debug_never_shows_credentials() {
        let s = IceServer {
            urls: vec![parse_ice_url("turn:x.example.invalid:80", 0).unwrap()],
            username: Some("SECRET-USER".into()),
            credential: Some("SECRET-PASS".into()),
        };
        let d = format!("{s:?}");
        assert!(!d.contains("SECRET") && !d.contains("x.example.invalid"));
    }
}

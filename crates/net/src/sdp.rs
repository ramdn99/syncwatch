//! DTLS fingerprint extraction from SDP (bound into the SPAKE2 proof in Milestone 5).

/// Returns "SHA-256 AB:CD:..." (algorithm upper-cased, hex upper-cased) of the first
/// `a=fingerprint` line (session- or media-level).
pub fn dtls_fingerprint(sdp: &str) -> Option<String> {
    sdp.lines().find_map(|l| {
        let v = l.trim().strip_prefix("a=fingerprint:")?;
        let (alg, hex) = v.split_once(' ')?;
        let hex = hex.trim();
        let ok = !hex.is_empty()
            && hex
                .split(':')
                .all(|b| b.len() == 2 && b.chars().all(|c| c.is_ascii_hexdigit()));
        ok.then(|| format!("{} {}", alg.to_ascii_uppercase(), hex.to_ascii_uppercase()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_normalizes() {
        let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=fingerprint:sha-256 ab:cd:EF:01\r\n";
        assert_eq!(
            dtls_fingerprint(sdp).as_deref(),
            Some("SHA-256 AB:CD:EF:01")
        );
        assert_eq!(dtls_fingerprint("a=fingerprint:sha-256 zz:00"), None);
        assert_eq!(dtls_fingerprint("v=0"), None);
    }
}

/// Counts ICE candidate types in SDP or candidate lines: (host, srflx, relay, prflx). Types only,
/// never addresses (used for logs and for the "could not connect" reasons).
pub fn candidate_types(text: &str) -> (usize, usize, usize, usize) {
    let (mut host, mut srflx, mut relay, mut prflx) = (0, 0, 0, 0);
    for w in text.split("typ ").skip(1) {
        match w.split_whitespace().next() {
            Some("host") => host += 1,
            Some("srflx") => srflx += 1,
            Some("relay") => relay += 1,
            Some("prflx") => prflx += 1,
            _ => {}
        }
    }
    (host, srflx, relay, prflx)
}

#[cfg(test)]
mod candidate_type_tests {
    #[test]
    fn counts_types_without_addresses() {
        let sdp = "a=candidate:1 1 udp 1 192.0.2.1 5000 typ host\r\na=candidate:2 1 udp 1 198.51.100.2 6000 typ srflx raddr 0.0.0.0 rport 0\r\na=candidate:3 1 udp 1 203.0.113.3 7000 typ relay raddr 0.0.0.0 rport 0\r\n";
        assert_eq!(super::candidate_types(sdp), (1, 1, 1, 0));
        assert_eq!(super::candidate_types("v=0"), (0, 0, 0, 0));
    }
}

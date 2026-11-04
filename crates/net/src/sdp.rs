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

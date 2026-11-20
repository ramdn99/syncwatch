//! Torrent metadata from untrusted sources: a `.torrent` file the host picks, or metadata the
//! host offers members over the data channel. Parsed defensively with our own small bencode
//! reader (size cap, depth cap, node cap, file and tracker caps, UTF-8 paths, path validation)
//! BEFORE librqbit sees it, then re-encoded with only validated trackers. The SHA-1 of the raw
//! `info` dictionary must equal the announced infohash.

use crate::validate;
use librqbit_sha1_wrapper::{ISha1, Sha1};
use syncwatch_protocol::Infohash;

/// Default cap for torrent metadata (configurable in Settings, never above the protocol's).
pub const DEFAULT_META_CAP: usize = 5 * 1024 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_NODES: usize = 400_000;
pub const MAX_FILES: usize = 10_000;
pub const MAX_TRACKERS: usize = 32;
pub const MAX_TRACKER_LEN: usize = 512;
pub const MAX_PIECE_LEN: u64 = 64 * 1024 * 1024;
pub const MAX_WEB_SEEDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MetaError {
    #[error("the torrent metadata is larger than the limit")]
    TooLarge,
    #[error("the torrent file is not valid")]
    Malformed,
    #[error("the torrent file is nested too deeply")]
    TooDeep,
    #[error("the torrent file has too many entries")]
    TooMany,
    #[error("a file name in the torrent is not valid UTF-8")]
    NotUtf8,
    #[error("the torrent contains an unsafe file name or path")]
    UnsafePath,
    #[error("the metadata does not belong to this torrent (infohash mismatch)")]
    HashMismatch,
    #[error("the torrent has no files")]
    NoFiles,
}

#[derive(Debug)]
enum B<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<B<'a>>),
    /// key, value, value's raw byte range
    Dict(Vec<(&'a [u8], B<'a>, std::ops::Range<usize>)>),
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    nodes: usize,
}

impl<'a> Reader<'a> {
    fn value(&mut self, depth: usize) -> Result<B<'a>, MetaError> {
        if depth > MAX_DEPTH {
            return Err(MetaError::TooDeep);
        }
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(MetaError::TooMany);
        }
        match self
            .buf
            .get(self.pos)
            .copied()
            .ok_or(MetaError::Malformed)?
        {
            b'i' => {
                self.pos += 1;
                let end = self.find(b'e')?;
                let s = std::str::from_utf8(&self.buf[self.pos..end])
                    .map_err(|_| MetaError::Malformed)?;
                // Canonical integers only: no "+", no leading zeros, no "-0".
                if s.is_empty()
                    || s.starts_with('+')
                    || s == "-0"
                    || (s.len() > 1 && s.starts_with('0'))
                    || s.starts_with("-0")
                {
                    return Err(MetaError::Malformed);
                }
                let v = s.parse::<i64>().map_err(|_| MetaError::Malformed)?;
                self.pos = end + 1;
                Ok(B::Int(v))
            }
            b'l' => {
                self.pos += 1;
                let mut v = Vec::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    v.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(B::List(v))
            }
            b'd' => {
                self.pos += 1;
                let mut v: Vec<(&[u8], B, std::ops::Range<usize>)> = Vec::new();
                while self.buf.get(self.pos) != Some(&b'e') {
                    let B::Bytes(k) = self.bytes()? else {
                        return Err(MetaError::Malformed);
                    };
                    if v.last().is_some_and(|(prev, _, _)| *prev >= k) {
                        return Err(MetaError::Malformed); // keys must be sorted and unique
                    }
                    let start = self.pos;
                    let val = self.value(depth + 1)?;
                    v.push((k, val, start..self.pos));
                }
                self.pos += 1;
                Ok(B::Dict(v))
            }
            b'0'..=b'9' => self.bytes(),
            _ => Err(MetaError::Malformed),
        }
    }

    fn find(&self, c: u8) -> Result<usize, MetaError> {
        self.buf[self.pos..]
            .iter()
            .position(|b| *b == c)
            .map(|i| self.pos + i)
            .ok_or(MetaError::Malformed)
    }

    fn bytes(&mut self) -> Result<B<'a>, MetaError> {
        let colon = self.find(b':')?;
        let len_s =
            std::str::from_utf8(&self.buf[self.pos..colon]).map_err(|_| MetaError::Malformed)?;
        if len_s.is_empty() || len_s.len() > 10 || (len_s.len() > 1 && len_s.starts_with('0')) {
            return Err(MetaError::Malformed);
        }
        let len: usize = len_s.parse().map_err(|_| MetaError::Malformed)?;
        let start = colon + 1;
        let end = start.checked_add(len).ok_or(MetaError::Malformed)?;
        if end > self.buf.len() {
            return Err(MetaError::Malformed);
        }
        self.pos = end;
        Ok(B::Bytes(&self.buf[start..end]))
    }
}

fn get<'b, 'a>(d: &'b [(&'a [u8], B<'a>, std::ops::Range<usize>)], k: &str) -> Option<&'b B<'a>> {
    d.iter()
        .find(|(key, _, _)| *key == k.as_bytes())
        .map(|(_, v, _)| v)
}

fn text(b: &B) -> Result<String, MetaError> {
    match b {
        B::Bytes(s) => std::str::from_utf8(s)
            .map(str::to_owned)
            .map_err(|_| MetaError::NotUtf8),
        _ => Err(MetaError::Malformed),
    }
}

fn int(b: &B) -> Result<i64, MetaError> {
    match b {
        B::Int(v) => Ok(*v),
        _ => Err(MetaError::Malformed),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaFile {
    pub index: u32,
    /// "Name/dir/file.mkv" for multi-file torrents, "file.mkv" otherwise.
    pub path: String,
    pub len: u64,
    pub video: bool,
    pub padding: bool,
}

/// A validated torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentMeta {
    pub infohash: Infohash,
    pub name: String,
    pub files: Vec<MetaFile>,
    pub total_size: u64,
    pub piece_len: u64,
    /// Validated http/https/udp trackers (capped).
    pub trackers: Vec<String>,
    /// BEP 19 web seeds listed in the file (librqbit 9.0.1 cannot use them).
    pub web_seeds: usize,
    /// Validated web seed URLs (kept in the sanitized torrent so the count survives).
    web_seed_urls: Vec<String>,
    info_raw: Vec<u8>,
}

fn sha1(b: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(b);
    h.finish()
}

fn tracker_ok(t: &str) -> bool {
    t.len() <= MAX_TRACKER_LEN
        && url::Url::parse(t)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "udp") && u.host_str().is_some())
}

/// Parses and validates `.torrent` bytes (or offered metadata). `expect` = the announced
/// infohash the info dictionary must hash to.
pub fn inspect(
    bytes: &[u8],
    cap: usize,
    expect: Option<Infohash>,
) -> Result<TorrentMeta, MetaError> {
    if bytes.len() > cap {
        return Err(MetaError::TooLarge);
    }
    let mut r = Reader {
        buf: bytes,
        pos: 0,
        nodes: 0,
    };
    let top = r.value(0)?;
    if r.pos != bytes.len() {
        return Err(MetaError::Malformed); // trailing garbage
    }
    let B::Dict(top) = top else {
        return Err(MetaError::Malformed);
    };
    let (info, info_range) = top
        .iter()
        .find(|(k, _, _)| *k == b"info")
        .map(|(_, v, r)| (v, r.clone()))
        .ok_or(MetaError::Malformed)?;
    let B::Dict(info) = info else {
        return Err(MetaError::Malformed);
    };
    let info_raw = bytes[info_range].to_vec();
    let infohash = Infohash(sha1(&info_raw));
    if expect.is_some_and(|e| e != infohash) {
        return Err(MetaError::HashMismatch);
    }
    let name = text(get(info, "name").ok_or(MetaError::Malformed)?)?;
    validate::check_component(&name).map_err(|_| MetaError::UnsafePath)?;
    let piece_len = int(get(info, "piece length").ok_or(MetaError::Malformed)?)?;
    if piece_len <= 0 || piece_len as u64 > MAX_PIECE_LEN {
        return Err(MetaError::Malformed);
    }
    let pieces = match get(info, "pieces") {
        Some(B::Bytes(p)) if p.len() % 20 == 0 && !p.is_empty() => p.len() / 20,
        _ => return Err(MetaError::Malformed),
    };
    let mut files = Vec::new();
    match (get(info, "length"), get(info, "files")) {
        (Some(l), None) => {
            let len = int(l)?;
            if len < 0 {
                return Err(MetaError::Malformed);
            }
            files.push(MetaFile {
                index: 0,
                video: validate::is_video(&name),
                path: name.clone(),
                len: len as u64,
                padding: false,
            });
        }
        (None, Some(B::List(list))) => {
            if list.is_empty() {
                return Err(MetaError::NoFiles);
            }
            if list.len() > MAX_FILES {
                return Err(MetaError::TooMany);
            }
            for (i, f) in list.iter().enumerate() {
                let B::Dict(f) = f else {
                    return Err(MetaError::Malformed);
                };
                let len = int(get(f, "length").ok_or(MetaError::Malformed)?)?;
                if len < 0 {
                    return Err(MetaError::Malformed);
                }
                let padding = matches!(get(f, "attr"), Some(B::Bytes(a)) if a.contains(&b'p'));
                let path = get(f, "path.utf-8")
                    .or(get(f, "path"))
                    .ok_or(MetaError::Malformed)?;
                let B::List(parts) = path else {
                    return Err(MetaError::Malformed);
                };
                let mut comps = vec![name.clone()];
                for p in parts {
                    comps.push(text(p)?);
                }
                validate::check_path(&comps).map_err(|_| MetaError::UnsafePath)?;
                let path = comps.join("/");
                files.push(MetaFile {
                    index: i as u32,
                    video: !padding && validate::is_video(&path),
                    path,
                    len: len as u64,
                    padding,
                });
            }
        }
        _ => return Err(MetaError::Malformed),
    }
    let total_size: u64 = files
        .iter()
        .try_fold(0u64, |a, f| a.checked_add(f.len))
        .ok_or(MetaError::Malformed)?;
    // One hash per piece; one extra trailing hash is tolerated (librqbit's own creator writes
    // it for sizes that are exact piece multiples, and librqbit accepts it).
    let need = total_size.div_ceil(piece_len as u64);
    if total_size == 0 || !(pieces as u64 == need || pieces as u64 == need + 1) {
        return Err(MetaError::Malformed);
    }
    // Trackers: announce + announce-list, untrusted: only http/https/udp, capped.
    let mut trackers: Vec<String> = Vec::new();
    let mut push = |t: &B| {
        if let Ok(s) = text(t)
            && tracker_ok(&s)
            && !trackers.contains(&s)
            && trackers.len() < MAX_TRACKERS
        {
            trackers.push(s);
        }
    };
    if let Some(a) = get(&top, "announce") {
        push(a);
    }
    if let Some(B::List(tiers)) = get(&top, "announce-list") {
        for tier in tiers {
            if let B::List(ts) = tier {
                for t in ts {
                    push(t);
                }
            }
        }
    }
    let web_ok = |s: &str| {
        s.len() <= MAX_TRACKER_LEN
            && url::Url::parse(s)
                .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
    };
    let mut web_seed_urls: Vec<String> = Vec::new();
    match get(&top, "url-list") {
        Some(b @ B::Bytes(_)) => {
            if let Ok(s) = text(b)
                && web_ok(&s)
            {
                web_seed_urls.push(s);
            }
        }
        Some(B::List(l)) => {
            for b in l {
                if let Ok(s) = text(b)
                    && web_ok(&s)
                    && web_seed_urls.len() < MAX_WEB_SEEDS
                    && !web_seed_urls.contains(&s)
                {
                    web_seed_urls.push(s);
                }
            }
        }
        _ => {}
    }
    let web_seeds = web_seed_urls.len();
    Ok(TorrentMeta {
        infohash,
        name,
        files,
        total_size,
        piece_len: piece_len as u64,
        trackers,
        web_seeds,
        web_seed_urls,
        info_raw,
    })
}

fn bstr(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(s.len().to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(s);
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl TorrentMeta {
    /// The `.torrent` bytes we hand to librqbit and to members: the original info dictionary
    /// (same infohash) plus ONLY the validated trackers.
    pub fn sanitized(&self) -> Vec<u8> {
        let mut out = b"d".to_vec();
        if let Some(first) = self.trackers.first() {
            bstr(&mut out, b"announce");
            bstr(&mut out, first.as_bytes());
            bstr(&mut out, b"announce-list");
            out.push(b'l');
            for t in &self.trackers {
                out.push(b'l');
                bstr(&mut out, t.as_bytes());
                out.push(b'e');
            }
            out.push(b'e');
        }
        bstr(&mut out, b"info");
        out.extend_from_slice(&self.info_raw);
        if !self.web_seed_urls.is_empty() {
            bstr(&mut out, b"url-list");
            out.push(b'l');
            for w in &self.web_seed_urls {
                bstr(&mut out, w.as_bytes());
            }
            out.push(b'e');
        }
        out.push(b'e');
        out
    }

    /// A magnet for the room descriptor (trackers trimmed to stay under the protocol limit).
    pub fn magnet(&self) -> String {
        let mut m = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            self.infohash.to_hex(),
            pct(&self.name)
        );
        for t in &self.trackers {
            let add = format!("&tr={}", pct(t));
            if m.len() + add.len() > 4096 {
                break;
            }
            m.push_str(&add);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_single(name: &str, len: u64, piece: u64) -> Vec<u8> {
        let pieces = len.div_ceil(piece.max(1)) as usize;
        let mut out = b"d".to_vec();
        bstr(&mut out, b"length");
        out.extend_from_slice(format!("i{len}e").as_bytes());
        bstr(&mut out, b"name");
        bstr(&mut out, name.as_bytes());
        bstr(&mut out, b"piece length");
        out.extend_from_slice(format!("i{piece}e").as_bytes());
        bstr(&mut out, b"pieces");
        bstr(&mut out, &vec![7u8; pieces * 20]);
        out.push(b'e');
        out
    }

    fn info_multi(name: &str, files: &[(&[&str], u64)], piece: u64) -> Vec<u8> {
        let total: u64 = files.iter().map(|f| f.1).sum();
        let mut out = b"d".to_vec();
        bstr(&mut out, b"files");
        out.push(b'l');
        for (path, len) in files {
            out.push(b'd');
            bstr(&mut out, b"length");
            out.extend_from_slice(format!("i{len}e").as_bytes());
            bstr(&mut out, b"path");
            out.push(b'l');
            for p in *path {
                bstr(&mut out, p.as_bytes());
            }
            out.extend_from_slice(b"ee");
        }
        out.push(b'e');
        bstr(&mut out, b"name");
        bstr(&mut out, name.as_bytes());
        bstr(&mut out, b"piece length");
        out.extend_from_slice(format!("i{piece}e").as_bytes());
        bstr(&mut out, b"pieces");
        bstr(&mut out, &vec![7u8; total.div_ceil(piece) as usize * 20]);
        out.push(b'e');
        out
    }

    fn torrent(info: &[u8], trackers: &[&str]) -> Vec<u8> {
        let mut out = b"d".to_vec();
        if let Some(t) = trackers.first() {
            bstr(&mut out, b"announce");
            bstr(&mut out, t.as_bytes());
            bstr(&mut out, b"announce-list");
            out.push(b'l');
            for t in trackers {
                out.push(b'l');
                bstr(&mut out, t.as_bytes());
                out.push(b'e');
            }
            out.push(b'e');
        }
        bstr(&mut out, b"info");
        out.extend_from_slice(info);
        out.push(b'e');
        out
    }

    #[test]
    fn single_and_multi_file_torrents_parse_and_hash() {
        let info = info_single("movie.mkv", 1_000_000, 262_144);
        let t = torrent(&info, &["http://t.example/announce"]);
        let m = inspect(&t, DEFAULT_META_CAP, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.infohash, Infohash(sha1(&info)));
        assert_eq!(m.files.len(), 1);
        assert!(m.files[0].video);
        assert_eq!(m.trackers, vec!["http://t.example/announce".to_owned()]);
        // The announced infohash must match.
        assert_eq!(
            inspect(&t, DEFAULT_META_CAP, Some(m.infohash)).map(|x| x.infohash),
            Ok(m.infohash)
        );
        assert_eq!(
            inspect(&t, DEFAULT_META_CAP, Some(Infohash([1; 20]))).err(),
            Some(MetaError::HashMismatch)
        );
        let info = info_multi(
            "Pack",
            &[(&["a.mkv"], 500_000), (&["extras", "b.nfo"], 10)],
            262_144,
        );
        let m =
            inspect(&torrent(&info, &[]), DEFAULT_META_CAP, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            m.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["Pack/a.mkv", "Pack/extras/b.nfo"]
        );
        assert_eq!(m.total_size, 500_010);
    }

    #[test]
    fn one_extra_piece_hash_is_tolerated_two_are_not() {
        let make = |hashes: usize| {
            let mut out = b"d".to_vec();
            bstr(&mut out, b"length");
            out.extend_from_slice(b"i524288e");
            bstr(&mut out, b"name");
            bstr(&mut out, b"m.mkv");
            bstr(&mut out, b"piece length");
            out.extend_from_slice(b"i262144e");
            bstr(&mut out, b"pieces");
            bstr(&mut out, &vec![1u8; hashes * 20]);
            out.push(b'e');
            torrent(&out, &[])
        };
        assert!(inspect(&make(2), DEFAULT_META_CAP, None).is_ok());
        assert!(
            inspect(&make(3), DEFAULT_META_CAP, None).is_ok(),
            "librqbit-created"
        );
        assert!(inspect(&make(4), DEFAULT_META_CAP, None).is_err());
        assert!(inspect(&make(1), DEFAULT_META_CAP, None).is_err());
    }

    #[test]
    fn sanitized_keeps_the_infohash_and_drops_bad_trackers() {
        let info = info_single("m.mkv", 300_000, 262_144);
        let t = torrent(
            &info,
            &[
                "http://ok.example/a",
                "file:///etc/passwd",
                "javascript:x",
                "udp://u.example:80",
            ],
        );
        let m = inspect(&t, DEFAULT_META_CAP, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            m.trackers,
            vec![
                "http://ok.example/a".to_owned(),
                "udp://u.example:80".to_owned()
            ]
        );
        let s = m.sanitized();
        let again =
            inspect(&s, DEFAULT_META_CAP, Some(m.infohash)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again.trackers, m.trackers);
        // Web seeds: only http(s), capped, kept through sanitizing (the count drives the UI).
        let mut ws = torrent(&info, &[]);
        ws.pop();
        bstr(&mut ws, b"url-list");
        ws.push(b'l');
        for u in [
            "https://archive.example/a/",
            "ftp://x/",
            "http://b.example/",
        ] {
            bstr(&mut ws, u.as_bytes());
        }
        ws.extend_from_slice(b"ee");
        let w = inspect(&ws, DEFAULT_META_CAP, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(w.web_seeds, 2);
        assert_eq!(
            inspect(&w.sanitized(), DEFAULT_META_CAP, Some(w.infohash)).map(|x| x.web_seeds),
            Ok(2)
        );
        assert!(
            m.magnet()
                .starts_with(&format!("magnet:?xt=urn:btih:{}", m.infohash.to_hex()))
        );
        assert!(
            syncwatch_protocol::magnet::parse(&m.magnet()).is_ok_and(|p| p.infohash == m.infohash)
        );
    }

    /// Fuzz-style helper (Milestone 9 security review): xorshift64* and 1-4 random edits.
    fn fuzz_mutate(state: &mut u64, src: &[u8]) -> Vec<u8> {
        let mut next = || {
            *state ^= *state >> 12;
            *state ^= *state << 25;
            *state ^= *state >> 27;
            state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        };
        let mut v = src.to_vec();
        for _ in 0..=next() % 4 {
            let at = (next() % (v.len() as u64 + 1)) as usize;
            match next() % 5 {
                0 if at < v.len() => v[at] ^= 1 << (next() % 8),
                1 => v.insert(at, next() as u8),
                2 if at < v.len() => {
                    v.remove(at);
                }
                3 => v.truncate(at),
                4 if at < v.len() => {
                    v[at] = [0, b'e', b':', b'0', b'9', 0xff][(next() % 6) as usize]
                }
                _ => {}
            }
        }
        v
    }

    #[test]
    fn mutated_torrents_never_panic_and_stay_within_limits() {
        let n: usize = std::env::var("SYNCWATCH_FUZZ_ITERS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000);
        let single = torrent(
            &info_single("movie.mkv", 1_000_000, 262_144),
            &["http://t.example/announce", "udp://u.example:80"],
        );
        let multi = torrent(
            &info_multi(
                "Show",
                &[(&["S1", "e01.mkv"], 700_000), (&["S1", "e01.srt"], 9_000)],
                262_144,
            ),
            &[],
        );
        let mut state = 0x6d65_7461_0001_u64;
        let mut ok = 0;
        for i in 0..n {
            let m = fuzz_mutate(&mut state, if i % 2 == 0 { &single } else { &multi });
            if let Ok(meta) = inspect(&m, DEFAULT_META_CAP, None) {
                ok += 1;
                assert!(meta.files.len() <= MAX_FILES && meta.trackers.len() <= MAX_TRACKERS);
                for f in &meta.files {
                    let parts: Vec<&str> = f.path.split('/').collect();
                    assert!(crate::validate::check_path(&parts).is_ok(), "{:?}", f.path);
                }
                // The sanitized copy parses again with the same infohash.
                let again = inspect(&meta.sanitized(), DEFAULT_META_CAP, Some(meta.infohash));
                assert_eq!(again.map(|x| x.infohash), Ok(meta.infohash));
            }
        }
        assert!(ok > 0, "no mutation stayed valid: the seeds are wrong");
    }

    #[test]
    fn hostile_and_malformed_inputs_are_refused() {
        let ok = info_single("m.mkv", 300_000, 262_144);
        let cases: Vec<(Vec<u8>, MetaError)> = vec![
            (b"".to_vec(), MetaError::Malformed),
            (b"d4:info".to_vec(), MetaError::Malformed),
            (
                torrent(&ok, &[]).into_iter().chain(*b"junk").collect(),
                MetaError::Malformed,
            ),
            (b"di1e1:xe".to_vec(), MetaError::Malformed),
            (b"d1:b1:x1:a1:ye".to_vec(), MetaError::Malformed), // unsorted keys
            (b"d4:infoi-0ee".to_vec(), MetaError::Malformed),
            (b"d4:info99999999999:xe".to_vec(), MetaError::Malformed),
            (
                [b"l".repeat(100), b"e".repeat(100)].concat(),
                MetaError::TooDeep,
            ),
            (vec![0u8; DEFAULT_META_CAP + 1], MetaError::TooLarge),
            (
                torrent(&info_single("../evil.mkv", 300_000, 262_144), &[]),
                MetaError::UnsafePath,
            ),
            (
                torrent(&info_single("CON.mkv", 300_000, 262_144), &[]),
                MetaError::UnsafePath,
            ),
            (
                torrent(
                    &info_multi("P", &[(&["..", "x.mkv"], 300_000)], 262_144),
                    &[],
                ),
                MetaError::UnsafePath,
            ),
            (
                torrent(
                    &info_multi("P", &[(&["C:", "x.mkv"], 300_000)], 262_144),
                    &[],
                ),
                MetaError::UnsafePath,
            ),
            (
                torrent(
                    &info_multi("P", &[(&["a\u{7}.mkv"], 300_000)], 262_144),
                    &[],
                ),
                MetaError::UnsafePath,
            ),
            (
                torrent(&info_single("m.mkv", 10, 0), &[]),
                MetaError::Malformed,
            ),
        ];
        for (i, (bytes, want)) in cases.iter().enumerate() {
            assert_eq!(
                inspect(bytes, DEFAULT_META_CAP, None).err(),
                Some(*want),
                "case {i}"
            );
        }
        // Invalid UTF-8 path component.
        let mut bad = info_multi("P", &[(&["xx.mkv"], 300_000)], 262_144);
        let pos = bad.windows(6).position(|w| w == b"xx.mkv").unwrap_or(0);
        bad[pos] = 0xFF;
        assert_eq!(
            inspect(&torrent(&bad, &[]), DEFAULT_META_CAP, None).err(),
            Some(MetaError::NotUtf8)
        );
        // Wrong piece count.
        let mut wrong = info_single("m.mkv", 300_000, 262_144);
        let p = wrong.windows(6).position(|w| w == b"pieces").unwrap_or(0);
        wrong.splice(p + 6..p + 9, b"20:".iter().copied());
        assert!(inspect(&torrent(&wrong, &[]), DEFAULT_META_CAP, None).is_err());
        // Too many trackers are capped, not fatal.
        let many: Vec<String> = (0..100).map(|i| format!("http://t{i}.example/a")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let m =
            inspect(&torrent(&ok, &refs), DEFAULT_META_CAP, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.trackers.len(), MAX_TRACKERS);
        // Too many files.
        let files: Vec<(Vec<String>, u64)> = (0..MAX_FILES + 1)
            .map(|i| (vec![format!("f{i}.bin")], 1))
            .collect();
        let fref: Vec<(Vec<&str>, u64)> = files
            .iter()
            .map(|(p, l)| (p.iter().map(String::as_str).collect(), *l))
            .collect();
        let fslices: Vec<(&[&str], u64)> = fref.iter().map(|(p, l)| (p.as_slice(), *l)).collect();
        assert_eq!(
            inspect(
                &torrent(&info_multi("P", &fslices, 262_144), &[]),
                DEFAULT_META_CAP,
                None
            )
            .err(),
            Some(MetaError::TooMany)
        );
    }
}

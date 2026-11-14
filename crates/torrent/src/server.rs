//! Loopback HTTP server that feeds mpv (spec section 10): 127.0.0.1, random port, a 128-bit
//! token in the path, single-range requests. While downloading it reads through the torrent
//! stream; after completion it serves the FINISHED file from the SAME URL. An open response
//! switches source at its current offset, so the handoff needs no reload and no seek.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt};
use tokio::sync::{oneshot, watch};

pub type Reader = Box<dyn AsyncRead + Send + Unpin>;

/// Opens the torrent stream at a byte offset (the engine implements it with librqbit).
pub trait StreamOpener: Send + Sync {
    fn open_at(&self, pos: u64) -> BoxFuture<'static, std::io::Result<Reader>>;
}

#[derive(Clone)]
enum Source {
    Stream(Arc<dyn StreamOpener>),
    File(PathBuf),
}

#[derive(Clone)]
struct Slot {
    len: u64,
    source: Source,
    generation: u64,
}

/// What the player is reading: the request's start (`new_request`) and progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadReport {
    pub pos: u64,
    pub new_request: bool,
    pub seq: u64,
    /// Response bodies still being served: the player is still reading (or waiting) there.
    pub open: u32,
}

/// One player request as served: [start, end) read so far; `open` while it is being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadSpan {
    /// Request order (increasing).
    pub seq: u64,
    pub start: u64,
    pub end: u64,
    pub open: bool,
}

/// Bytes a request must read before it counts as playback (an index read is shorter).
pub const PLAYBACK_SPAN_MIN: u64 = 1024 * 1024;

/// Where playback resumed after a seek: the start of the first request made after the seek
/// (`seq >= floor`) that read at least [`PLAYBACK_SPAN_MIN`]. mpv's first request after a
/// seek is often the index (MKV Cues at the end), never a time -> byte sample (ADR 0012).
pub fn playback_start_after(spans: &[ReadSpan], floor: u64) -> Option<u64> {
    spans
        .iter()
        .filter(|s| s.seq >= floor && s.end.saturating_sub(s.start) >= PLAYBACK_SPAN_MIN)
        .min_by_key(|s| s.seq)
        .map(|s| s.start)
}

/// Every request is kept (bounded) until the engine takes it: the engine ticks every 200 ms,
/// while mpv's container reads at open (header, then index, then the position) take
/// milliseconds each.
const MAX_SPANS: usize = 64;

#[derive(Clone)]
struct Shared {
    token: String,
    port: u16,
    slot: Arc<RwLock<Option<Slot>>>,
    reads: Arc<watch::Sender<ReadReport>>,
    spans: Arc<std::sync::Mutex<Vec<(u64, ReadSpan)>>>,
    next_span: Arc<std::sync::atomic::AtomicU64>,
}

impl Shared {
    fn span_update(&self, id: u64, f: impl FnOnce(&mut ReadSpan)) {
        if let Ok(mut g) = self.spans.lock()
            && let Some((_, s)) = g.iter_mut().find(|(i, _)| *i == id)
        {
            f(s);
        }
    }
}

pub struct Loopback {
    shared: Shared,
    stop: Option<oneshot::Sender<()>>,
}

const CHUNK: usize = 256 * 1024;

fn new_token() -> String {
    let mut b = [0u8; 16];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Parses a single `Range: bytes=` header against `len`: Ok(None) = whole file,
/// Err(RangeError) = unsatisfiable or unsupported (multi-range).
/// An unsatisfiable or unsupported Range header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeError;

pub fn parse_range(h: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, RangeError> {
    let Some(h) = h else { return Ok(None) };
    let spec = h.trim().strip_prefix("bytes=").ok_or(RangeError)?;
    if spec.contains(',') {
        return Err(RangeError);
    }
    let (a, b) = spec.split_once('-').ok_or(RangeError)?;
    let (a, b) = (a.trim(), b.trim());
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().map_err(|_| RangeError)?;
        if n == 0 {
            return Err(RangeError);
        }
        (len.saturating_sub(n), len)
    } else {
        let s: u64 = a.parse().map_err(|_| RangeError)?;
        let e = if b.is_empty() {
            len
        } else {
            b.parse::<u64>()
                .map_err(|_| RangeError)?
                .saturating_add(1)
                .min(len)
        };
        (s, e)
    };
    if start >= len || start >= end {
        return Err(RangeError);
    }
    Ok(Some((start, end)))
}

fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn open(source: &Source, pos: u64) -> std::io::Result<Reader> {
    match source {
        Source::Stream(o) => o.open_at(pos).await,
        Source::File(p) => {
            let mut f = tokio::fs::File::open(p).await?;
            f.seek(std::io::SeekFrom::Start(pos)).await?;
            Ok(Box::new(f))
        }
    }
}

async fn media(
    State(sh): State<Shared>,
    AxPath(token): AxPath<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    if !same_secret(&token, &sh.token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Only the loopback host (defence against DNS rebinding in addition to the token).
    let host_ok = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_none_or(|h| h == format!("127.0.0.1:{}", sh.port));
    if !host_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(slot) = sh.slot.read().ok().and_then(|g| g.clone()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = slot.len;
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, partial) = match parse_range(range, len) {
        Ok(Some((s, e))) => (s, e, true),
        Ok(None) => (0, len, false),
        Err(RangeError) => {
            let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let mut resp = if method == Method::HEAD {
        Response::new(Body::empty())
    } else {
        sh.reads.send_modify(|r| {
            r.pos = start;
            r.new_request = true;
            r.seq += 1;
        });
        Response::new(body(sh.clone(), start, end))
    };
    *resp.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = resp.headers_mut();
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&(end - start).to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if partial && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{len}", end - 1)) {
        h.insert(header::CONTENT_RANGE, v);
    }
    resp
}

struct BodyState {
    sh: Shared,
    offset: u64,
    end: u64,
    reader: Option<(Reader, u64)>,
    span: u64,
}

impl Drop for BodyState {
    fn drop(&mut self) {
        // The response ended or the player closed the connection: it no longer waits here.
        self.sh.reads.send_modify(|r| {
            r.open = r.open.saturating_sub(1);
            r.seq += 1;
        });
        self.sh.span_update(self.span, |s| s.open = false);
    }
}

fn body(sh: Shared, start: u64, end: u64) -> Body {
    sh.reads.send_modify(|r| r.open += 1);
    let span = sh
        .next_span
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = sh.spans.lock() {
        if g.len() >= MAX_SPANS {
            // Keep the newest; drop the oldest finished one (or the oldest).
            let i = g.iter().position(|(_, s)| !s.open).unwrap_or(0);
            g.remove(i);
        }
        g.push((
            span,
            ReadSpan {
                seq: span,
                start,
                end: start,
                open: true,
            },
        ));
    }
    let st = BodyState {
        sh,
        offset: start,
        end,
        reader: None,
        span,
    };
    let s = futures_util::stream::unfold(st, |mut st| async move {
        if st.offset >= st.end {
            return None;
        }
        let slot = st.sh.slot.read().ok().and_then(|g| g.clone())?;
        // A new source generation (the finished file after completion): reopen at the SAME
        // offset; mpv never notices.
        if st
            .reader
            .as_ref()
            .is_none_or(|(_, g)| *g != slot.generation)
        {
            match open(&slot.source, st.offset).await {
                Ok(r) => st.reader = Some((r, slot.generation)),
                Err(e) => return Some((Err(e), st)),
            }
        }
        let want = CHUNK.min((st.end - st.offset) as usize);
        let mut buf = vec![0u8; want];
        let (r, _) = st.reader.as_mut()?;
        match r.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                st.offset += n as u64;
                let pos = st.offset;
                st.sh.span_update(st.span, |s| s.end = pos);
                st.sh.reads.send_modify(|r| {
                    r.pos = pos;
                    r.new_request = false;
                    r.seq += 1;
                });
                Some((Ok(Bytes::from(buf)), st))
            }
            Err(e) => {
                // The stream source can fail once the torrent is released: retry from the
                // (possibly new) source on the next poll.
                st.reader = None;
                let gen_changed = st
                    .sh
                    .slot
                    .read()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| s.generation))
                    != Some(slot.generation);
                if gen_changed {
                    Some((Ok(Bytes::new()), st))
                } else {
                    Some((Err(e), st))
                }
            }
        }
    });
    Body::from_stream(s)
}

impl Loopback {
    /// Starts on 127.0.0.1 with a random port.
    pub async fn start() -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let port = listener.local_addr()?.port();
        let (reads, _) = watch::channel(ReadReport::default());
        let shared = Shared {
            token: new_token(),
            port,
            slot: Arc::new(RwLock::new(None)),
            reads: Arc::new(reads),
            spans: Arc::new(std::sync::Mutex::new(Vec::new())),
            next_span: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let app = Router::new()
            .route("/{token}/media", axum::routing::get(media).head(media))
            .with_state(shared.clone());
        let (tx, rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Ok(Self {
            shared,
            stop: Some(tx),
        })
    }

    /// The URL mpv loads (contains the token: never log it).
    pub fn url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/{}/media",
            self.shared.port, self.shared.token
        )
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    pub fn reads(&self) -> watch::Receiver<ReadReport> {
        self.shared.reads.subscribe()
    }

    /// The sequence number the next request will get.
    pub fn next_span_seq(&self) -> u64 {
        self.shared
            .next_span
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The finished requests since the last call (removed), and a copy of the open ones.
    pub fn take_spans(&self) -> (Vec<ReadSpan>, Vec<ReadSpan>) {
        let Ok(mut g) = self.shared.spans.lock() else {
            return (Vec::new(), Vec::new());
        };
        let (done, open): (Vec<_>, Vec<_>) = g.drain(..).partition(|(_, s)| !s.open);
        let open_copy = open.iter().map(|(_, s)| *s).collect();
        *g = open;
        (done.into_iter().map(|(_, s)| s).collect(), open_copy)
    }

    fn set(&self, len: u64, source: Source) {
        if let Ok(mut g) = self.shared.slot.write() {
            let generation = g.as_ref().map(|s| s.generation + 1).unwrap_or(1);
            *g = Some(Slot {
                len,
                source,
                generation,
            });
        }
    }

    pub fn set_stream(&self, len: u64, opener: Arc<dyn StreamOpener>) {
        self.set(len, Source::Stream(opener));
    }

    /// The finished file (same URL; open responses switch at their offset).
    pub fn set_file(&self, len: u64, path: PathBuf) {
        self.set(len, Source::File(path));
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.shared.slot.write() {
            *g = None;
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_sample_comes_from_playback_never_from_the_index_read() {
        let s = |seq, start, len, open| ReadSpan {
            seq,
            start,
            end: start + len,
            open,
        };
        let spans = [
            s(3, 900, 5_000_000, false),       // before the seek: ignored
            s(5, 77_400_000, 18_000, false),   // MKV Cues read at the end
            s(6, 61_000_000, 2_000_000, true), // playback at the new position
        ];
        assert_eq!(playback_start_after(&spans, 5), Some(61_000_000));
        // Only the index read so far: no sample yet.
        assert_eq!(playback_start_after(&spans[..2], 5), None);
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-"), 100), Ok(Some((0, 100))));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Ok(Some((10, 20))));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Ok(Some((90, 100))));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Ok(Some((90, 100))));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Err(RangeError));
        assert_eq!(parse_range(Some("bytes=5-1"), 100), Err(RangeError));
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Err(RangeError));
        assert_eq!(parse_range(Some("items=0-1"), 100), Err(RangeError));
        assert_eq!(parse_range(Some("bytes=-0"), 100), Err(RangeError));
    }

    #[test]
    fn tokens_are_128_bit_and_compared_safely() {
        let t = new_token();
        assert_eq!(t.len(), 32);
        assert_ne!(t, new_token());
        assert!(same_secret(&t, &t.clone()));
        assert!(!same_secret(&t, &new_token()));
        assert!(!same_secret(&t, "short"));
    }
}

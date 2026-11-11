# ADR 0009: Torrent engine and piece prioritization

Status: proposed (Milestone 7, before implementation). Date: 2026-11-06.
Source read: librqbit 9.0.1 (crates.io, 2026-08-20), files `torrent_state/streaming.rs`,
`piece_tracker.rs`, `chunk_tracker.rs`, `file_info.rs`, `torrent_state/live/mod.rs` and
`session.rs`, plus reqwest 0.13.5 and librqbit-sha1-wrapper 9.0.1.

## Engine version and build (rustls only)

- **Version: librqbit 9.0.1**, with every `librqbit-*` sub-crate pinned to `=9.0.1`. librqbit
  9.1.0 (2026-10-29) is less than two weeks old. Without the pins, Cargo resolves the
  sub-crates to 9.1.0 even under librqbit 9.0.1, because they depend on each other with `^`.
- **librqbit's own `rust-tls` feature cannot be used.** It turns on `reqwest/rustls`, which in
  reqwest 0.13 brings in **aws-lc-rs**. Its `sha1-ring` feature is misnamed: it also uses
  aws-lc-rs (`sha1-ring = ["aws-lc-rs"]`). Both conflict with ADR 0004 (ring only).
- **What works (verified with a probe build and `cargo tree`):**
  - `librqbit` with `default-features = false`;
  - our own direct dependency `reqwest = { version = "=0.13.5", default-features = false,
    features = ["rustls-no-provider"] }`. Feature unification gives librqbit's HTTP client
    rustls without a built-in provider;
  - `rustls::crypto::ring::default_provider().install_default()` at startup, before the engine
    starts. reqwest picks up the process default; without it, reqwest panics on purpose;
  - SHA-1 stays on librqbit's default `sha1-crypto-hash`. On Windows the `crypto-hash` crate
    uses the system CryptoAPI (advapi32), so neither OpenSSL nor aws-lc is involved;
  - result for the Windows target: `cargo tree -i aws-lc-rs`, `-i aws-lc-sys` and `-i native-tls`
    match nothing, `-i openssl` prints nothing, and ring 0.17 is the only crypto. OpenSSL still
    appears in `Cargo.lock`, because crypto-hash uses it on Linux. It is never built for
    Windows. The CI checks stay as they are and gain `aws-lc-rs`.
- **In process, no binary:** librqbit is a library on our tokio runtime. The engine is created
  only when a torrent is needed and destroyed afterwards (ADR 0010).

## What librqbit offers for ordering

1. **Streams first.** `ManagedTorrent::stream(file_id)` returns a `FileStream` (AsyncRead +
   AsyncSeek). Each open stream asks for the pieces from its position to position + 32 MiB.
   That window is the private constant `PER_STREAM_BUF_DEFAULT` and cannot be configured. A
   peer looking for work takes the first such piece it has that is neither downloaded nor in
   flight.
2. **Streams interleave fairly, not by priority.** With several streams, librqbit takes one
   piece from each in turn, in a freshly **shuffled** order every time. There is no API for a
   priority between streams.
3. **A seek re-targets at once.** `start_seek` only moves the stream's position, so the next piece
   request already follows the new window. A stream does not need to be read to count: moving an
   unread stream with `seek()` is enough.
4. **Everything else is the background order:** the selected files sorted by name. In each file
   the first piece comes first, then the **last** piece, then the rest in order from the start.
   It never starts from the playhead.
5. **File selection:** `only_files` (on add, or `update_only_files` later) limits downloading to
   the chosen files. Pieces on a file boundary are shared with the neighbouring file, so their
   bytes for that neighbour are written too.
6. Slow peers: a piece is stolen from a peer 10x (first pass) or 3x (last pass) slower than the
   asking peer. This is end-game behaviour; we keep it.

## Decision: a pure PiecePolicy driving librqbit streams

`crates/torrent` has a pure, unit-tested `PiecePolicy`: (clock, inputs) -> the desired set of
**byte cursors**. The adapter turns each cursor into one `FileStream` that it only seeks, never
reads (except the playback stream, which the loopback server reads for mpv).

| Spec claim (section 10) | Can librqbit do it? | What we do |
|---|---|---|
| Only the selected file's pieces | Yes (`only_files`) | Add with `only_files = [index]`. Boundary pieces write a few bytes into neighbour files: the adapter deletes those files when the torrent is released (ADR 0010) |
| File head first | Yes: the first piece of the file comes first, and the playback stream starts at 0 | The playback stream |
| File tail first (MKV Cues, MP4 moov) | Partly: only the LAST piece comes early | A **tail cursor** at `len - tail_bytes` (default 8 MiB, at least 2 pieces), dropped when that range is complete. Because of the fixed 32 MiB window, it covers through to the end of the file |
| Pre-buffer before the first play | No concept | Policy: the playback window must hold `prebuffer_bytes` (bitrate x 10 s, or a byte fallback) before the member reports Ready to the gate |
| Current window and read-ahead at the highest priority | Only as one of several equal streams, shuffled | **Exclusive mode:** while the playback window is short of its target, the policy keeps ONLY the playback cursor (the tail and fill cursors are dropped). It brings them back with hysteresis once the window holds its target again. Strict priority, built from what exists |
| Re-prioritize on every seek | Yes, natively (seek moves the window) | mpv's range request after a seek moves the playback stream. On a room-wide seek every member also moves its cursor at once, before mpv asks |
| Hysteresis for rapid seeks | No | Policy: auxiliary cursors return only after the window has held its target for a calm period (default 2 s), and an old position is never re-targeted inside it. Several seeks in a row cost no extra streams |
| Background fill from the playhead, wrap-around | **No:** the background goes from the file start | A **fill cursor**: the first missing piece after the playback window, wrapping to the first missing piece from 0. The adapter moves it with `seek()` as pieces complete. librqbit's own background order picks up whatever the cursors don't cover |
| VBR-aware mapping, byte-offset fallback | Not librqbit's job (bytes only) | The loopback server sees mpv's real byte range requests, so the read position is exact and needs no mapping. A mapping is needed only to pre-target a room seek to time T before mpv asks. The policy uses mpv's demuxer bitrate when known, else `T / duration x size` |
| Read-ahead size | Fixed at 32 MiB per stream | A larger read-ahead uses a second playback-region cursor at +32 MiB; a smaller one is not possible (documented) |

**Rejected:** forking librqbit to add per-piece priorities. It would give finer control, but we
would carry a fork of a fast-moving crate. The cursor design covers every claim except "a
read-ahead smaller than 32 MiB", which we do not need.

## Seeking into a missing region

The playback stream's `poll_read` returns `Pending` until its piece is verified, so the loopback
server simply holds the response. The policy reports Buffering (target bytes and progress) to
the ready gate, and the member pauses locally. mpv gets a long `network-timeout` with
`cache-pause`, so it waits and never errors. When the window is satisfied, the member reports
ready and resumes in sync. If the swarm stalls, already-downloaded data keeps playing, and the
state becomes "waiting for peers" with a periodic retry (a new peer stream: reconnect).

## Tests

- **Unit (PiecePolicy, pure):**
  - the cursors on start, the tail cursor until complete, exclusive mode while short;
  - hysteresis on rapid seeks;
  - wrap-around fill;
  - the bitrate mapping and the byte fallback;
  - property tests: cursors always inside the file, never more than 4.
- **Integration:** in-process seeder and leecher (two librqbit sessions on 127.0.0.1, no DHT, a
  local test tracker). Checks: the head and tail ranges complete before the middle; after a
  seek, the target window completes before the old one; the fill wraps around.

## Approval conditions (2026-11-11)

- **The playback-window workaround is proven with a throttled seeder.** Neither the tail cursor
  nor the background fill may cause a stall while playback is short. The `PiecePolicy` stays a
  pure, unit-tested function. The stream juggling sits behind a small adapter trait
  (`StreamDriver`), so the adapter is tested with a fake driver and with real librqbit.
- **TLS provider:**
  - the ring provider is installed exactly once, at startup, in one function
    (`tls::install_ring_provider`);
  - a test proves that a second install cannot happen: the function is idempotent and the
    rustls default stays ring.
- **Windows dependency tree:**
  - `cargo tree --target x86_64-pc-windows-msvc -i <crate>` must match nothing for aws-lc-rs,
    aws-lc-sys, native-tls, openssl and openssl-sys;
  - this check runs in `scripts/check-tls-tree.ps1` and the CI script, and `deny.toml` bans
    these crates;
  - the Linux-only OpenSSL entry in `Cargo.lock` is documented in `docs/build-guide.md`.
- **Re-checks after the TLS change:**
  - one live Metered credentials fetch plus a connection test (`live_probe`: 1 API request,
    allocations released at once);
  - the signaling client tests;
  - `cargo audit` on the pinned versions;
  - the upgrade procedure is documented in the build guide.

## Results (Milestone 7)

- **Unit tests:** 25 in `crates/torrent`. They cover the policy (seek to the middle, rapid
  repeated seeks, seek back, tail-first, wrap-around fill, a late joiner fetching the head,
  property tests), the juggler with a fake driver, the range parser, validation, the cache and
  the announce encoding.
- **Throttled seeder** (`tests/playback_window.rs`, seeder limited to 1.5 MiB/s, the player
  reads at 0.7 MiB/s):
  - exclusive window: prebuffer after a seek in 2.09 s, **200 ms** of stall;
  - control (tail and fill compete): 2.38 s, **1.95 s** of stall.
- **Two windows with qBittorrent** (`scripts/dev/torrent-lab.ps1`, 700 KiB/s):
  - both windows play from 22% downloaded;
  - a seek to 50:00 (not downloaded) holds the room within 34 ms (gate, "loading" plus our
    Buffering), and both resume in sync 9.5 s later (positions within 28 ms);
  - a seek back takes 374-386 ms;
  - positions agree within 118-188 ms while downloading.
- **Lab detail:** everything runs on 127.0.0.1, so qBittorrent must allow several connections
  from one IP (`enable_multi_connections_from_same_ip`). Otherwise libtorrent drops our
  handshake. This applies to the lab only.

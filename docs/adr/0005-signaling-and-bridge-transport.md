# ADR 0005: Signaling server and the WebView2 bridge transport

Status: accepted (Milestone 4). Date: 2026-11-04.

## Context

Spec section 1 fixes WebRTC data channels inside a hidden WebView2 window (the "bridge"), with no
webrtc-rs because it lacks TURN over TCP/TLS. The spec also requires a small signaling server.
Known pitfalls from the earlier build (spec section 2):
- background timer throttling caused false "connection interrupted" banners;
- a hung bridge page was not detected;
- joins took far too long;
- signaling errors were opaque;
- base64 IPC cost throughput.

## Decisions

### Signaling (`crates/signaling-server`, `crates/protocol/src/signaling.rs`)
- axum 0.8.9 WebSocket at `/ws` with JSON messages tagged by `t`, plus `/healthz`. The protocol
  version is checked in `Hello`.
- Room codes have 8 characters from `23456789ABCDEFGHJKMNPQRSTUVWXYZ` (no 0/O/1/I/L). The host gets a
  resume token, compared in constant time. The host has a 30 s grace period to come back before the
  room closes.
- Members talk only to the host (star topology). The server routes `Signal` between them and never
  inspects SDP beyond size limits.
- Abuse limits:
  - 32 connections per IP;
  - token bucket of 50 msg/s per connection (burst 200);
  - 32 KiB frames, 16 KiB SDP;
  - 60 s idle timeout (the client pings every 20 s);
  - room cap of 12 members.
- Non-root Docker image pinned by digest. Run it behind Caddy for TLS (deploy/README.md).
- Client (`crates/net/src/signaling.rs`): tokio-tungstenite 0.30 with our own rustls ClientConfig
  (ring provider + rustls-platform-verifier). That means no webpki-roots, aws-lc, native-tls or
  openssl.
  - DNS, TCP and the handshake run as separate steps, so the error says which host failed and why
    (DNS / refused / timed out / TLS / HTTP status / version). It never includes credentials.
  - `ws://` is accepted only for localhost.
  - The default URL is a placeholder (`wss://signal.example.invalid/ws`) until deployment.
    `SYNCWATCH_SIGNALING_URL` overrides it and the URL in use is logged at startup.

### Bridge transport
- One hidden window (`visible(false)`, no taskbar entry, 1x1) loads `bridge.html`, a second Vite
  entry. The page owns the RTCPeerConnections and has **no timers and no timing logic**.
- Two negotiated data channels per peer: id 0 reliable+ordered, id 1 `ordered:false,
  maxRetransmits:0` (keepalives, clock pings). The member is the offerer. Trickle ICE sends every
  candidate as soon as it is found, then `endOfCandidates`. `iceTransportPolicy: relay` applies when
  "always use relay" is on.
- **Binary IPC, no base64.** Rust to page: a Tauri `Channel` with raw bodies. Page to Rust: a raw-body
  `invoke`. Frames are `[0x01][peer u32 LE][lane][payload]` (data) or `[0x02][json]` (commands and
  events).
- **Batching** (`[0x03]([len][frame])*`) in both directions. Rust has a writer task that coalesces
  everything queued while it was busy. The page keeps one invoke in flight and sends what queued up
  meanwhile. Batching comes from backpressure, not a timer. It raised bulk throughput from about
  1 MB/s to 13-14 MB/s (table below).
- **Flow control by credit.** Rust counts the wire bytes it queued per peer. After each batch the
  page reports the cumulative bytes it accepted plus `bufferedAmount`. Bulk senders wait while
  `queued - accepted + buffered >= 4 MiB`. The first version reset a counter on each report and could
  stall.
- **Anti-throttling.** Every webview gets the same `additionalBrowserArgs`, because WebView2 shares
  one browser process and mismatched arguments fail:
  - wry's defaults: `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection`;
  - `--disable-background-timer-throttling`;
  - `--disable-renderer-backgrounding`;
  - `--disable-backgrounding-occluded-windows`.

  A unit test keeps `bridge::BROWSER_ARGS` and tauri.conf.json identical. On top of that, every
  timer runs in Rust (tokio), which WebView2 cannot throttle at all.
- **Keepalives and watchdog (pure, `crates/net/src/watchdog.rs`).**
  - Rust sends a keepalive on the unreliable lane every 1 s.
  - "Interrupted" means no inbound frame of any kind for 4 s, and it clears on the next frame.
  - After 10 s of silence the peer is restarted: the member re-offers and the host answers every
    offer with a fresh connection.
  - Rust pings the page every 1 s. No pong for 5 s means the page is hung: the window is destroyed
    and rebuilt under a new label (`bridge-N`, because `destroy()` completes asynchronously), and
    every peer is renegotiated.
  - Only the current bridge label may call `bridge_attach` / `bridge_in`.
- `PeerOpen` carries the local and remote DTLS fingerprints (from the SDP) for the SPAKE2 binding
  in Milestone 5, and whether the selected pair is relayed (from `getStats`).

## Measurements

All runs: Windows 11, both instances on this machine, WebView2 156. `scripts/dev/net-lab.ps1`
with `-Release` (optimized, debug assertions on). Direct = host candidates only.

| Test | Result |
|---|---|
| Join, direct (create peer to both channels open) | 580-680 ms (6 runs) |
| Join, relay-only via local coturn 4.18 (UDP) | 655 ms, `relayed=true` |
| Join, relay-only via Metered (static ICE list, all 5 URLs) | 2.6-2.8 s, `relayed=true` (self-test limit 5 s) |
| RTT over the reliable lane, direct / coturn / Metered (p50) | 3.4 ms / 5.8 ms / 225 ms (p95 316 ms) |
| Bulk, direct, debug build, no batching | about 1 MB/s |
| Bulk, direct, debug build, batched | 9.3 MB/s |
| Bulk, direct, optimized, batched, 16 KiB chunks | 13.1-14.2 MB/s (256 MiB) |
| Bulk, direct, optimized, batched, 60 KiB chunks | 13.9 MB/s: per-byte cost, not per message |
| Raw data channel inside one page (no IPC, both ends on one renderer) | 23.0 MB/s |
| Bulk, relay-only via local coturn | 8.1 MB/s (64 MiB) |
| Keepalive worst gap, 25 s runs, foreground | 1.25-1.26 s (1 s cadence) |
| Silent link (`-DropAt 10`) | interrupted after 4.0 s, restart at 10 s, new link open 17 ms later |
| Hung page (`-FreezeAt 10`) | detected after 5.0 s, new page ready +150 ms, link open +44 ms (about 5.2 s total) |
| 13 min minimized (both windows), relay via coturn | 786 s: keepalive worst gap 1.27 s, 0 interrupts, 0 restarts, join 1.02 s |
| Bulk while minimized, relay via coturn, idle machine | 9.0 MB/s (same as foreground: no throttling) |

The 13-minute run's own bulk step measured 1.7 MB/s because a full clippy and test build ran on the
same machine at that moment. The idle rerun above is the representative figure.

Notes:
- Throughput is about 60% of the in-page WebRTC ceiling. The remaining cost is Tauri's IPC: every
  raw Channel message larger than 1 KiB is delivered with an `eval` plus a `fetch` round trip, and
  every byte crosses the IPC twice (sender and receiver). That is enough for this app: the bridge
  carries control, chat, subtitle and other small files; torrent data uses librqbit's own sockets
  (Milestone 7).
- The earlier build's 26 MB/s figure was measured differently and is not directly comparable.
  The in-page ceiling on this machine is 23 MB/s.
- Metered quota used in this milestone: 1 short run (10 s, relay-only).
  - 0 API requests (static ICE list).
  - At most 8 TURN allocations (4 TURN URLs x 2 peers), closed cleanly at the end.
  - Well under 1 MB relayed: DTLS/SCTP setup, about 50 RTT probes, keepalives. The 1 MiB bulk
    step did not start inside the 10 s window.

## Consequences
- Rooms (Milestone 5) consume `TransportEvent`s and drive `PeerTransport`. The SPAKE2 proof binds
  the fingerprints from `PeerInfo`.
- If more throughput is ever needed: shared memory between Rust and the page
  (`ICoreWebView2SharedBuffer`) or several bulk channels. Neither is needed now.

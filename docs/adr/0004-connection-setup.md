# ADR 0004: Connection setup (Metered credentials, connection test, secure storage)

- Status: Accepted (Milestone 3)
- Spec: section 5 (binding), section 1 (Metered help text, error detection), section 11

## Decision

### Crate `crates/ice-config`
- **TLS stack:** rustls only, with the **ring** crypto provider (`reqwest` with
  `rustls-no-provider`, `tokio-rustls` with `ring`). Certificates are verified by
  `rustls-platform-verifier` (the Windows certificate store, present on clean machines).
  reqwest 0.13's default `rustls` feature pulls in aws-lc-rs, a C build (CMake/NASM) linked
  against our static CRT; ring avoids that risk. `cargo tree` confirms there is no openssl,
  native-tls or aws-lc.
- **Fetch (Tab A):** one allowlist, `metered::is_allowed_host`, used for the first request AND
  every redirect hop (max 3). It accepts HTTPS only, `<app>.metered.live` (pinned to the user's
  app) or `*.metered.ca`, with no userinfo and no port other than 443, so look-alike hosts
  (`myapp.metered.live.evil`, `evilmetered.ca`, punycode) are rejected. There is no proxy, an
  8 s total / 5 s connect timeout and a 64 KiB body cap. Errors are classified by **body text
  first** (`Invalid API Key`), then 401/403, then transport (DNS, refused, timeout, TLS,
  offline). reqwest errors are reduced to an enum because their `Display` includes the URL,
  which carries the key.
- **App names:** letters, digits, `-` and `_`. The first live run showed real Metered app
  names can contain underscores (RFC 1123 host names cannot, DNS can). A test pins this.
- **Lenient paste (Tab B):** the first balanced `[...]`/`{...}` region is cut out (strings and
  comments are skipped everywhere) and parsed as JSON5 **data**. Nothing is ever executed. It
  accepts arrays, `{ iceServers }`, single servers, and Metered's JavaScript snippet (unquoted
  keys, single quotes, trailing commas, comments).
- **Validation:** schemes stun/stuns/turn/turns only (RFC 7064/7065 URIs, no userinfo or paths);
  `?transport=` only on TURN; turn(s) need username and credential; at most 16 servers and 16
  URLs each; bounded string lengths. `Debug` never prints credentials.
- **Connection test:** its own STUN/TURN client (`stun.rs`, `probe.rs`), verified against the
  RFC 5769 vectors (short-term request, IPv4/IPv6 responses, long-term auth). Each URL is
  probed over its own transport (UDP with retransmit, TCP framing, TLS). STUN gets a Binding
  request; TURN gets Allocate with a 401 challenge and long-term auth (retrying once on 438
  stale nonce), must yield a relay candidate, and is then **released at once** with Refresh
  LIFETIME=0. "Verified" needs at least one relay candidate. Results carry a label without
  the host name (`turn/tcp:443`), latency, the error class and the STUN code. The probe also
  reports bytes and allocations for quota accounting.
- **Fake TURN server (tests):** real UDP+TCP servers on 127.0.0.1 that verify
  MESSAGE-INTEGRITY with the long-term key and can simulate quota (486). The tests prove no
  allocation is left open (`active == 0`), and that bad credentials, quota, refused and DNS
  failures are classified correctly.

### Storage
- Installed: Windows Credential Manager through `keyring` 4 (`v1` mode: service
  `com.syncwatch.app`, account `metered-credentials`). Payloads over 2000 bytes (the blob limit
  is 2560) go to a DPAPI file, with an `overflow` marker in Credential Manager.
- Portable: a DPAPI file under `data\config\` (CurrentUser scope with app entropy, so it is
  bound to this user and this PC).
- The last good ICE servers are cached DPAPI-encrypted (`cache\ice-last-good.dpapi`) for
  create/join when Metered is unreachable, with a visible warning.
- The plain `config\connection.json` holds only mode, relay host names, verified flag and time,
  per-URL OK/latency, and the relay-only flag. A test checks that it has no secret-bearing
  fields.
- Every loaded secret (app name, key, TURN usernames and credentials) is registered with the
  log scrubber before use.

### App behavior
- Credentials are saved **only after a test that verifies** (at least one relay).
- Re-verification runs in the background at start. If Metered or the network is unreachable,
  the last verified state is kept with a warning. A rejected key clears "verified".
- Create/Join are gated on `verified`. In debug builds only, `SYNCWATCH_DEV_BYPASS_CONNECTION=1`
  bypasses the gate; release builds have no bypass (`cfg!(debug_assertions)`).
- `open_help_link` opens only the ids in `HELP_LINKS` (https metered.ca pages) through
  ShellExecute.
- All help and error text lives in `apps/desktop/src/strings/connection-help.ts`. A test
  enforces that Metered's menu names appear nowhere else (UI and Rust sources) and that
  unverified lines carry `TODO-VERIFY`.

### Change source (protocol and UI level)
- Magnet links: the Rust `protocol::magnet::parse` is authoritative (`magnet_inspect`,
  `prepare_change_media`). The TypeScript mirror (`src/lib/magnet.ts`) gives instant feedback
  and passes the same `magnet-vectors.json`.
- Local files: identity = blake3(first 4 MB, last 4 MB, size). The dialog builds the real
  `Request::ChangeMedia` and runs it through `protocol::encode`, so it passes wire validation.
  The torrent tab shows "Torrent engine not available yet" and does nothing on the network.

## Live verification (developer's real account, 2026-11-04)
Two runs of `cargo run -p syncwatch-ice-config --example live_probe`. Output showed only
labels, OK/FAIL and latency, and was secret-scanned before display.
- Tab A (API key): 1 credentials request (about 1.7 s), 5 ICE servers. stun/udp:80,
  turn/udp:80, turn/tcp:80, turn/udp:443 and turns/tls:443 were all OK (76-131 ms), with
  4 relay allocations, all released.
- Tab B (pasted array): the same 5 URLs were all OK (93-259 ms), with 4 allocations, all
  released.
- Total quota use: 1 API request, 12 allocations (all released immediately), about 9 KB of
  STUN/TURN control traffic, and **0 bytes relayed**.

## Open items
- `TODO-VERIFY`: Metered's quota-exhausted response, and whether the free plan is enough.
- "Copy diagnostics" (credentials redacted, IPs redacted by default) is Milestone 8.
- Relay-only is stored now and enforced by the WebRTC bridge in Milestone 4.

## Revision: the credentials fetch at room creation (Milestone 9, 2026-11-21)

- **Symptom:**
  - On two fresh installs, every room creation logged "using the last good relay servers
    (Metered unreachable)", while Settings, Test connection passed.
  - The same failure appeared on this PC's release-mode instances at startup: 3 of 4 checks
    failed within a minute.
  - The fetch error was swallowed (`Err(_)`), so the logs had no reason.
- **Cause:**
  - Metered's credentials API answered in 5.1 s during the test, and its answer time varies.
  - The fetch gave up after 8 s in total (connect 5 s), so a slow answer became "unreachable".
  - Settings, Test connection passed because, at that moment, the answer was fast.
  - Metered documents no response time or rate limit for that endpoint.
- **Fix:**
  - The timeouts are now 20 s per attempt (connect 10 s).
  - A timeout or network error is retried once.
  - Each fetch logs its duration and error class (never the URL, app name or key):
    `Metered credentials fetched ms=...` or `... failed error=Timeout`.
  - A room that has to use the saved servers says so in the room: "Could not refresh the relay
    servers from Metered; the saved ones may have expired. SyncWatch tries them anyway". The
    same text goes in the room's events and diagnostics, so the outcome is visible.
- **Saved servers can expire.**
  - Metered's docs (read 2026-11-21) give the GET credentials endpoint no expiry field.
  - Credentials created with `expiryInSeconds` stop working at that time: "a hard cutoff,
    including for calls that are still in progress".
  - The docs recommend fetching fresh ones at every start.
  - So a cached copy cannot be trusted for a new room. It is still tried, because it often
    works, but never silently.
  - Sources:
    - `https://www.metered.ca/docs/turn-rest-api/get-credential`;
    - `.../post-create-credential/`;
    - `.../turnserver-guides/expiring-turn-credentials/`.

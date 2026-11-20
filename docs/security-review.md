# Security review (Milestone 9)

Date: 2026-11-20. Scope: the whole codebase at the end of Milestone 9. Method: reading every
input path from the network, from files and from other processes; checking each against the
threat model (`docs/threat-model.md`); adding fuzz-style tests for every parser. This is a
review by the developer (Claude Code), not an outside audit.

## Findings

| # | Area | Finding | Severity | Status |
|---|---|---|---|---|
| 1 | App commands | Every app command (room, files, updates, **remove all my data**) could also be called from the hidden bridge webview. Only the two bridge commands checked their caller. The bridge page handles remote SDP but never evaluates it, so this needed a WebView2 or page bug to matter. | Low (defence in depth) | **Fixed**: `only_from_main` in `commands.rs` refuses every app command from any webview but `main` (the bridge keeps `bridge_attach` and `bridge_in`); test `only_the_main_window_reaches_app_commands`. |
| 2 | Update download | The verified MSI could be swapped by same-user malware between the final check and `msiexec` reading it. | Informational | Accepted: such malware can replace the installed app directly. Documented in the threat model. |
| 3 | Installer | No Authenticode signature (no certificate). The first download relies on HTTPS from GitHub and `SHA256SUMS`; SmartScreen warns. | Informational | Accepted (decision). Documented in the README and user guide. In-app updates are signed (minisign). |
| 4 | Loopback server | A request without a Host header is accepted (the token is still required). Browsers always send Host, so DNS rebinding stays blocked. | Informational | Accepted. |
| 5 | Fuzz tests | No crash or wrong acceptance found in 200,000 mutations per parser. The one failing assertion was a mistake in the new test (the Range parser returns a half-open range), not in the code. | — | — |

No secret values were found in the code, history, logs or the package (scans below).

## What was reviewed

### Invites and replies (`crates/net/src/invite.rs`, ADR 0014)
- Size checks before any work: text at most 16 KiB, inflated body at most 32 KiB
  (`decompress_to_vec_with_limit`, so no decompression bomb), checksum, then postcard decode,
  then the ed25519 signature over a domain-separated hash, then expiry and kind.
- Invites never contain credentials (test). Replies are only applied after **Admit**, and the
  DTLS fingerprint is checked when the link opens.
- Tests: 3,000 random mutations (existing), wrong kind, expired, bombs, truncation.

### Peer messages (`crates/protocol`, `crates/sync-core`, `crates/net/src/session.rs`)
- Every frame: size first (64 KiB), then the version, then `Message::validate` (string, list
  and chunk limits).
- Permissions are checked by the host for each request. Auth uses SPAKE2 plus ed25519 with
  constant-time MAC compares (`ct_eq`).
- File names from peers are validated (`check_file_name`) and never become paths: received
  externals are stored as `ext-<id>.<letters>` in the room cache, with size ceilings
  (subtitle 20 MB, audio 1 GB) and a BLAKE3 check.
- New fuzz tests in `crates/protocol/tests/fuzz.rs`:
  - `decode`, `decode_auth`, the signaling parsers, magnets and file names;
  - whatever decodes must re-encode.

### Bridge and IPC (`crates/net/src/frame.rs`, `bridge.rs`, `bridge.ts`)
- Frames have a size cap (70 KiB), batches 1 MiB, and checked length prefixes.
- Bridge events are typed serde enums.
- The bridge page has no capabilities and only its two commands (finding 1).
- New fuzz tests in `crates/net/tests/fuzz.rs`: frames, batches, the wire format, events and
  SDP fingerprints. No decoded output is larger than its input.

### Loopback media server (`crates/torrent/src/server.rs`)
- 127.0.0.1 only, random port, 128-bit token with a constant-time compare, Host check,
  single ranges only. The token is never logged.
- New fuzz test of `parse_range`: every accepted range lies inside the file and is never
  empty, including for files of size 0 and `u64::MAX`.

### Torrent parsers (`crates/torrent/src/meta.rs`, `validate.rs`)
- Our own bencode reader runs before librqbit, with these limits:
  - depth, node and file limits;
  - canonical integers and sorted keys;
  - no trailing data;
  - UTF-8 paths;
  - path validation;
  - at most 32 trackers.
- New test `mutated_torrents_never_panic_and_stay_within_limits` (100,000 mutations run):
  - every accepted torrent has valid paths and stays within the limits;
  - its sanitized copy parses again to the same infohash.

### External files (subtitles, audio)
- Picked by the host via the system dialog, read only.
- Members receive them chunked, in order, size-capped and hash-checked (above).
- mpv opens them as subtitle or audio tracks only.

### Relay setup (`crates/ice-config`)
- Pasted text: a size cap, a balanced-bracket region, JSON5, then validation (schemes, ports,
  hosts).
- STUN answers: length and attribute checks, integrity and fingerprint verification.
- New fuzz tests in `crates/ice-config/tests/fuzz.rs` (placeholder credentials only).
- Credentials: Windows Credential Manager and DPAPI, never in invites, logs, diagnostics or
  the package.

### Updates (`apps/desktop/src-tauri/src/updater.rs`, ADR 0016)
- Signed manifest and signed MSI (minisign, `minisign-verify`, no legacy signatures).
- Strictly newer versions only.
- SHA-256 and size checks.
- An HTTPS host allowlist on every redirect, timeouts and size limits.
- `msiexec` by absolute path.
- Test-only overrides are compiled out of releases.
- New test: 3,000 mutated manifests or signatures are all refused without panics. Existing
  tests use a local server serving newer, older, unsigned, wrong-key and tampered releases
  and a bad redirect.

### Installer and uninstall (`wix/main.wxs`, `cleanup.rs`, ADR 0015)
- Per-user install with no elevation.
- The cleanup runs only with `REMOVEUSERDATA=1` on a full uninstall (never on upgrade), or on
  the explicit Settings button.
- It removes a fixed folder (`remove_dir_all` does not follow junctions) and the two
  Credential Manager entries.
- `verify-package.ps1` checks:
  - per-user mode, no admin rights, the file list, pinned DLL hashes;
  - imports (only system DLLs, the bundled libmpv and the Vulkan loader);
  - no secrets, and no OpenSSL, native-tls or aws-lc.

### Capabilities and CSP
- The main window can use events and the open and save dialogs only.
- The CSP blocks inline and remote scripts, with `connect-src` IPC only, `object-src 'none'`
  and frozen prototypes. A UI test fails on any remote resource.
- The only link opener (`open_help_link`) maps ids to two fixed https URLs.

### Secrets in logs, diagnostics and the repository
- Third-party crates are capped at `warn`, and every line goes through the scrubber:
  registered secrets, paths shortened, no chat.
- Diagnostics remove credentials, keys, tokens, the Metered app name and IP addresses.
- The full-history scan (gitleaks plus a check against the real local values, never printed)
  and the package scan are clean. The repository is public, and so is its history (the
  first-release check in `scripts/release.ps1` runs both again).

### Native code
- 34 `unsafe` uses, almost all Win32 window and DLL calls in `native/`, each with a
  `SAFETY` note.
- Outside `native/`: `ShellExecuteW` of a constant URL, `GetDiskFreeSpaceExW`, DPAPI, and one
  `set_var` before any thread starts.
- libmpv and Vulkan load by absolute path with safe search flags.

## Commands

```
cargo test -p syncwatch-protocol --test fuzz
cargo test -p syncwatch-net --test fuzz
cargo test -p syncwatch-ice-config --test fuzz
cargo test -p syncwatch-torrent --test fuzz
cargo test -p syncwatch-torrent --lib mutated
cargo test -p syncwatch-desktop mutated
```

- `$env:SYNCWATCH_FUZZ_ITERS = 200000` runs the integration fuzz tests longer.
- The default (4,000 iterations) runs in CI in well under a second per test.

## Not covered

- No coverage-guided fuzzing (cargo-fuzz needs nightly). The tests are seeded and
  reproducible, but they find shallow bugs, not deep ones.
- librqbit's own parsers, mpv/FFmpeg's demuxers and WebView2 are third-party code, kept
  current and pinned.
- A malicious host can still make members open a media file they chose to load (by design).

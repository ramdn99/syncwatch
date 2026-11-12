# SyncWatch build guide

All scripts target PowerShell 7 (`pwsh`). Commands are listed one per line. If PowerShell blocks
`pnpm` (script execution policy), use `pnpm.cmd` instead.

## 1. Check the environment

```
pwsh -NoProfile -File scripts/check-dev-env.ps1 -Milestone 1
```

Each check belongs to the first milestone that needs it. Checks up to `-Milestone` FAIL when missing;
later ones only WARN. Tools are found on PATH, on the user/machine PATH in the registry ("open a new
terminal" hint), and at known locations. The script never prints secrets.

## 2. Fetch pinned third-party files

```
pwsh -NoProfile -File scripts/fetch-third-party.ps1
```

This downloads the fonts (Space Grotesk, Inter, Noto Sans; OFL) and the LunarG Vulkan loader into
`third_party/cache` and verifies every SHA-256 from `third_party/pins.json`. It mirrors the fonts to
`apps/desktop/public/fonts` and checks `LIBMPV_DIR` read-only. It is idempotent.

## 3. JavaScript dependencies and brand assets

```
pnpm.cmd install
pnpm.cmd brand
```

`pnpm brand` regenerates `assets/branding/generated/*` and the Tauri icons from the never-edited
`assets/branding/icon.svg` (reproducible output).

## 4. Build, lint, test

```
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
pnpm.cmd --dir apps/desktop lint
pnpm.cmd --dir apps/desktop test
pwsh -NoProfile -File scripts/verify-imports.ps1
pwsh -NoProfile -File scripts/tests/verify-imports.tests.ps1
```

## 4b. Sync simulator

```
cargo run --release -p syncwatch-sim -- --seed 7
cargo run --release -p syncwatch-sim -- --latency-ms 150 --jitter-ms 60 --loss 0.2
```

Host + N members (one muted in chat, one without the file) plus a late joiner. The scenario covers
play, seek, speed change, chat and an external subtitle. The run fails above 200 ms steady-state
divergence or on any protocol violation.

## 4c. Live connection check (your own Metered account)

Reads `.env.local` and `.env.ice.local` and prints only transport labels, OK/FAIL and latency. Every
TURN allocation is released immediately (no relayed data). Keep runs rare; it uses your quota.

```
cargo run -p syncwatch-ice-config --example live_probe
cargo test -p syncwatch-ice-config -- --ignored keyring
```

The relay-only network lab with `-Ice metered` (section 5b) is the end-to-end check through your
relay. It reads the static ICE list (no API request) and also uses quota.

The second command touches the real Windows Credential Manager (a throwaway entry it deletes).

## 5. Run the app (development)

```
$env:SYNCWATCH_DEV_MEDIA = 'D:\media\test.mkv'
pnpm.cmd --dir apps/desktop tauri dev
```

Development environment variables:

| Variable | Effect |
|---|---|
| `SYNCWATCH_DEV_MEDIA` | Debug builds: open this file once the UI is ready |
| `SYNCWATCH_FORCE_BUNDLED_VULKAN=1` | Skip the system `vulkan-1.dll` and load `<exe>\vulkan\vulkan-1.dll` |
| `SYNCWATCH_DEV_LAYOUT_PROBE=1` | Write `%LOCALAPPDATA%\SyncWatch\dev\layout-probe.json` after every video placement |
| `SYNCWATCH_DEV_BYPASS_CONNECTION=1` | Debug builds only: allow Create/Join without a verified connection (release builds have no bypass) |
| `SYNCWATCH_LOG` | App log level (`trace`/`debug`/`info`/`warn`/`error`). Third-party crates stay at `warn` |
| `SYNCWATCH_SIGNALING_URL` | Signaling server for this instance (default `wss://signal.example.invalid/ws`, a placeholder until deployment). `ws://` is accepted only for localhost. The URL is logged at startup |
| `SYNCWATCH_DEV_IDENTITY=a` | Debug builds: use `%LOCALAPPDATA%\SyncWatch-dev-a` as the data folder, so two instances can run side by side |

Logs go to `%LOCALAPPDATA%\SyncWatch\logs\syncwatch.log` (in portable mode: `<exe>\data\logs`).

## 5a. Two windows in one room

`scripts/dev/two-windows.ps1` starts:
- a local signaling server;
- the dev UI server;
- two app instances as two different people: window `a` (data in `%LOCALAPPDATA%\SyncWatch-dev-a`) and window `b`.

It stops everything again when both windows are closed (or on Ctrl+C). It never moves the mouse or presses keys.

```
pwsh -NoProfile -File scripts/dev/two-windows.ps1
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -AutoRoom
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -AutoRoom -Password test
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Soak 900
pwsh -NoProfile -File scripts/dev/make-test-media.ps1
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario sync
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario chattime
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario late
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario popup
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario succession
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario chat
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario rooms
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Ice coturn -AutoRoom
```

| Option | Meaning |
|---|---|
| `-AutoRoom` | window a creates a room, window b joins it |
| `-Password x` | the auto room uses this password |
| `-Scenario chat` | three windows through the real UI command path: chat both ways, mute (refused, still sees chat, still muted after a reconnect), unmute, Chat enabled off/on (only the host can send while off), a late joiner while chat is off, slow mode, delete, clear. Checks the member's chat input in its own window (disabled with the right text, enabled again). Prints PASS/FAIL per step |
| `-Scenario sync` | real players: play/pause/seek/speed by host and permitted member, refused member seek, audio/subtitle switch, external subtitle + audio sent and selected, subtitle and audio delay; prints latencies and position agreement |
| `-Scenario chattime` | chat chips at 0:10, 5:30 and 1:02:03 of a playing 1 h+ video, paused position, no-media message, ordering with two simultaneous senders |
| `-Scenario late` | three windows: a late joiner with the mismatch file lands at the host position with the same tracks; a member missing the file does not block |
| `-Scenario popup` | a minimized member loads and plays: the window stays minimized, the video hidden; shown again after restore |
| `-Scenario succession` | three windows: the host process is killed; the next member hosts, the room keeps playing |
| `-Scenario rooms` | room 1 with a video (`D:\media\test.mkv`, read only, or `-Media`), close it, create room 2, join: asserts nothing from room 1 is loaded or shown |
| `-Soak N` | auto room, window b minimized, both idle for N s; fails on any "Reconnecting" banner or link reconnect |
| `-Ice none` / `coturn` / `metered` | direct (default) / relay-only via the local coturn / relay-only via your Metered connection (uses quota) |
| `-Docker` | run the signaling server in Docker instead of the local binary |
| `-FreePort` | stop a stale node process of this repo that still holds port 1420 |
| `-Media D:\media\test.mkv` | both windows open this file at startup |

Without `-AutoRoom`, use Create room in window a, then Join room in window b with the code from a's header. Rooms without a password show four words in b's header and next to b's name in a's member list: they must match.

## 5b. Signaling server and network lab

Run the signaling server locally:

```
cargo run -p syncwatch-signaling
```

It listens on `0.0.0.0:8787` (`SYNCWATCH_SIGNALING_BIND` changes that). Docker and TLS
deployment notes are in `deploy/README.md`.

The network lab starts the signaling server, the Vite dev server and two app instances. They
connect through the real WebView2 bridge, write measurements and are stopped again. It never
moves the mouse or presses keys. Results are printed and kept in `%TEMP%\syncwatch-netlab-<pid>`.

```
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice none -Seconds 25 -MB 256 -BenchMB 256 -Release
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice coturn -Seconds 25 -MB 64 -Release
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice none -Seconds 45 -MB 0 -DropAt 10 -Release
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice none -Seconds 40 -MB 0 -FreezeAt 10 -Release
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice coturn -Seconds 780 -MB 16 -Minimized -Release
pwsh -NoProfile -File scripts/dev/net-lab.ps1 -Ice metered -Seconds 10 -MB 1 -Release
```

| Option | Meaning |
|---|---|
| `-Ice none` | host candidates only (direct, local) |
| `-Ice coturn` | relay-only through the local coturn container (Docker Desktop must be running; no quota) |
| `-Ice metered` | relay-only through your Metered account, using the ICE list in `.env.ice.local` (uses quota: keep it short) |
| `-DropAt N` | member stops forwarding traffic N s after connecting (watchdog: interrupt at 4 s, restart at 10 s) |
| `-FreezeAt N` | member's bridge page blocks its event loop N s after connecting (watchdog: page rebuilt after 5 s) |
| `-Minimized` | both windows minimized for the whole run (background-throttling check) |
| `-BenchMB N` | also measure raw data-channel speed inside one bridge page (no IPC) |
| `-Release` | optimized build (debug assertions stay on so the dev-only lab code is included) |

## 5c. Torrents (Milestone 7)

Tests run in process on 127.0.0.1 only, with an in-process librqbit seeder plus fake HTTP and
UDP trackers that record every announce:

```
cargo test -p syncwatch-torrent
```

The throttled playback-window comparison (`playback_window`) takes about 40 s.

The lab uses two real windows. qBittorrent is the only seeder, throttled, with its embedded
tracker on 127.0.0.1. Only the script drives it (never the app), and it uses our own generated
test torrent:

```
pwsh -NoProfile -File scripts/dev/make-test-media.ps1
pwsh -NoProfile -File scripts/dev/torrent-lab.ps1
pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -UpKiB 500 -KeepQbt
```

Lab variants:

```
pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -Scenario torrentfile -UpKiB 1500
pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -SameIpDefault
pwsh -NoProfile -File scripts/dev/torrent-lab.ps1 -Manual
```

- `-Scenario torrentfile` uses three windows:
  1. the host opens a `.torrent` file;
  2. one member declines, then rejoins late and starts on the host's file;
  3. the source switches .torrent, magnet, local file, then back to the .torrent.
- `-SameIpDefault` puts qBittorrent on 127.0.0.2 with libtorrent's default of one connection
  per IP, like a real public swarm seen from one PC. This reproduced the member bug of ADR 0011.
- `-Manual` seeds both generated test torrents and prints the multi-file magnet, the 40 MB
  `.torrent` file path and its magnet for a hands-on test. It cleans up when you press a key,
  or after `-ManualMinutes` (60 by default).

Inspect any `.torrent` with our parser, and optionally try it for N seconds (trackers only, DHT
off, loopback listener):

```
cargo run -p syncwatch-torrent --example inspect_torrent -- <file.torrent>
cargo run -p syncwatch-torrent --example inspect_torrent -- <file.torrent> 60
```

The manual Wireshark/TCPView checklist is `docs/checklists/torrent-stop.md`.

| Variable (debug builds) | Effect |
|---|---|
| `SYNCWATCH_DEV_TORRENT_BIND=127.0.0.1` | The torrent engine listens on the loopback only (no firewall prompt during automated runs) |
| `SYNCWATCH_DEV_TORRENT_DHT=0` | DHT off |

## 5d. TLS dependency tree, cargo-deny, cargo-audit

```
pwsh -NoProfile -File scripts/check-tls-tree.ps1
cargo deny --locked check bans advisories sources
cargo audit
pwsh -NoProfile -File scripts/ci.ps1
```

- `check-tls-tree.ps1` runs `cargo tree --target x86_64-pc-windows-msvc -i <crate>` and fails
  if aws-lc-rs, aws-lc-sys, native-tls, openssl or openssl-sys is part of the Windows build.
  `deny.toml` bans the same crates.
- **OpenSSL in Cargo.lock is expected:** `crypto-hash` (librqbit's SHA-1) depends on OpenSSL on
  Linux only. On Windows it uses the system CryptoAPI, so OpenSSL is never built for Windows.
  `cargo tree -i openssl` therefore prints "nothing to print" rather than "did not match".
- The rustls ring provider is installed once, at startup
  (`syncwatch_ice_config::install_crypto_provider`). A test makes sure no other install exists.
- **Ignored advisory: RUSTSEC-2026-0293.** It concerns `ringbuf` 0.4.8, pulled in through
  `librqbit-utp` 0.7.0. It is unreachable for us, because librqbit-utp only stores `u8` there
  and uTP is off (`TcpOnly`). The reasons are in `deny.toml` and `.cargo/audit.toml`.

### Upgrading librqbit

librqbit and all its sub-crates are pinned to the same version in `crates/torrent/Cargo.toml`:
librqbit-core, -sha1-wrapper, -upnp, -tracker-comms, -dht, -bencode, -buffers,
-clone-to-owned, -lsd and -peer-protocol. Otherwise `^` resolution mixes versions.

1. Pick a release that is at least two weeks old (crates.io dates).
2. Bump every `=9.x.y` pin together.
3. Do not enable librqbit's `rust-tls`, `default-tls` or `sha1-ring` features. They pull in
   aws-lc-rs or native-tls. Keep `default-features = false`.
4. Re-read the parts the ADRs depend on, and update ADRs 0009 and 0010 if they changed:
   - `torrent_state/streaming.rs` (32 MiB window, stream interleaving);
   - `piece_tracker.rs` and `file_info.rs` (background order);
   - `session.rs` (`pause`, `delete`, `stop`, `make_peer_rx`);
   - tracker-comms (does it send "stopped" now?).
5. Run `scripts/check-tls-tree.ps1`, `cargo deny` and `cargo audit`. Check whether
   RUSTSEC-2026-0293 still applies: if librqbit-utp moved to ringbuf 0.5.2 or later, remove the
   ignore from both files.
6. Run `cargo test -p syncwatch-torrent` and `scripts/dev/torrent-lab.ps1`.

## 6. Secret scan

```
pwsh -NoProfile -File scripts/secret-scan.ps1 -Mode All
```

The git hooks in `scripts/git-hooks` run it before every commit (`-Mode Staged`) and every push
(`-Mode History`). Enable them once per clone:

```
git config core.hooksPath scripts/git-hooks
```

## 7. Alignment check (only when nobody is using the machine)

`scripts/dev/alignment-check.ps1` moves the real mouse and sends real keys. See the script header.

# ADR 0006: Rooms, peer authentication and the room session

Status: accepted (Milestone 5). Date: 2026-11-05.

## Context

Spec sections 1, 6, 9 and 11 ask for:
- rooms with an optional password, where SPAKE2 is bound to BOTH DTLS fingerprints;
- four verification words for rooms without a password;
- an ed25519 identity per install;
- wrong-password throttling (slow down, a temporary ban after 6 tries, a global pause after 20
  failures a minute);
- permissions enforced by the host, and chat with moderation and limits;
- kick, ban for the session, lock, and host transfer;
- a dropped member keeping their seat for 20 s.

Spec section 2 also requires that the "Reconnecting" banner never appears falsely, with a
15-minute two-window soak to prove it. Milestone 4 provided the bridge transport (ADR 0005) and
Milestone 2 the sync-core machine (ADR 0003).

## Decisions

### Identity
- An ed25519 key per install (ed25519-dalek 2.2.0) is created on first use. It is stored like the
  connection credentials: in Windows Credential Manager (account `identity-ed25519`), or in a DPAPI
  file in portable mode. The public key is the `Identity` that seats and bans are keyed by.
- Debug builds with `SYNCWATCH_DEV_IDENTITY` get their own key, so two dev windows are two people.

### Authentication handshake (`crates/protocol/src/auth.rs`, `crates/net/src/auth.rs`)
- The handshake runs on the reliable data channel before any room message. It has its own
  versioned postcard envelope (4 KiB cap), so an unauthenticated peer never reaches sync-core.
  App frames start with a kind byte: 0 = auth, 1 = room.
- Order:
  1. Host sends `HostHello`.
  2. Member sends `MemberHello`, plus SPAKE2 message A in password rooms.
  3. Host sends `HostSpake` (password rooms only).
  4. Member sends `MemberConfirm`.
  5. Host sends `HostConfirm`.

  **The member proves first**, so the host always detects a wrong password and can throttle it.
  The host never hands out a confirmation that could be used to test password guesses offline.
- **Transcript.** A blake3 hash, with domain separation and length-prefixed fields, over:
  - the room code;
  - the member's (offerer's) DTLS fingerprint and the host's (answerer's) DTLS fingerprint;
  - both identities and the member's display name;
  - both nonces;
  - the SPAKE2 messages.

  Each side computes it from its own view of the connection.
- **Password rooms.** SPAKE2 (spake2 0.4.0, Ed25519 group, the identities as SPAKE2 identities)
  gives the key. Both sides MAC the transcript with keys derived from it, and both sign the
  transcript with their identity key. A relay or signaling MITM terminates DTLS separately on
  each side, so the transcripts differ and the MACs fail even with the right password (tested).
- **Rooms without a password.** The MAC key is derived from the transcript (it proves agreement
  only). Each side shows four words from a 256-word list (32 bits), derived from the transcript.
  Members see them in the room header and the host sees them next to each member. Matching words
  mean nobody is in the middle (tested).
- After a host transfer, members pin the new host's identity (from the handover) and refuse any
  other.
- **Throttling** (`AuthThrottle`, per room, on the host):
  - Each wrong password delays the next attempt: 1, 2, 4, 8, 8 s.
  - A try during the delay is refused without counting as a guess.
  - The 6th failure bans that identity for 10 minutes and closes the connection.
  - 20 failures within 60 s pause ALL password attempts in the room for 60 s. Identities are free
    to create, so this global limit is what bounds online guessing.

### Room session (`crates/net/src/session.rs`)
- One deterministic machine, `(Clock, Input) -> Vec<Output>`, combines signaling, transport links,
  authentication and the sync-core `Peer`. The app runs it in a tokio task
  (`apps/desktop/src-tauri/src/room.rs`). The tests run several sessions in process, with a fake
  signaling server and fake links (`crates/net/tests/session.rs`).
- **Host star.** The member is the offerer. The host creates an answerer link when an offer
  arrives. A new offer on a known link means a restart: the old seat is marked dropped, then the
  member authenticates again and gets the same seat.
- **Seat retention.** sync-core keeps a dropped member's seat for 20 s (`SEAT_RETENTION_US`).
  During that time the member is listed as reconnecting (new field `MemberInfo::connected`). A
  deliberate leave (`Bye`), a kick or a ban frees the seat at once.
- **Signaling drops do not break rooms.** The host keeps an authenticated link whose member left
  signaling. Both sides reconnect to signaling with backoff: the host resumes with its token, a
  member re-joins.
- **Banner rule.** "Connection interrupted. Reconnecting..." shows only:
  - on the transport's `Interrupted` event, which means inbound traffic actually stopped
    (ADR 0005);
  - or while the link to the host is being renegotiated.

  It clears on `Recovered` or after re-authentication. The host never shows a room-wide banner,
  only a per-member state.
- **Host transfer over the network:**
  1. The old host sends `HandoverSecret { password }` to the new host over their authenticated
     link.
  2. It broadcasts the sync-core handover (playback, tracks, manifest, permissions, chat, bans,
     lock).
  3. It sends signaling `TransferHost`.
  4. The signaling server swaps the roles and gives the new host a resume token (`BecameHost`).
     Members and the old host get `HostChanged`; the old host also gets a new member number.
  5. The old host keeps its link to the new host and closes the rest.
  6. Each other member reconnects to the new host once it has seen both the handover and
     `HostChanged` (8 s fallback). Their seats are kept for 20 s meanwhile.

  Offers that reach the new host before its own handover arrives are queued.
- **Kick and ban.** sync-core sends `Kicked`, and the link closes after a 400 ms flush. Banned
  identities are refused with `Reject(Banned)`; locked and full rooms likewise. The member sees a
  friendly reason.
- Chat, permissions, moderation, rate limits and sanitization are enforced on the host by
  sync-core (ADR 0003). The app shows plain text only and never links URLs.

### "Choose my file" (no permission)
- sync-core shows the Missing-file panel once per media epoch. That includes joining a room whose
  media is a local file (from the Welcome snapshot), and every change to a local file.
- The panel shows the name, size and duration. "Choose my file" (or dropping a file while the panel
  is open) hashes the file locally: blake3 over the first and last 4 MB, plus the size. Only the
  identity is reported; the path never leaves the machine.
- A mismatch shows a warning and the member may proceed. "Choose a different file..." stays in the
  side panel.
- Playback control of the real player stays for Milestone 6: Milestone 5 only loads the chosen
  file.

### Dev tooling
- `scripts/dev/two-windows.ps1`:
  - starts the signaling server (local binary, or Docker with `-Docker`), the Vite dev server and
    two instances (identities a and b);
  - reports or frees a busy port 1420 (`-FreePort`);
  - can create and join a room automatically (`-AutoRoom`);
  - runs the soak (`-Soak N`: the member window is minimized, and the run fails on any banner or
    link reconnect);
  - stops everything it started.
- No mouse or keyboard input is used. Debug-only switches:
  - `SYNCWATCH_DEV_AUTOROOM`, `SYNCWATCH_DEV_ROOM_FILE`, `SYNCWATCH_DEV_NAME`;
  - `SYNCWATCH_DEV_ICE` (none or coturn);
  - `SYNCWATCH_DEV_FORCE_RELAY`;
  - `SYNCWATCH_DEV_SOAK_FILE`, `SYNCWATCH_DEV_MINIMIZE`.

## Measurements

| Test | Result |
|---|---|
| Two real windows, direct: signaling connect to authenticated | about 0.6 s |
| 15-minute two-window soak (member minimized, both idle) | 914 s in the room on both sides: 0 Reconnecting banners, 0 link reconnects, 0 watchdog interruptions logged (passes the 306 s mark of the old open issue) |
| Room join, relay-only through Metered (fresh credentials per create/join) | both links relayed; member signaling connect to host authenticated about 1.45 s; 0 banners in a 30 s run. Quota: 2 API requests, at most 8 TURN allocations (4 TURN URLs x 2 peers), well under 1 MB relayed (handshake, keepalives). The script stops the windows forcibly, so those allocations expired on the server instead of being released |
| In-process session tests (fake signaling and links) | 13 tests: join with and without password, missing/wrong/right password with throttling, MITM, kick and ban, lock, chat refusal (now: mute, ADR 0008), banner only on real interruption, restart keeps the seat, host transfer, password moves with the host, close room, leave frees the seat |

## Findings from the first manual test (fixed)
1. **Room actions with multi-word fields failed** (chat, chat settings, slow mode) with
   `missing field client_id`. `#[serde(rename_all)]` on an enum renames only the variant names,
   not the fields inside the variants. The enum now also uses `rename_all_fields = "camelCase"`
   and `deny_unknown_fields`.
   - Contract test: `room.contract.test.ts` writes one payload per action type, built from a
     `Record` the compiler forces to be complete, to `room-actions.fixture.json`. The Rust test
     `room_actions_contract` deserializes that file with the real command argument type and checks
     that every variant is covered. With the old attribute it fails with the user's exact error.
   - End to end: `two-windows.ps1 -Scenario chat` runs the actions through the real `invoke` path
     in two app instances (22 checks).
   - Failed commands show a friendly message. The raw error goes to the Diagnostics view.
2. **An open room joined with a typed password** now tells the member it was ignored, in the
   Join dialog and once in the room (`RoomUi::PasswordIgnored`, session test).
3. **The "Choose my file" panel stayed open after a file was chosen.** The panel state now lives
   in the Rust UI mirror (`panelOpen`). It closes as soon as a file is picked and stays closed
   through verification, a mismatch and unrelated updates. Only a new media change opens it
   again (mirror test). A mismatch shows a bar with "Watch anyway" and "Choose a different file".
4. **Members see their own permissions** ("You can: ..."), updated live (session, mirror and
   render tests).
5. **Diagnostics view:**
   - connection state (signaling host, connected, reconnects; links per member);
   - recent room events;
   - rejected requests per member, plus this window's own UI errors.

   "Copy diagnostics" leaves out chat text, credentials and addresses.

6. **A new room showed the previous room's video.**
   - Root cause: the player was never unloaded. The `Player` trait had no unload at all, so mpv
     kept the last file loaded after a room ended. The next room view read the player snapshot
     when it mounted. Also, at end of file mpv cleared "file loaded" but kept the old track list.
   - Not the cause:
     - Room state in memory: every room gets a new session and sync-core machine, and the UI
       mirror was already reset on start.
     - A saved "last played" value: none exists; only layout widths and the display name are
       stored.
     - The dev startup variable: `SYNCWATCH_DEV_MEDIA` loads once per process, before any room.
       Entering a room now unloads it, and the rooms scenario uses its own variable.
   - Fix: `Player::unload()` (mpv `stop`, which clears the state and the tracks), and
     `reset_room_scope` runs when a room starts and when it ends, for every way it can end. It
     unloads the player, forgets the chosen file and resets the UI state. The room view also
     remounts per room (`sessionId`).
   - Nothing else is left behind. The sync-core state (descriptor, epochs, playback, tracks,
     manifest, chat history, gate, transfers) goes away with the session. Torrents and the
     per-room external-file cache do not exist yet (Milestones 6 and 7). They must hook into
     `reset_room_scope`.
   - Tests:
     - `nothing_from_a_room_survives_into_the_next` (same process, mock player);
     - `a_new_room_after_closing_one_starts_clean_for_host_and_member` (sessions);
     - `VideoEmpty.test.tsx` ("No video yet");
     - `two-windows.ps1 -Scenario rooms`: load a video in room 1, close it, create room 2 and
       join. With the unload disabled it reproduces the bug.

## Security notes (feed the Milestone 9 threat model)
- **Signaling compromise.** It cannot join or impersonate anyone in a password room without the
  password: one online guess per attempt, throttled. In rooms without a password, a MITM changes
  the four words, so users must compare them.
- **The host reads all chat** and the room password: the password moves with the host role on
  transfer, over the authenticated DTLS link.
- **Display names are untrusted.** sync-core sanitizes them, and they are bound into the
  transcript.
- **Logs** contain short identity prefixes (4 bytes), never passwords, chat text or SDP.

## Consequences
- Milestone 6 drives the real player from the session's `Player` outputs and adds the
  relayed-transfer warning, using the link state that already exists.
- Host succession when the host disappears (Milestone 6, last item) can reuse the transfer path:
  members already know how to reconnect to a new host with a pinned identity.

## Revision: wrong passwords with invites (Milestone 9, 2026-11-21)

**The manual-test finding.** A member who typed a wrong password could not get in:
- the host kept showing "Connecting";
- the member's retry did nothing.

**Root causes.** The auth state machines were already right:
- a wrong password does not close the link;
- the member's retry starts a fresh SPAKE2 exchange with a new nonce (unit test
  `a_retry_on_the_same_connection_restarts_spake_cleanly`).

The session around them was not:
1. **The host closed the link after 30 s.** Its link kept the 30 s auth timeout after a wrong
   password (the 180 s password time was applied only on the member's side). A member who took
   longer than 30 s lost the connection, and its next submit went nowhere.
2. **The member's retry cut off its own link.** Its submit cleared "waiting for a password"
   without restarting the auth clock, so a retry more than 30 s after the open was closed by
   the member's own next tick. Over a relay that tick comes before the host's answer.
3. **The host never showed the attempts.** The invite never reflected wrong passwords (still
   "Connecting"). Bans, timeouts and members who left had no reason.
4. **The Join dialog's password form had no countdown or attempt count.**

**Fixes:**
- Every attempt gets the password time again (180 s), on both sides.
- The host's invite counts wrong attempts, with the time of the last one. A ban fails the
  invite with a reason, and so do a timeout and a member who leaves.
- Prompts carry the attempt count and "too early" versus "wrong", with a shared retry
  component (countdown, self-enabling button, Enter).
- The throttle is unchanged. It is kept per member identity on the host, so neither a
  reconnect nor a new invite resets it (test). No password or proof is logged.
- **Order kept:** the password is checked after Admit (the words come first), because
  nothing connects before Admit (ADR 0014). A right password goes straight into the room.

**Tests:**
- Session tests:
  - a retry a minute later on the same link;
  - a retry while throttled;
  - a ban after 6 that survives a new invite;
  - the host timeout with a reason;
  - cancel on both sides;
  - a late retry with the member's tick before the host's answer.
- UI tests for the exact flow.
- Release-mode end-to-end runs: wrong, wrong 41 s later, then right: 36/36 direct and 36/36
  relay-only.

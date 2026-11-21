# ADR 0014: Manual invites instead of a signaling server

Status: accepted (Milestone 9, 2026-11-19).

## Context

Users get no server of any kind. Each person has only their own Metered.ca TURN/STUN account.
TURN relays media but does not carry the first WebRTC handshake (offer, answer, candidates).
Until Milestone 8 that handshake went through our own WebSocket signaling server, which was
never deployed.

## Decision

Release builds connect with **manual invites**:
1. The host clicks **Invite a member**. The app creates a WebRTC offer, gathers all ICE
   candidates (with the host's Metered servers; with "Always use relay" only relay
   candidates, which also hides the host's IP) and shows an **invite** text.
2. The member pastes it under **Join**. The app answers and shows a **reply** text.
3. The member sends the reply back by any chat app. The host pastes it into the invite.
4. Both screens show the member's name and the same **four words**. They compare them out
   loud. The host clicks **Admit** or **Refuse**.
5. Only after Admit does the host apply the answer and the connection start. Then the
   existing handshake runs: the optional SPAKE2 password bound to the DTLS fingerprints,
   signed identities and throttling (ADR 0006).

### Invite and reply texts (`crates/net/src/invite.rs`)

- The text is `SWINV1-` or `SWREP1-`, then base64url of the deflated postcard body plus a
  4-byte checksum.
- **Body:**
  - room protocol version;
  - room id;
  - single-use 128-bit invite id;
  - host identity;
  - room name;
  - whether a password is needed;
  - expiry (default 10 minutes);
  - for a re-invite, the member identity it is bound to;
  - the complete SDP and all candidates.
  - A reply carries the invite id, the member identity, the display name, and the SDP and
    candidates.
- **Signed** with the sender's ed25519 identity over a domain-separated hash: a changed text is
  refused as tampered.
- **Checksum:** tells a paste error (cut off, one character changed) apart from tampering, for
  a friendly message.
- **Checks, in order:** length (16 KiB of text), prefix and kind (an invite pasted where a reply
  belongs is named as such), checksum, decompression limit (32 KiB, no decompression bombs),
  format and protocol version, structure (SDP size, a DTLS fingerprint, at most 40 candidates
  of at most 512 bytes, no control characters, names of at most 40 characters), signature,
  expiry.
- **No credentials:** an invite or reply never contains the Metered API key or TURN
  usernames or credentials (test `invites_and_replies_carry_no_relay_credentials`). The SDP's
  ICE username fragment and password only authorize that one WebRTC session.
- **Invite words** bind the room id, the invite id, both DTLS fingerprints and both
  identities. When the connection opens, each side checks that the other's certificate is the
  one the invite or reply named; otherwise the link is closed. After joining, the same words
  are shown (not a second set).

### Handshake transport (`crates/net/src/handshake.rs`)

- `HandshakeMode::ManualInvite` is the release default.
- `HandshakeMode::Signaling` keeps the WebSocket server for development and the in-process
  tests only. It is not in release builds and needs no deployment.
- The `HandshakeTransport` trait carries the session's signaling messages. It has two
  backends: `NoNetwork` (manual invites) and `SignalingConn` (WebSocket). Another message bus
  can be added as a third backend.

## What gets worse than with a server

| Topic | With a signaling server | With manual invites |
|---|---|---|
| Joining | An 8-character room code | Two copy-pastes per joiner (invite out, reply back), through any chat app |
| Many members | One code for everyone | One invite per member (single use) |
| Reconnect after a real outage | Automatic (ICE restart through signaling) | Short outages recover by themselves (ICE consent). After 30 s of silence, or when the link closes, the member's session ends. The host clicks **Re-invite** on that member: an invite bound to their identity, and their seat is kept for 5 minutes (configurable). |
| Late join | Code at any time | An invite from the host at any time (same flow) |
| Host transfer ("Make host") | Members reconnect to the new host through signaling | Not available in invite rooms yet: the session refuses it with a message |
| Host succession (host crashed) | A member takes over; the others reconnect through signaling | Not available: when the host is gone, members see "connection lost". Fallback documented in the user guide: the room ends; someone creates a new room and invites the others again |
| Room code | Short, shareable | A room name for display only |

### Standby links for succession: not built (honest status)

The plan was to pre-establish standby links between the designated successor and every
member, with the host relaying the handshakes over its data channels. Not built in this
milestone: it needs a relayed-signaling message type, authentication of links to a member
who is not host yet, and a switch-over protocol, all on top of everything else in Milestone 9.
The documented fallback applies. A later milestone can add it behind the same session API.

## Considered: Metered Realtime Messaging (not adopted)

Found 2026-11-19 in Metered's documentation: a separate Metered product, a WebSocket message
bus meant as the signaling layer next to their TURN service.
- **Endpoint:** `wss://rms.metered.ca/v1`.
- **Free plan:** 100 concurrent connections and 100k messages a month, hard-capped.
- **Keys:** publishable `pk_` keys, or `sk_` secret keys that create short-lived connect
  tokens limited to some channels and permissions.
- **Limits:** no message history; messages up to 64 KB.
- **Not documented:** whether Metered can read the messages.

It would turn joining into one paste (a one-way invite), and would carry reconnects, late
joins and succession over a room channel. **Not adopted, by decision:**
- **One Metered product only.** Every host would have to enable and configure a second product.
- **No credentials in invites.** Members can only join the host's channel with something from
  the host's account: either its publishable key (long-lived and account-wide) or a token the
  host's app creates with its secret key. Either would be a credential inside the invite.
- **Unverified risk, TURN credential exposure:** Metered's "Auto-inject TURN" puts TURN
  credentials into the connect response. A token handed to joiners might therefore give them
  the host's TURN credentials, and they would use the host's relay quota. The documentation
  does not show a per-key or per-token switch.

**To revisit:** a short live test on the maintainer's own account:
1. Enable Realtime Messaging.
2. Create a secret key with Auto-inject TURN off.
3. Create a token limited to one channel with a short expiry.
4. Check what a client receives in `welcome.metadata`, and whether the token can reach any
   other channel.

If it holds up, it can be an optional `HandshakeTransport` backend; manual invites stay the
default.

## Tests

- **Codec unit tests** (`invite.rs`):
  - round trip;
  - size (a realistic invite is under 2,000 characters);
  - expiry, kind and version checks;
  - paste damage versus tampering;
  - decompression bombs and bad structure;
  - 3,000 mutated inputs without a panic;
  - words that depend on every input.
- **Session tests** (`crates/net/tests/session.rs`):
  - invite, reply, admit, chat;
  - an unadmitted reply never gets room data, and the member gives up after the invite's
    lifetime;
  - reused, expired, tampered and unknown replies are refused;
  - no relay credentials in invites;
  - password rooms and the wrong-password path;
  - a connection that does not match the reply is closed;
  - re-invite to a kept seat, and the wrong person refused;
  - Make host refused.
- **Window tests:** see the user guide walkthrough and `two-windows.ps1 -Scenario invite`.

## Revision: two-machine failures found in the Sandbox (Milestone 9, 2026-11-21)

In the hands-on test with two machines, every invite failed: the host showed "Connecting",
then "Error connecting", and the member's dialog silently went back to the paste step.
Reproduced on one PC with two release-mode instances using the real Metered fetch
(`scripts/dev/release-pair.ps1`). Earlier labs had missed it: they used debug builds with no
relay or a local coturn, and direct links on one PC. Causes and fixes:

1. **The member's attempt died before the host could answer.**
   - The member's ICE starts checking as soon as it has the invite, but the host applies the
     answer only after Admit, often minutes later.
   - Between machines (or relay-only) nothing answers those checks, and ICE reaches `failed`
     after about 17 s. The bridge then closed the peer and the session ended (`NotAdmitted`).
   - On one PC this stayed hidden: the host answers the checks on the local network even
     before it has the answer.
   - **Fix:**
     - A link that never opened is no longer closed on `failed` (`bridge.ts`).
     - The session gets ICE state changes (`TransportEvent::IceState`). WebRTC recovers once
       the host starts its own checks: proven after 26 s and after 4 minutes in `failed`.
2. **No time limit and no reason.**
   - Host: once it admits, the link must open within 45 s, and an ICE `failed` after Admit
     ends it at once. The invite turns "failed" with a reason built from what each text
     carried (types only, never addresses).
   - Member: the same rule starts when the host's checks arrive (`failed` back to `checking`).
     The attempt ends with `ConnectFailed { reason }`, shown as "The host could not connect:
     <reason>".
   - Otherwise the member waits until the invite's lifetime (plus 2 minutes) has passed.
3. **Auth messages before the open were dropped.**
   - The bridge reports "open" only after all three channels are open and its stats call has
     returned. So the host's first auth message can arrive before the member's session has
     seen the open, which is likely over a relay.
   - That message was silently dropped. Both sides then waited 30 s, and the member was never
     admitted (seen with relay + password).
   - **Fix:** up to 8 early auth messages per link are kept and replayed at the open. The test
     `the_host_hello_before_the_member_sees_the_open_is_kept_and_answered` fails without the
     replay.
4. **The member's dialog reset itself.** It showed the waiting step only while the latest
   room state contained the reply. It now keeps the reply, the words and the end reason
   (UI tests).
5. **Text format 2:**
   - Both texts start with the app version, so a different SyncWatch is named even when its
     format differs: "The other side runs a different version of SyncWatch (theirs: X,
     yours: Y)".
   - The invite carries its **lifetime** (`ttl_secs`) instead of a deadline on the host's
     clock. The member never compares clocks: the host alone enforces expiry, on its own
     clock, when the reply arrives.
   - Both texts say whether "Always use relay" is on, and the logs and diagnostics show it.
   - 0.1.0 texts are refused as "a different version" (0.1.0 was never published).
6. **The Metered fetch timed out.** See ADR 0004, revision.

The in-process session tests cover each point with no network:
- the member surviving its own ICE failure;
- the host's 45 s limit with a reason;
- the member's reason after the host's checks;
- a member clock a day ahead;
- the early hello.

The release-mode lab (`two-windows.ps1 -ReleasePair`) runs the `sync` scenario between two
release-optimized test builds with the real Metered setup. Results:
- relay-only with a password: 36/36;
- direct with a password: 36/36;
- relay-only without a password: 36/36;
- with no relay servers, as in the Sandbox: 36/36.

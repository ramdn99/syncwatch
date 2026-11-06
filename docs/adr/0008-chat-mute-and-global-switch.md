# ADR 0008: Chat controls: per-member mute and one global switch

Status: accepted (Milestone 7, Part A). Date: 2026-11-06. Supersedes the chat parts of
ADR 0003 (the per-member chat permission and the hidden-chat option).

## Context

There were four overlapping chat controls: a per-member chat permission, a per-member mute,
a "Chat enabled" switch and an option that hid chat from members. A muted member's window
also still showed that it could chat.

## Decisions

- **Chat is not a permission.** `can_chat` is removed from `Permissions`, from the presets
  ("Only me" = `Permissions::NONE`), the defaults for new joiners, the member menu, the badge,
  sync-core and the tests.
- **Mute / unmute is the per-member control.**
  - A muted member cannot send but still sees all messages.
  - Mutes are keyed by the member's identity, so they survive a reconnect.
  - The mute list (`HostHandover::muted`) moves with the host role on a transfer. It is also
    kept by the designated successor for host succession (ADR 0007, Part B).
- **"Chat enabled" is global.**
  - When off, only the host can send. Every member is refused, muted or not, but everyone still
    sees all messages already sent and everything the host sends.
  - When on, every member can chat except the muted ones. It defaults to on.
  - The host is never muted.
- **The hidden-chat option is gone** everywhere: the setting, the protocol field, the state, the
  UI, the snapshot handling, the tests and the docs. All members always see the chat. A test
  (`crates/protocol/tests/no_hidden_chat.rs`) greps the source for it.
- **Enforcement stays on the host.** A `ChatSend` from a muted member is refused with `Muted`;
  from any member while chat is off, with `ChatDisabled`. Neither is relayed. Muted is checked
  first, so a muted member is told it is muted even while chat is off. Moderation (mute, unmute,
  chat enabled, delete, clear, slow mode) is accepted only from the current host identity. Slow
  mode is unchanged.
- **One authoritative state per member: `ChatState { config: ChatConfig { enabled,
  slow_mode_secs }, muted }`.**
  - The host pushes it to the affected member on every mute or unmute, and to every member
    (each with its own mute flag) on every config change.
  - It is part of every join and reconnect snapshot.
  - A `Muted` or `ChatDisabled` refusal also corrects the member's state, in case it was stale.
  - The member's UI derives the input, placeholder, status line and Send button only from this.
    The texts are "You are muted by the host" and "Chat is turned off by the host"; muted wins
    when both apply. These are normal states, not errors: no toast.
  - The status summary lists permissions without chat and shows "Chat: on / muted / off"
    separately.

## Root cause of "a muted member still looks able to chat"

1. The member's state machine dropped `Mute` and `Unmute` commands (`=> {}`). A member learned
   about its own mute only from a flag inside the broadcast member list.
2. The member's UI never read that flag. It computed "can chat" from the old `can_chat`
   permission and the global switch, and the badge listed "chat" from the permission.
3. A refusal only showed a toast and changed no state.
4. During the fix, the first window run found one more gap of the same class: the new
   `ChatState` event fell through a catch-all in the app's UI mirror, so the state arrived but
   was not applied. A mirror unit test now covers it.

The snapshots did carry the global config, so chat-off was correct after a join. They carried
no mute state.

## Tests

- **sync-core:**
  - a muted member is refused, others are not, and the muted member still sees everything;
  - chat off refuses every member but the host, and everyone still receives;
  - on again re-enables everyone except the muted;
  - mute survives a reconnect and a host transfer;
  - a late joiner gets the flag and its own mute;
  - every change and every snapshot carries the state;
  - a refusal corrects a stale member state.
- **Session:** muted, refused, still muted after a reconnect, unmute.
- **UI:** each combination (on, muted, off, both), and the status line.
- **App mirror:** state updates are applied.
- **Window check:** three windows (`two-windows.ps1 -Scenario chat`), checking the member's real
  input in its own DOM after mute, after a reconnect while muted, after chat off, after joining
  late while chat is off, and after unmute and chat on.

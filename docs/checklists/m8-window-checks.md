# Milestone 8 window checks (done 2026-11-19)

Real mouse and keyboard input inside the agreed window (Thursday 2026-11-19, from 09:55 Cairo
time), in a two-window room (`scripts/dev/two-windows.ps1 -AutoRoom`, direct links, no relay).
Only the SyncWatch windows were captured, by window handle (PrintWindow). The video window and
the app window were captured separately and combined. Frames from the test video are not
stored in the repository; the video checks are described in text.

| Check | Result | Evidence |
|---|---|---|
| Latency next to each member's link | Pass: "Direct · 8 ms" on both sides; the member sees its own RTT on the host row | `images/m8/icons-after-host.png`, `icons-after-member.png` |
| "Muted by you" icon | Pass: after "Mute locally" an eye-off icon appears next to the host's name; "Unmute locally" removes it | `images/m8/check-muted-by-you.png` |
| Copy button for links in chat | Pass: the member sent "Release notes: https://github.com/ramdn99/syncwatch/releases, have a look." The host sees the link as plain text with a copy icon; the trailing comma stays outside the link. Clicking the icon put exactly `https://github.com/ramdn99/syncwatch/releases` on the clipboard | `images/m8/check-chat-link-copy.png` |
| Fullscreen chat overlay above the subtitles | Pass: paused at 03:47 on a bottom subtitle line; the overlay's lowest line (and its input, after Enter) sits at about 32% of the height, well above the subtitle | Described here (video frames not committed) |
| Drag and drop of a .torrent onto Change source | Pass: a sample .torrent dragged from File Explorer onto the Torrent tab is read at once: name, infohash, size, one file, one tracker; no network | `images/m8/check-dnd-torrent.png` |

## Found and fixed during the checks

**The control bar covered the bottom subtitle line in fullscreen while paused.** The seek bar
ran through "Yep, that's our mark." at 03:47. The spec asks this only for the chat overlay,
but it hides dialogue whenever the controls are visible.
- **Fix:** while the control bar is shown, the app raises mpv's subtitle margin
  (`sub-margin-y`) by the bar's height, and lowers it again when the bar hides.
- **Re-check:** the same frame now shows the subtitle above the bar.
- **Test:** `VideoArea.test.tsx`.
- **Limit:** ASS subtitles with their own positioning keep their authored position.

## Icons: before and after

The "before" shots are from 2026-11-14 (before ADR 0013); the "after" shots are from
2026-11-19. Same views: host and member in a room with no video yet.
- `images/m8/icons-before-host.png` / `icons-after-host.png`:
  - section arrows: before, small triangle characters; after, Lucide chevrons;
  - the host badge gains a crown icon;
  - the link line gains a link icon and the latency;
  - the member menu is an ellipsis icon with an accessible name.
- `images/m8/icons-before-member.png` / `icons-after-member.png`:
  - the lock on read-only track lists is a Lucide lock instead of a padlock emoji;
  - the "Shortcuts" button with its keyboard icon is new in Milestone 8.

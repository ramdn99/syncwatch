# Changelog

Each release has a section `## <version> - <date>`. `scripts/release.ps1` uses the text of
that section as the release notes on GitHub and in the update offer.

## 0.1.0 - unreleased

First public release.

- Watch together in sync: everyone plays their own copy of a local file or a torrent;
  play, pause, seek, speed, audio and subtitle tracks stay in sync for the whole room.
- No SyncWatch server: rooms are joined with copy-and-paste invites, over your own
  Metered.ca relay (TURN/STUN) account.
- Chat with clickable video timestamps.
- Torrents stream while they download and stop all traffic at 100%.
- Per-user installer (no administrator rights) with automatic, signed updates from GitHub.

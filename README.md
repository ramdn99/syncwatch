# SyncWatch

Watch videos together, in sync, with friends on other computers. Peer to peer, for Windows 10
and 11 (x64).

- Everyone plays their **own copy** of the video (a local file, or a torrent each person
  downloads). The video itself is never sent to anyone.
- Play, pause, seek, speed, audio and subtitle tracks stay in sync for everyone in the room.
- Chat with clickable video timestamps.
- No SyncWatch server: you connect with your own free Metered.ca TURN/STUN account and invite
  people with a copy-and-paste invite.
- Dark interface, bundled player (libmpv): no codecs or other software to install.

## Install

1. Open the [Releases page](https://github.com/ramdn99/syncwatch/releases) and download
   `SyncWatch-<version>-x64.msi` from the latest release.
2. Run it.

**Windows SmartScreen.** The installer is not code-signed, so Windows may show "Windows
protected your PC". Click **More info**, check that the app is SyncWatch, then click **Run
anyway**. Each release lists the SHA-256 of the installer in `SHA256SUMS` if you want to check
the download.

SyncWatch checks this repository's releases when it starts and offers to update. You can turn
that off in Settings.

The user guide is in [docs/user-guide.md](docs/user-guide.md).

## Build from source

You need Windows x64, Rust (stable, MSVC), Visual Studio Build Tools, Node.js with pnpm, and
PowerShell 7. The full list, the libmpv files and every step are in
[docs/build-guide.md](docs/build-guide.md). In short:

```
pwsh -NoProfile -File scripts/check-dev-env.ps1 -Milestone 1
pwsh -NoProfile -File scripts/fetch-third-party.ps1
pnpm install
cargo test --workspace
pnpm --dir apps/desktop tauri dev
```

Release builds come only from `scripts/build-release.ps1`.

## License

SyncWatch is licensed under the GNU General Public License v3.0 (see [LICENSE](LICENSE)). It
bundles libmpv, a GPL build of mpv that includes FFmpeg and libass, and other components under
their own licenses; see [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

Some video and audio formats may be covered by patents in some countries. See
[docs/legal-notes.md](docs/legal-notes.md) (not legal advice).

# Architecture decision records

One file per significant decision: the context, the decision, what was tested and the
consequences. Newer ADRs may change older ones; the newer one says so.

| ADR | Decision | Milestone |
|---|---|---|
| [0001](0001-video-window.md) | Video in mpv's own borderless window under the transparent main window (inverted ownership) | 1 |
| [0002](0002-native-loading-vulkan.md) | libmpv and the Vulkan loader load by absolute path; bundled loader as fallback | 1 |
| [0003](0003-protocol-and-sync-core.md) | Versioned postcard protocol and a pure, deterministic sync state machine | 2 |
| [0004](0004-connection-setup.md) | Metered TURN/STUN setup, connection test, DPAPI and Credential Manager storage | 3 |
| [0005](0005-signaling-and-bridge-transport.md) | WebRTC through a hidden WebView2 bridge; signaling server (dev and tests only since 0014) | 4 |
| [0006](0006-rooms-and-authentication.md) | Rooms, SPAKE2 + ed25519 authentication, the room session | 5 |
| [0007](0007-real-player-sync-tracks-succession.md) | Real-player sync, shared tracks, external files, chat timestamps, host succession | 6 |
| [0008](0008-chat-mute-and-global-switch.md) | Chat: per-member mute and one global switch | 6 |
| [0009](0009-torrent-piece-prioritization.md) | librqbit engine and streaming piece prioritization | 7 |
| [0010](0010-torrent-stop-at-100.md) | All torrent traffic stops at 100% | 7 |
| [0011](0011-host-provided-torrent-metadata.md) | The host provides torrent metadata; members use the host's file choice | 7 |
| [0012](0012-ready-at-the-room-position.md) | "Ready" means the data needed now; the buffering popup is a real stall | 7 |
| [0013](0013-icons-not-emoji.md) | One icon set, no emoji in the app's own text | 8 |
| [0014](0014-manual-invites.md) | Copy-and-paste invites instead of a signaling server (Metered Realtime Messaging findings recorded) | 9 |
| [0015](0015-msi-installer.md) | Per-user MSI with everything bundled, offline WebView2, "remove my data" | 9 |
| [0016](0016-updates-from-github.md) | Signed in-app updates from GitHub Releases | 9 |

Related documents:
- [threat model](../threat-model.md);
- [security review](../security-review.md);
- [protocol overview](../protocol.md);
- [build guide](../build-guide.md);
- [release guide](../release-guide.md).

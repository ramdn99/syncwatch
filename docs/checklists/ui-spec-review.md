# UI spec review (Milestone 8, B9)

Date: 2026-11-18. The UI was compared item by item against spec sections 4 (branding and dark
theme), 6 (product requirements), 7 (room UI layout) and 9 (chat UI), mainly from the source
and the tests, and by hand for the parts covered in the 2026-11-14 window (Change source
dialog). Where something differed, it was fixed in the same commit unless a decision record
says otherwise.

## Fixed in this review

| Spec item | Was | Now | Evidence |
|---|---|---|---|
| 7: members list shows "direct/relayed/reconnecting **with latency**" | Link type only | "Direct · 42 ms" / "Relayed · 118 ms": each member's clock-sync RTT to the host (protocol v3, `MemberInfo.rtt_ms`); a member sees its own RTT on the host row. Members report it again only when it moves by 10 ms and a quarter | `member_rtt_reaches_the_members_list` (sync-core), `only_noticeable_rtt_changes_are_reported`, `MembersPanel.test.tsx` |
| 7: members list "muted icon" | Icon only for a host chat mute | Also an icon when you muted someone locally ("Muted by you") | `MembersPanel.test.tsx` |
| 9: "URLs shown as text with a copy action" | Plain text, no copy action | Each URL keeps plain text styling (not link-coloured, never opened) with a small "Copy link" button | `ChatText.test.tsx` |
| 7: fullscreen chat overlay "never covering subtitles" | Bottom edge 96 px above the screen bottom: inside the band of two- and three-line subtitles | Bottom edge above three subtitle lines at the current subtitle size (29% of the height at 100%, up to 60% at 200%) | `ChatOverlay.test.tsx` |
| 11 (shown in the UI through Copy diagnostics): no full paths | Logs had full paths (web view folder, libmpv path, bundled Vulkan path, and any path inside mpv's warnings); Copy diagnostics did not shorten paths | Every absolute path is shortened to `...\name` in logs (scrubber) and in Copy diagnostics | `logs_carry_no_full_paths`, `redact.test.ts` |
| 12: test that chat content never reaches logs or diagnostics | No test | Test at the most verbose log level | `chat_content_never_reaches_logs_or_diagnostics` |
| 3/13 portable mode on another PC | The relay credentials (DPAPI, per user) cannot be decrypted there, and the app said "could not be saved" while still claiming to be set up | Clear message, and the app goes back to setup | `credentials_from_another_pc_send_the_user_back_to_setup` |

## Decided differences (not changed)

| Spec item | Decision |
|---|---|
| 9: per-member `can_chat` permission and "Hide chat from members" | Replaced by a host chat mute and one global "Chat enabled" switch; hidden chat removed everywhere (ADR 0008, test `no_hidden_chat.rs`). |
| 4: links use info colour with underline | There are no links in the app. Chat URLs are deliberately not links (spec section 9). |

## Checked and matching

| Spec item | Where |
|---|---|
| 4: tokens (background, surface, elevated, border, text, muted, disabled, accent states, status colours) | `styles.css`, `theme/tokens.ts`: values equal the spec |
| 4: WCAG AA enforced by an automated test; member colour chips with accessible contrast | `theme/usage.test.ts`, `theme/tokens.test.ts` |
| 4: dark only, `color-scheme: dark`, no white flash, bundled fonts, Space Grotesk headings and room codes, tabular figures | `theme/usage.test.ts` (B1) |
| 6: Change source dialog (two tabs, magnet validation, dn and infohash, multi-file picker, restart warning, member confirm and decline) | `ChangeSourceDialog.test.tsx`; hands-on `change-source-clickthrough.md` |
| 6: "Choose my file" without permission, mismatch warning, "Choose a different file" | `RoomDialogs.tsx`, `HostControls.tsx`, sync-core media tests |
| 7: three columns, resizers, collapse buttons, `[` `]`, per-section collapse, fullscreen | `RoomView.tsx`, alignment check script |
| 7: per-member menu: permissions, mute, kick, ban, Make host (host); mute locally (members) | `MembersPanel.tsx` |
| 7: host controls: presets (Everyone, Only me, Custom), Change source, track lists, ready-gate options, torrent block; members see a read-only summary | `HostControls.tsx`, `TorrentPanels.tsx` |
| 7: track lists collapsed on each load, header with title, selection and count, `aria-expanded`, "Add ... file..." first (host), "+" in the header, lock tooltip for members | `TrackList.test.tsx` |
| 7: shortcuts Space, arrows, F, M, `[`, `]`, C; track shortcuts host only; typing in chat never triggers them; help overlay | `lib/shortcuts.ts`, `ShortcutsHelp.test.tsx` |
| 7: overlay controls auto-hide with a dark scrim | `VideoArea.tsx` |
| 7: friendly error states with one-click actions and Copy diagnostics | `strings/errors.test.tsx` (B5) |
| 7: "View as host/member" dev switch | `RoomView.tsx` |
| 9: chips `mm:ss` / `h:mm:ss`, seek-bar markers, jump to latest with unread badge, "near current time" filter, spoiler guard, "Now playing" divider, delete, clear, slow mode, chat switch, local mute | `ChatPanel.tsx`, `lib/chat.ts`, `room.rs` |
| 9: ready gate shows non-ready members by name and reason; "resume without" | `RoomView.tsx`, sync-core gate tests |

## Not checked by hand

The window for mouse and keyboard checks (2026-11-14, 14:00 to 15:00 Cairo time) covered only
the Change source dialog. These changes from this review were not seen on screen; they are
covered by the tests above:
- the latency text;
- the "Muted by you" icon;
- the copy-link button;
- the overlay position.

Check them in the next hands-on session, or on the clean VM in Milestone 9.

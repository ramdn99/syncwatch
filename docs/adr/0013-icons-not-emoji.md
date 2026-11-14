# ADR 0013: One icon set, no emoji in the app's own text

Status: accepted (Milestone 8). Date: 2026-11-14.

## Context

The UI used emoji and pictograph characters as icons: 🔇 🔊 🔒 for mute, volume and lock; ▶ ❚❚
for play and pause; ⤢ ⤡ for fullscreen; ⋯ ✕ ▸ ▾ ● « » ←. They render differently on every
system, some fall back to colour emoji fonts, and they look unprofessional.

## Decision

- **One bundled icon set: Lucide** (`lucide-react` 1.66.0, exact).
  - **License:** ISC; the icons derived from Feather are MIT. Both are compatible with GPL-3.0.
    The text is in `third_party/licenses/`.
  - **Version:** the newest release at least 14 days old.
  - **Bundling:** named imports only (tree-shaken, about 30 icons in the bundle), no CDN, no
    remote fetching. No second icon set.
- **One map: `src/ui/icons.tsx`.** Semantic names (`play`, `muted`, `lock`, `direct`, `relayed`,
  `reconnecting`, `ready`, `missingFile`, `host`, `kick`, `ban`...) map to icons, so they can be
  swapped in one place.
  - The `Icon` component has a size scale (14, 16, 20, 24 px) and one stroke width (2), and
    uses `currentColor`, so icons follow the theme tokens and the contrast checks.
  - It is `aria-hidden` by default; with `label`, it is an image with that accessible name.
- **Icon-only buttons** have `aria-label` and a tooltip (`title`), and get the global focus
  ring.
- **Status never relies on colour alone:**
  - the connection state (direct, relayed, reconnecting) has a distinct icon and text;
  - the media status has an icon and text;
  - the host badge has a crown and "HOST";
  - chat-muted shows an icon with an accessible name.
- **Plain typography stays** (·, …, ’, —, –, the • of a masked field), because the bundled Inter
  font renders it.
- **Chat messages typed by people are user content.** They are rendered exactly as typed;
  only the app's own text is covered.

## Enforcement

- **`apps/desktop/scripts/emoji-scan.mjs`** scans the UI sources and strings, `index.html`, the
  Tauri config (window title), every Rust source (the strings sent to the UI, toasts), the
  scripts (the installer to come) and the built bundle.
  - **What fails it:** Extended_Pictographic code points, variation selector 16, zero-width
    joiners, and the symbol blocks used as icons.
  - **Allowlist:** `apps/desktop/emoji-allowlist.txt`, with a reason per entry. The only entry
    today is a Rust test input.
  - **Where it runs:** as a CI step after the frontend build, and as a UI test for the sources.
- **`src/ui/icon-buttons.test.tsx`:**
  - every icon-only `<button>` in the source has `aria-label` and `title`;
  - every rendered button has an accessible name;
  - icons are decorative unless labelled, and use `currentColor`.
- **`src/ui/chat-emoji.test.tsx`:** a typed message with emoji, skin tones and ZWJ sequences
  renders unchanged.

There is no tray icon or system notification text today. The window title and the taskbar
entry are "SyncWatch".

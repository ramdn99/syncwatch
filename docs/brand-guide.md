# Brand guide

SyncWatch is dark only: there is no light theme and no theme switch (spec section 4).

## Mark

- `assets/branding/icon.svg`: four amber rings and a center dot. This is the master file.
  **Never edit it.**
- Everything else is generated from it with `pnpm brand`
  (`scripts/generate-brand-assets.mjs`) into `assets/branding/generated/`:
  - PNGs from 16 to 1024 px;
  - `icon.ico`;
  - `icon-small.svg`, with a thicker outer ring and dot for 16 to 32 px;
  - the logo lockups (`logo-lockup.svg`, `logo-lockup-on-dark.svg`, and PNGs 512 and 1024 px
    wide).
- The wordmark is "SyncWatch" in Space Grotesk Bold, converted to outlines, so the lockup
  needs no font.
- Keep clear space around the mark of at least the width of its outer ring. Don't recolor,
  stretch, add effects or put it on light or busy backgrounds.

## Colors

The source is `apps/desktop/src/theme/tokens.ts`. `tokens.test.ts` keeps `styles.css` in step
and enforces WCAG AA contrast.

| Token | Hex | Use |
|---|---|---|
| bg | `#0B0B0D` | Window background (also before the first paint: no white flash) |
| surface | `#141417` | Panels |
| elevated | `#1C1C21` | Menus, dialogs, cards |
| border | `#2A2A31` | Dividers |
| borderControl | `#6E6E7A` | Input and button outlines (3:1 against the background) |
| text | `#F2F2F4` | Main text |
| muted | `#9A9AA5` | Secondary text |
| disabled | `#5C5C66` | Disabled controls only |
| accent | `#E8A33D` | Amber: the mark, primary buttons, focus, the seek bar |
| accentHover / accentPressed | `#F0B25A` / `#C98A28` | Primary button states |
| onAccent | `#141414` | Text on amber |
| success / warning / danger / info | `#4CC38A` / `#E8A33D` / `#F26B6B` / `#6AA8F7` | Status only, always with text or an icon, never color alone |
| letterbox | `#000000` | Around the video |

## Type

| Face | Use |
|---|---|
| Inter (variable) | All interface text |
| Space Grotesk | The wordmark and a few headings (`--font-brand`) |
| Noto Sans | Subtitles (Latin, Greek, Cyrillic); Windows fonts handle other scripts |

All fonts are bundled under the SIL OFL (`THIRD-PARTY-NOTICES.md`). The app never loads a
remote font.

## Icons and words

- Icons: Lucide only (ADR 0013). No emoji in SyncWatch's own text; people's chat is theirs.
- Writing:
  - short, plain sentences that say what happened and what to do next;
  - name buttons by what they do ("Update now", "Admit", "Copy invite");
  - errors are friendly and offer **Copy diagnostics**;
  - Metered help text lives in `apps/desktop/src/strings/connection-help.ts`.

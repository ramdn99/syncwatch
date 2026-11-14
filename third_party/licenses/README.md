# Third-party licenses: bundled UI assets

This report covers what is bundled into the user interface. The complete license report for
the installer (Rust crates, libmpv and its components, the Vulkan loader, the fonts) is built in
Milestone 9.

| Component | Version | License | Text | Notes |
|---|---|---|---|---|
| Lucide icons (`lucide-react`) | 1.66.0 (exact) | ISC; the icons derived from Feather are MIT | `lucide-react-LICENSE.txt` | Named imports only, tree-shaken (about 30 icons in the bundle); no CDN. Version and license checked on the npm registry on 2026-11-14; the newest release at least 14 days old was chosen. |
| Inter | pinned in `third_party/pins.json` | SIL OFL 1.1 | downloaded to `third_party/cache/licenses` | Bundled in `public/fonts`. |
| Space Grotesk | pinned in `third_party/pins.json` | SIL OFL 1.1 | downloaded to `third_party/cache/licenses` | Bundled in `public/fonts`. |
| Noto Sans | pinned in `third_party/pins.json` | SIL OFL 1.1 | downloaded to `third_party/cache/licenses` | Subtitle fallback font for mpv/libass. |

All of them are compatible with GPL-3.0.

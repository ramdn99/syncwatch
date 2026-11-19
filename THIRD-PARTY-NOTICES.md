# Third-party notices

SyncWatch is licensed under the GPL-3.0 (see `LICENSE`). The installer and the portable zip
also ship the components below, each under its own license. The license texts are installed
with the app in the `licenses` folder. The full list of Rust crates and their licenses is
generated at release time (`licenses\rust-crates.html`, cargo-about).

| Component | Version / source | License | Notes |
|---|---|---|---|
| libmpv (`libmpv-2.dll`) | shinchiro/mpv-winbuild-cmake release `20261101`, mpv `v0.41.0-1374-g48fca90e8` (`docs/libmpv-build.md`) | GPL (this build includes GPL components) | Bundles FFmpeg, libass and other libraries under GPL/LGPL-compatible licenses. Source: https://github.com/mpv-player/mpv and https://github.com/shinchiro/mpv-winbuild-cmake |
| FFmpeg (inside libmpv) | as built by the above | GPL build | https://ffmpeg.org/ |
| libass (inside libmpv) | as built by the above | ISC | https://github.com/libass/libass |
| Vulkan loader (`vulkan\vulkan-1.dll`) | LunarG Vulkan Runtime (pinned in `third_party/pins.json`) | Apache-2.0 AND MIT | Used only when Windows has no Vulkan driver loader |
| Space Grotesk | pinned in `third_party/pins.json` | SIL OFL 1.1 | UI headings |
| Inter | pinned in `third_party/pins.json` | SIL OFL 1.1 | UI text |
| Noto Sans | pinned in `third_party/pins.json` | SIL OFL 1.1 | Subtitle fallback font |
| Lucide icons (`lucide-react` 1.66.0) | npm | ISC (icons derived from Feather: MIT) | `third_party/licenses/lucide-react-LICENSE.txt` |
| Microsoft Edge WebView2 Runtime | Microsoft | Microsoft software license | Installed by the SyncWatch installer if missing; not redistributed in source form |
| Rust crates (Tauri, librqbit, rustls, ring, tokio and others) | `Cargo.lock` | MIT, Apache-2.0, BSD, ISC, MPL-2.0 and others | Full report: `licenses\rust-crates.html` in each release |
| npm packages (React, Tailwind CSS and others) | `pnpm-lock.yaml` | MIT and others | Bundled into the app's web UI |

## Source code

The complete source of SyncWatch is this repository. The libmpv build is unmodified; its exact
source and build recipe are named above. A release tag in this repository corresponds to each
published installer.

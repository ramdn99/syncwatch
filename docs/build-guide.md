# SyncWatch build guide

All scripts target PowerShell 7 (`pwsh`). Commands are listed one per line. If PowerShell blocks
`pnpm` (script execution policy), use `pnpm.cmd` instead.

## 1. Check the environment

```
pwsh -NoProfile -File scripts/check-dev-env.ps1 -Milestone 1
```

Each check belongs to the first milestone that needs it. Checks up to `-Milestone` FAIL when missing;
later ones only WARN. Tools are found on PATH, on the user/machine PATH in the registry ("open a new
terminal" hint), and at known locations. The script never prints secrets.

## 2. Fetch pinned third-party files

```
pwsh -NoProfile -File scripts/fetch-third-party.ps1
```

This downloads the fonts (Space Grotesk, Inter, Noto Sans; OFL) and the LunarG Vulkan loader into
`third_party/cache` and verifies every SHA-256 from `third_party/pins.json`. It mirrors the fonts to
`apps/desktop/public/fonts` and checks `LIBMPV_DIR` read-only. It is idempotent.

## 3. JavaScript dependencies and brand assets

```
pnpm.cmd install
pnpm.cmd brand
```

`pnpm brand` regenerates `assets/branding/generated/*` and the Tauri icons from the never-edited
`assets/branding/icon.svg` (reproducible output).

## 4. Build, lint, test

```
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
pnpm.cmd --dir apps/desktop lint
pnpm.cmd --dir apps/desktop test
pwsh -NoProfile -File scripts/verify-imports.ps1
pwsh -NoProfile -File scripts/tests/verify-imports.tests.ps1
```

## 5. Run the app (development)

```
$env:SYNCWATCH_DEV_MEDIA = 'D:\media\test.mkv'
pnpm.cmd --dir apps/desktop tauri dev
```

Development environment variables:

| Variable | Effect |
|---|---|
| `SYNCWATCH_DEV_MEDIA` | Debug builds: open this file once the UI is ready |
| `SYNCWATCH_FORCE_BUNDLED_VULKAN=1` | Skip the system `vulkan-1.dll` and load `<exe>\vulkan\vulkan-1.dll` |
| `SYNCWATCH_DEV_LAYOUT_PROBE=1` | Write `%LOCALAPPDATA%\SyncWatch\dev\layout-probe.json` after every video placement |
| `SYNCWATCH_LOG` | App log level (`trace`/`debug`/`info`/`warn`/`error`). Third-party crates stay at `warn` |

Logs go to `%LOCALAPPDATA%\SyncWatch\logs\syncwatch.log` (in portable mode: `<exe>\data\logs`).

## 6. Secret scan

```
pwsh -NoProfile -File scripts/secret-scan.ps1 -Mode All
```

The git hooks in `scripts/git-hooks` run it before every commit (`-Mode Staged`) and every push
(`-Mode History`). Enable them once per clone:

```
git config core.hooksPath scripts/git-hooks
```

## 7. Alignment check (only when nobody is using the machine)

`scripts/dev/alignment-check.ps1` moves the real mouse and sends real keys. See the script header.

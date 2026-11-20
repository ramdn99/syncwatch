# ADR 0015: Per-user MSI installer

Status: accepted (Milestone 9, 2026-11-19).

## Context

The maintainer decided:
- the installer is an `.msi` that installs everything SyncWatch needs, with nothing else
  required from the user;
- releases go to GitHub with a version tag;
- there is no code signing certificate.

Tauri 2 builds MSIs with WiX Toolset 3.14.

## Decision

### Per-user MSI, no administrator rights

Tauri's own MSI template is `InstallScope="perMachine"`: it installs under Program Files and
needs an administrator (a UAC prompt). SyncWatch ships a copy of that template
(`apps/desktop/src-tauri/wix/main.wxs`, from tauri-bundler 2.10.4, MIT OR Apache-2.0). Every
change is marked `SyncWatch:` in the file:
- **Per-user install:**
  - `InstallScope="perUser"` and `InstallPrivileges="limited"`: the package asks for no
    elevation (summary word count 10);
  - `ALLUSERS=2` with `MSIINSTALLPERUSER=1`, Microsoft's "single package authoring": Windows
    Installer maps the Program Files folder to `%LOCALAPPDATA%\Programs`, so the app installs to
    `%LOCALAPPDATA%\Programs\SyncWatch` without a UAC prompt.
- **Desktop shortcut:** optional, through the property `INSTALLDESKTOPSHORTCUT` (default `1`;
  `0` skips it in a silent install). The Start menu shortcut is always created.
- **Removing your data on uninstall:** `msiexec /x <msi> REMOVEUSERDATA=1` runs
  `syncwatch.exe --cleanup-user-data` before the files are removed. This never happens on an
  upgrade.

`scripts/verify-package.ps1` checks `ALLUSERS`, `MSIINSTALLPERUSER` and the privilege bit.
Whether a standard (non-admin) user really gets no prompt has to be checked on a standard
account; Windows Sandbox runs as an administrator (`docs/clean-machine-test.md`).

### WebView2: offline installer embedded

- `webviewInstallMode: offlineInstaller` embeds Microsoft's Evergreen standalone installer.
  The MSI runs it only when no WebView2 runtime is registered, so no internet is needed.
- **Cost:** 205 MB of the 262 MB MSI (SyncWatch 0.1.0).
- **Not pinned by hash:** Microsoft's link always serves the current runtime.
  `verify-package.ps1` instead checks that the embedded file is the one the bundler downloaded
  and that it carries a valid Microsoft Authenticode signature.
- **Alternative**, if size matters more than offline installs: `embedBootstrapper`. That makes
  an MSI of about 58 MB, but it downloads the runtime during install (it needs internet, and
  only when WebView2 is missing).
- Windows 11 always has WebView2; Windows 10 usually does through Windows Update.

### Upgrades

- **UpgradeCode:** fixed at `b04c9e7e-3415-4c46-8dcf-6b4e8ddd2472` in `tauri.conf.json`. It
  must never change.
- **Major upgrade:** Tauri's `MajorUpgrade` (`Schedule="afterInstallInitialize"`) removes the
  old version and installs the new one in one transaction.
- **Downgrades** are refused with a message, and same-version reinstalls are allowed.
- The MSI version is the app version (`major.minor.patch`, each part within the MSI limits:
  255.255.65535).

### Removing user data

An MSI cannot easily ask a question during uninstall. So:
- **Settings, About, "Remove all my SyncWatch data...":** after a confirmation, SyncWatch starts
  `syncwatch.exe --cleanup-user-data --wait` and quits. The helper waits until the WebView2
  profile is free (up to 60 s), then removes:
  - the data folder (`%LOCALAPPDATA%\SyncWatch`, or `<exe>\data` in portable mode);
  - the two Credential Manager entries (relay setup and identity).

  It never touches the install folder (test `removes_the_data_folder_and_only_it`).
- **Uninstall with `REMOVEUSERDATA=1`** does the same, from the command line.
- A normal uninstall keeps the data. The user guide says so.

### What the package contains (`verify-package.ps1` checks every item)

- **The app:**
  - `syncwatch.exe` (static CRT, rustls only);
  - `libmpv-2.dll` (the pinned shinchiro build);
  - `vulkan\vulkan-1.dll`;
  - `fonts\NotoSans-*.ttf`;
  - `default-config.toml`.
- **`licenses\`:**
  - SyncWatch GPL-3.0;
  - third-party notices;
  - GPL and codec notes;
  - the Rust crate report (cargo-about);
  - mpv Copyright and LICENSE.GPL;
  - FFmpeg LICENSE.md and COPYING.GPLv3;
  - libass COPYING;
  - the Vulkan runtime license;
  - the Noto Sans, Inter and Space Grotesk OFL;
  - Lucide.

  The license files are pinned by SHA-256 in `third_party/pins.json`.
- **Not separate files:** the UI fonts (Space Grotesk, Inter) and Lucide are part of the app's
  embedded web assets.
- **`default-config.toml`:** read only on the first start, as default settings. It never holds
  credentials or server addresses (a test and the package check).
- **The WebView2 offline installer:** inside the MSI, as a binary that runs only when needed.

### Build path and prerequisites

- `scripts/build-release.ps1` is the only release build. It stages the files, generates a
  Tauri config overlay with the resources, runs `tauri build --bundles msi`, and makes:
  - `SyncWatch-<version>-x64.msi`;
  - `SyncWatch-Portable-<version>-x64.zip` (an extra artifact, not attached to releases);
  - `SHA256SUMS`.
  It keeps every step's full output in `dist\release\<version>\logs`.
- **Nothing to install or enable on the build machine:**
  - Tauri downloads WiX 3.14 by itself and verifies it by SHA-256;
  - the MSI built on Windows 11 with the default optional features;
  - cargo-about 0.9.3 was already present for the license report.

## Sandbox findings (2026-11-20)

- A per-user install's Add/Remove Programs entry is under **HKLM** `...\Uninstall\{ProductCode}`.
  The product data is under the user's SID in `Installer\UserData`, and it is published in
  HKCU `Installer\Products`. Check "installed" through the Windows Installer API, not one
  registry key.
- The major upgrade leaves exactly one product. `CleanupUserData` runs deferred and
  impersonated, as the signed-in user, before `RemoveFiles`. It removes the data folder and
  the Credential Manager entries, and writes `%TEMP%\SyncWatch-cleanup.log`. Without
  `REMOVEUSERDATA`, everything is kept.
- Each install spends about 2 minutes in Windows Installer's software restriction policy check
  of the unsigned 337 MB package (in the sandbox). Details: `docs/clean-machine-test.md`.

## Consequences

- No UAC prompt on install, upgrade (including the in-app updater) or uninstall.
- One install per Windows user. A second user installs it again.
- The MSI is large because of the offline WebView2 installer: 262 MB with the 205 MB
  installer of 2026-11-19, 337 MB with the 280 MB one Tauri downloaded on 2026-11-20. Tauri
  fetches Microsoft's current installer at each build, so it is not pinned by hash.
  `verify-package.ps1` checks instead that the embedded file carries a valid Microsoft
  signature.
- Unsigned: Windows SmartScreen warns on first download ("More info", then "Run anyway").
  The README and the user guide explain it.
- The template is a copy. When Tauri's template changes, diff it against
  `tauri-bundler/src/bundle/windows/msi/main.wxs` and carry the `SyncWatch:` changes over.

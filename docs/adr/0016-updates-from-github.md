# ADR 0016: Updates from GitHub Releases

Status: accepted (Milestone 9, 2026-11-19).

## Context

The maintainer decided:
- SyncWatch checks GitHub for updates when it opens and offers "Update now" or "Later";
- each release is an MSI on the public repository's Releases page;
- there is no code signing certificate;
- updates are signed with a minisign key pair that the maintainer generates;
- the app never contains a GitHub token.

## Decision

Our own small updater (`apps/desktop/src-tauri/src/updater.rs`), not Tauri's updater plugin.
The plugin does not let us:
- restrict every redirect to an allowlist;
- cap the download size;
- authenticate the manifest's version claim.

The signature format is Tauri's: keys from `tauri signer generate`, signatures from
`tauri signer sign`, verified with `minisign-verify` 0.3.0.

### Flow

1. **Check.** After the first paint, and only if **Check for updates when SyncWatch starts** is
   on (default), the app fetches `latest.json` and `latest.json.sig` from
   `https://github.com/ramdn99/syncwatch/releases/latest/download/`.
   - That standard HTTPS request is the only data sent.
   - Settings, About also has **Check now**.
2. **Manifest.** It must be signed by the maintainer's key (only the public key is in the
   app: `apps/desktop/src-tauri/updater-pubkey.txt`).
   - It contains the version, notes, and for `windows-x86_64`: the MSI URL, its SHA-256 and
     its own signature.
   - Only a **strictly newer** semantic version is offered. Because the manifest is signed,
     its version claim cannot be changed to push an old MSI as an update.
   - The MSI URL must be HTTPS on an allowed host and end in `/SyncWatch-<version>-x64.msi`.
3. **Offer.** A card on the home screen (never inside a room, never modal) shows
   "Version X is available", the notes, and **Update now**, **Later** and **Skip this
   version**.
   - **Later** asks again at the next start.
   - **Skip** is stored and only that version is skipped.
4. **Download** with a progress bar and **Cancel**. The file must match the size limit, the
   SHA-256 and the MSI signature, or it is deleted.
5. **Install.** **Restart to update** checks the file once more, then starts
   `%SystemRoot%\System32\msiexec.exe /i <msi> /passive AUTOLAUNCHAPP=True` and quits.
   - The per-user MSI's major upgrade removes the old version and installs the new one, with
     no UAC prompt (ADR 0015).
   - The MSI then starts SyncWatch again.

### Network rules

| Rule | Value |
|---|---|
| Hosts | `github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`; HTTPS only, default port |
| Redirects | at most 5; every hop must be on the list, or the request fails before it is made |
| Timeouts | connect 10 s; manifest and signature 20 s in total; download read timeout 30 s |
| Size limits | manifest 64 KiB, signature 4 KiB, MSI 600 MiB |
| Identity | User-Agent `SyncWatch/<version> (update check)`; no token, no cookies, no account |
| Local test server | only in debug builds and test MSIs (`SYNCWATCH_TEST_BUILD`, `build-release.ps1 -TestBuild`), through `SYNCWATCH_DEV_UPDATE_URL` (loopback only) and `SYNCWATCH_DEV_UPDATE_PUBKEY` |

### Keys

- The maintainer generates the key once (`docs/release-guide.md`, step a), keeps the private
  key and its password out of the repository (for example `D:\keys\` and a password manager),
  and puts only the public key into `updater-pubkey.txt`.
- Until then the file holds a placeholder, and the app reports "Updates are not set up in this
  build".
- `scripts/release.ps1` signs the MSI (bound to its version with `--app-version`) and
  `latest.json`. The password comes from a prompt and never appears on a command line.

## Tests

- **Unit tests**, with signed fixtures from a throwaway key:
  - a signed newer manifest is offered;
  - same or older versions are refused;
  - unsigned, tampered, wrongly signed and too-large manifests are refused;
  - downloads are checked by hash and signature;
  - only GitHub hosts over HTTPS are allowed;
  - the compiled-in key is valid or the placeholder.
- **Local server test** (`local_server_newer_older_unsigned_tampered_and_redirects`): the real
  HTTP client against 127.0.0.1:18093.
  - Newer: offered, downloaded and verified, with progress reported.
  - Wrong key: refused.
  - Cancel: stops the download.
  - Older: not offered.
  - Unsigned: refused.
  - A redirect to another host: refused.
  - A tampered MSI: refused.
- **End to end:** test MSIs 0.0.1 and 0.0.2 signed with a fresh test key, installed and updated
  inside Windows Sandbox (`docs/clean-machine-test.md`). This keeps the developer's own
  SyncWatch data untouched.

## Sandbox proof (2026-11-20)

0.0.1 updated itself to 0.0.2 through this path:
1. It checked a local server, downloaded the update, and verified the signature and hash.
2. It started `msiexec` and quit 9 s after starting.
3. The MSI replaced 0.0.1 (one product left).
4. `LaunchApplication` started 0.0.2 as the signed-in user, 126 s after 0.0.1 quit. Almost
   all of that time is Windows' software restriction policy check of the unsigned package.

`docs/clean-machine-test.md` has the results; the evidence is in `dist\sandbox-results`. The
relaunched app does not inherit the old process's environment, since it is started by the
MSI. This is irrelevant for users.

## Consequences

- An update is only as trustworthy as the maintainer's private key. If it leaks, rotate it:
  publish a release with the new public key, signed with the old key. Users who skip that
  release keep the old key.
- GitHub sees the update check's IP address and User-Agent, as for any download.
- Unsigned MSI (no code signing): SmartScreen does not warn on in-app updates, because the
  file is not downloaded through a browser, but it does on manual downloads.

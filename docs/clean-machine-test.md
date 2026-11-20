# Clean-machine test (Windows Sandbox)

This proves that the installer contains everything on a Windows that has never seen
SyncWatch, and that install, update and uninstall work. Everything runs in Windows Sandbox,
so your own SyncWatch data (`%LOCALAPPDATA%\SyncWatch`, your relay setup) is never touched.

## Honest limits

- **Nothing persists.** Closing the sandbox deletes everything in it, including the relay
  setup you type in. The only exceptions are the writable results folder of the automated
  test and what you copy out.
- **The sandbox user is an administrator** (`WDAGUtilityAccount`). The installer asks for no
  administrator rights, and the automated test checks that it writes nothing outside the
  user profile. A real standard-user account is still untested.
- **One sandbox at a time.** Windows runs only one. Two-person tests (invites, rooms) need
  your normal PC as the other side: run your development build there (it uses
  `SyncWatch-dev-*` folders in debug builds), or a second PC.
- **The no-WebView2 path is unproven.** Windows 11 (and so its sandbox) already has the
  WebView2 runtime. The MSI's embedded offline installer only runs where WebView2 is
  missing, which needs a Windows 10 machine without it. That was ruled out of scope (no
  Windows 10, ISO or Hyper-V).
- **With vGPU disabled, the sandbox has no GPU driver and no system `vulkan-1.dll`.** That is
  exactly the case the bundled Vulkan loader and mpv's D3D11 fallback are for. The video then
  renders in software (WARP): slow, but it must work.

## One-time setup

Windows Sandbox is an optional Windows feature. Turning it on needs administrator rights and
a restart.

1. Open PowerShell **as administrator** and run:

```
Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All
```

2. Restart Windows.
3. Check that it is on:

```
Test-Path "$env:windir\System32\WindowsSandbox.exe"
```

## Prepare the files (on your PC, normal PowerShell 7)

```
pwsh -NoProfile -File scripts/sandbox-prepare.ps1 -IncludeRelease
```

This:
- builds two **test** MSIs (0.0.1 and 0.0.2), about 10 minutes;
- signs them with a throwaway key and then deletes that private key;
- writes a signed update manifest for 0.0.2 that points to a local server inside the sandbox;
- copies the latest release MSI (from `dist\release`) for the hands-on part.

Everything goes to `dist\sandbox`, which is mapped **read-only** into the sandbox as
`C:\SyncWatchTest`. Test MSIs accept a local update server and a test key; they are marked
`TEST-BUILD` and `scripts/release.ps1` never publishes them.

The `.wsb` files contain absolute paths (`D:\dev\syncwatch\...`, because Windows Sandbox
does not accept relative ones). Edit them if your repository is elsewhere.

## Part 1: automated install, update and uninstall (about 5 minutes)

1. Double-click `scripts\sandbox-auto.wsb`. Don't touch the sandbox window while it runs.
2. The sandbox runs `auto-test.ps1` at logon:
   1. Installs 0.0.1 silently.
   2. Checks the files, the per-user registration, and that nothing went to Program Files.
   3. Starts the local update server and opens SyncWatch 0.0.1 with the test update source.
   4. The app checks for the update, downloads it, verifies the signature and hash, and
      installs it like **Update now** and **Restart to update**.
   5. Waits for 0.0.2 to be registered and running.
   6. Uninstalls with `REMOVEUSERDATA=1`.
   7. Checks that the program, the data folder and the Credential Manager entries are gone.
3. Notepad opens `result.txt`. The last line says `RESULT: ALL PASSED` or how many checks
   failed. The same file, the MSI logs and the app logs are on your PC in
   `dist\sandbox-results\auto-<time>\`.
4. Close the sandbox.

The real GitHub path (a published release updating to the next one) is step (e) of the
release guide, after the first two real releases.

## Part 2: hands-on checklist (about 45 minutes)

Double-click `scripts\sandbox-test.wsb`. The sandbox opens `C:\SyncWatchTest` in Explorer;
the release MSI is in `release\`. Clipboard sharing is on, so you can copy invites between
the sandbox and your PC.

Media: copy `D:\media\test.mkv` into the sandbox, for example by copying it in Explorer on
your PC and pasting it into the sandbox's Downloads folder. The original stays untouched.

| # | Check | How | Expected |
|---|---|---|---|
| 1 | Install | Run `release\SyncWatch-<v>-x64.msi` | SmartScreen may warn: More info, Run anyway. No UAC prompt. Installs to `%LOCALAPPDATA%\Programs\SyncWatch`; Start menu entry |
| 2 | First launch | Start SyncWatch | Dark window, no white flash, the connection setup screen |
| 3 | Metered setup | Paste your ICE servers or API key (your own account) | Test passes. Uses a little quota: keep it to one test |
| 4 | Video, bundled Vulkan, D3D11 fallback | Open `test.mkv` | Plays (slowly, software rendering). In `%LOCALAPPDATA%\SyncWatch\logs`, the latest log has `native libraries loaded` with `vulkan=Bundled`; no `vulkan-1.dll` exists in `C:\Windows\System32` |
| 5 | Non-Latin subtitles | Pick an Arabic, Japanese or Russian subtitle track of `test.mkv` | Readable text, no boxes |
| 6 | Invite: sandbox hosts | Create a room in the sandbox; join from your PC's dev build with the invite; paste the reply back; compare the four words; Admit | Both in the room; play, pause and seek follow |
| 7 | Invite: PC hosts | The other direction | Same |
| 8 | Torrent to 100% | In a room, open a small open-licensed torrent (for example Blender's "Sintel") | Streams while downloading; at 100%: "Downloading and sharing have stopped. No further traffic." |
| 9 | Update check | Settings, About, **Check now** | "SyncWatch is up to date" (or the offer, if a newer release exists). With the switch off, nothing is checked at start |
| 10 | Remove data | Settings, About, **Remove all my SyncWatch data...** | SyncWatch closes; `%LOCALAPPDATA%\SyncWatch` is gone; `cmdkey /list` shows no SyncWatch entries |
| 11 | Uninstall | Windows Settings, Apps, SyncWatch, Uninstall | `%LOCALAPPDATA%\Programs\SyncWatch` is gone; no Start menu entry |

Note any failure with the step number. **Copy diagnostics** (Settings, About) gives a report
with credentials, keys, tokens and addresses removed.

## Part 3: short cross-network relay-only test (about 10 minutes)

This needs a second network, for example a laptop on a phone hotspot running the same
release MSI. It costs a little relay quota: keep it short.

1. On both machines, turn on Settings, Network, **Always use relay**.
2. One creates a room. Send the invite through a chat app, then the reply back. Compare the
   four words by voice. Admit.
3. Check that the members list shows **Relay** for the link.
4. Load the same video on both. Play, seek to the middle, pause, and send a chat message with
   a timestamp. Everything follows within about a second.
5. Leave the room on both machines and turn **Always use relay** off again.
6. Write down the minutes used and the Metered usage shown in your dashboard.

The longer checklist is `docs/checklists/cross-network.md`. Its signaling setup is replaced
by invites since Milestone 9.

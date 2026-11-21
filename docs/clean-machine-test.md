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
- **The sandbox has a system `vulkan-1.dll`, even with vGPU disabled.** The first automated
  run logged `vulkan=System`. So the bundled loader is proven by forcing it
  (`SYNCWATCH_FORCE_BUNDLED_VULKAN=1`, which the automated test does), not by a machine
  without Vulkan. A PC with no Vulkan at all is still unproven. With vGPU disabled there is no
  GPU driver, so video renders in software (WARP, through mpv's D3D11 path): slow, but it must
  work.

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
   6. Uninstalls **without** `REMOVEUSERDATA`: the program goes; the data folder and the
      Credential Manager entries stay.
   7. Reinstalls 0.0.2 and starts it with the bundled Vulkan loader forced. The kept identity is
      used again.
   8. Uninstalls **with** `REMOVEUSERDATA=1`. Checks that the program, the data folder and the
      Credential Manager entries are gone, and that the cleanup ran as the signed-in user (it
      writes `SyncWatch-cleanup.log` in the user's temp folder).

   "Installed" is read from the Windows Installer API, not from one registry key. A per-user
   MSI's Add/Remove Programs entry is under **HKLM** (`...\Uninstall\{ProductCode}`). Its
   product data is under the user's SID in `Installer\UserData` and published in HKCU's
   `Installer\Products`. The test writes all of these to `evidence.txt` before and after each
   step, and keeps a verbose `msiexec` log of every install, upgrade and uninstall.
3. Notepad opens `result.txt`. The last line says `RESULT: ALL PASSED` or how many checks
   failed. The same file, the MSI logs and the app logs are on your PC in
   `dist\sandbox-results\auto-<time>\`.
4. Close the sandbox.

The real GitHub path (a published release updating to the next one) is step (e) of the
release guide, after the first two real releases.

### Results (2026-11-20)

**First run: 6 failures, all in the test script.** Evidence in
`dist\sandbox-results\auto-20261120-150103`, plus probes run in that same sandbox.
- **Failures 1 and 3** (empty version; "exactly one entry"): the script looked for the entry
  only under HKCU `...\Uninstall`. The per-user MSI's entry is under **HKLM**
  `...\Uninstall\{ProductCode}`. The product itself is per-user (Installer UserData under the
  user's SID, published in HKCU `Installer\Products`). The Windows Installer API showed
  exactly one product, 0.0.2: the major upgrade had replaced 0.0.1.
- **Failure 2** (370 s): the same lookup never saw 0.0.2, so the test waited until its
  deadline. The app had in fact relaunched: the app log shows a new start 126 s after it quit
  for the update.
- **Failures 4–6:** with no entry found, the script ran `msiexec /x` with an empty product
  code. That removed nothing, still returned 0, and wrote no log. Run by hand in the same
  sandbox with `REMOVEUSERDATA=1`, the uninstall removed the program, the data folder and the
  credential. The `CleanupUserData` action ran deferred and impersonated (type 1106), before
  `RemoveFiles`.

**Fixes:**
- **Test:**
  - "installed" now comes from the Windows Installer API, with the registry dumped as
    evidence;
  - every `msiexec` call keeps a verbose log, and there is no uninstall without a product code;
  - both uninstall cases are tested, the relaunch is timed, and the bundled Vulkan loader is
    forced.
- **Second run, 1 failure, also in the test:** the per-user probe read the install context
  through `ProductsEx`, which does not enumerate in Windows PowerShell 5.1, so it always got 0.
  It now uses `ProductInfo AssignmentType` (0 = per-user) and also checks that the product is
  registered under this user's SID only. That is no weaker: a per-machine install fails both.
- **App:**
  - the startup log line showed the crate version instead of the app version;
  - the cleanup now writes `%TEMP%\SyncWatch-cleanup.log` (user, results, errors), because the
    MSI ignores the helper's exit code;
  - test builds can log the update's `msiexec` (`SYNCWATCH_TEST_MSI_LOG`).

**Third run: ALL PASSED** (`auto-20261120-154153`). Results:
- **Relaunch after the update:** confirmed. The new process is started by `msiexec.exe` as the
  signed-in user, and the upgrade log shows `LaunchApplication`.
- **Data cleanup:** confirmed. The cleanup ran as the signed-in user: it removed the folder and
  both credential entries, with no errors.
- **Without `REMOVEUSERDATA`:** the data and the credential stay, and a reinstall reuses the
  same identity.

**Why an update takes about 2 minutes here.** Every install of the unsigned 337 MB MSI spends
about 123 s in Windows Installer's "software restriction policy" check before the package even
opens. That includes the fresh install, the reinstall and the in-app upgrade. The upgrade
itself then takes about 1 s. The time on a normal PC is not measured. Code signing or a
smaller MSI (the WebView2 bootstrapper instead of the offline installer, ADR 0015) are the
ways to shorten it. What the user sees on screen during that wait has not been checked.
**Measured by the maintainer (2026-11-20):**
- the release MSI took about 5 minutes to install in the Sandbox;
- the same MSI took a few seconds on the real PC.

The long wait is a Sandbox effect, and the installer stays unchanged.

### Hands-on results (2026-11-20) and what followed

**Passed:**
- install of the 0.1.0 MSI (no UAC prompt);
- first launch;
- Metered setup;
- the bundled Vulkan loader (`vulkan=Bundled`; the default run logs `vulkan=System`);
- non-Latin subtitles.

**Failed:** invites between the Sandbox and the real PC, in both directions, with and without
Always use relay.
- The host showed "Connecting", then "Error connecting".
- The member's dialog went back to the paste step.
- Every room creation logged "using the last good relay servers (Metered unreachable)".

Reproduced and fixed with two release-mode instances on one PC (ADR 0014 and ADR 0004
revisions):
- the member's attempt ended at its first ICE timeout, before the host could answer;
- early auth messages were dropped;
- the dialog reset itself;
- the Metered fetch gave up after 8 s.

The automated test now has a **room step (4b)**:
- two copies of the installed test build, a password room, invite, reply, Admit;
- then play, pause, seek, speed, tracks and external files;
- no relay servers inside the Sandbox, which has no relay setup.

The relay-only version of the same test runs on the developer's PC with the real Metered
setup:

```
pwsh -NoProfile -File scripts/dev/release-pair.ps1 -Build -TestBuild -Setup -RelayOnly
pwsh -NoProfile -File scripts/dev/two-windows.ps1 -Scenario sync -ReleasePair $env:TEMP\sw-release-pair -Password test
pwsh -NoProfile -File scripts/dev/release-pair.ps1 -Stop
```

Automated Sandbox run with these fixes (2026-11-21, `dist\sandbox-results\auto-20261121-114752`):
**ALL PASSED**. This includes the room step, with 35/35 scenario checks between two installed
copies, and the relaunch after the update (127 s, mostly the Sandbox's package check).

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
| 4 | Video without a GPU, bundled Vulkan | Close SyncWatch. In the sandbox's PowerShell: `$env:SYNCWATCH_FORCE_BUNDLED_VULKAN='1'; & "$env:LOCALAPPDATA\Programs\SyncWatch\syncwatch.exe"`. Open `test.mkv` | Plays (slowly, software rendering). The latest log in `%LOCALAPPDATA%\SyncWatch\logs` has `native libraries loaded vulkan=Bundled` |
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

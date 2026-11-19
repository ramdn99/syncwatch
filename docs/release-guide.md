# Release guide

You run all of these in your own PowerShell 7 terminal, in `D:\dev\syncwatch`. Nothing here
needs a GitHub token: the GitHub CLI is already logged in (`gh auth status` shows it). Never
paste your key password into a chat, a file or a commit.

Background:
- `scripts/release.ps1` (what it checks, in order);
- ADR 0015 (installer) and ADR 0016 (updates);
- `docs/checklists/release-checklist.md`.

## (a) Make the updater key (once) and back it up

1. Create a folder outside the repository:

```
New-Item -ItemType Directory -Force D:\keys
```

2. Generate the key pair. It asks for a password twice; choose a long one:

```
pnpm --dir apps/desktop tauri signer generate -w D:\keys\syncwatch-updater.key
```

This writes `D:\keys\syncwatch-updater.key` (private) and
`D:\keys\syncwatch-updater.key.pub` (public).

3. Put the public key into the app and commit it. Only the public key goes into the
   repository.

```
Copy-Item D:\keys\syncwatch-updater.key.pub apps\desktop\src-tauri\updater-pubkey.txt
git add apps/desktop/src-tauri/updater-pubkey.txt
git commit -m "Updater public key"
git push
```

4. Back up the private key file **and** its password:
   - in your password manager: a new entry with the password and
     `syncwatch-updater.key` as an attachment;
   - and on an offline copy, for example a USB stick kept somewhere safe.

   If you lose either one, installed copies can never be updated again: users would have to
   download a new installer by hand. `*.key` is git-ignored as a safety net. Still, never
   place the key inside the repository folder.

## (b) Dry run

Does everything except tagging, pushing and uploading. It asks for the key password once:

```
pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.0 -UpdaterKey D:\keys\syncwatch-updater.key -DryRun
```

It ends with "Would upload:" and the file list. The files are in `dist\release\0.1.0\`.

## (c) The first real release, 0.1.0

1. In `CHANGELOG.md`, replace `unreleased` in the `## 0.1.0 - unreleased` line with today's
   date (for example `2026-11-20`), then commit and push:

```
git add CHANGELOG.md
git commit -m "Changelog: 0.1.0"
git push
```

2. Release. It asks for the key password once:

```
pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.0 -UpdaterKey D:\keys\syncwatch-updater.key
```

It runs the checks, the full CI and the release build (about 25 minutes), then tags `v0.1.0`,
pushes the tag, creates the release, uploads five files, checks them and prints the release
URL.

## (d) The next release

Bug fixes go up in the last number (0.1.1), features in the middle one (0.2.0).

```
pwsh -NoProfile -File scripts/bump-version.ps1 -Version 0.1.1
```

Add a section at the top of `CHANGELOG.md` (under the intro):

```
## 0.1.1 - 2026-12-01

- What changed, in plain words.
```

Then:

```
git add -A
git commit -m "Release 0.1.1"
git push
pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.1 -UpdaterKey D:\keys\syncwatch-updater.key -DryRun
pwsh -NoProfile -File scripts/release.ps1 -Version 0.1.1 -UpdaterKey D:\keys\syncwatch-updater.key
```

## (e) Check the Releases page and test an update

1. Open https://github.com/ramdn99/syncwatch/releases. It shows "SyncWatch 0.1.0" marked
   Latest, with the MSI, its `.sig`, `latest.json`, `latest.json.sig` and `SHA256SUMS`.
2. From the terminal:

```
gh release view v0.1.0 --repo ramdn99/syncwatch
```

3. Check what the app will fetch. This prints the version the update check sees:

```
(Invoke-RestMethod https://github.com/ramdn99/syncwatch/releases/latest/download/latest.json).version
```

4. Test an update from 0.1.0 to 0.1.1 (Windows Sandbox keeps your own PC clean; see
   `docs/clean-machine-test.md`):
   1. Install 0.1.0 from the Releases page.
   2. Publish 0.1.1 as in (d).
   3. Start SyncWatch 0.1.0. A card appears on the home screen: "Version 0.1.1 is
      available".
   4. Click **Update now**, then **Restart to update**.
   5. SyncWatch closes, the installer runs (a progress bar, no questions) and SyncWatch
      starts again.
   6. Settings, About shows version 0.1.1.

## (f) Fix or delete a bad release safely

- **A broken release that is already published:**
  - Hide it from updates at once. Turning it into a draft makes "latest" point back to the
    previous release:

```
gh release edit v0.1.1 --repo ramdn99/syncwatch --draft
```

  - Then publish a fixed **0.1.2** as in (d). Never reuse a version number: installed copies
    only accept a strictly newer version, and the MSI refuses downgrades. Anyone who already
    installed 0.1.1 moves to 0.1.2 through the normal update.
- **Delete a release and its tag completely** (only if nobody should keep it):

```
gh release delete v0.1.1 --repo ramdn99/syncwatch --yes --cleanup-tag
git tag -d v0.1.1
```

- **The tag was pushed but creating the release failed:** fix the cause (the script says
  what failed). Then either create the release by hand from the files that already exist:

```
gh release create v0.1.1 dist\release\0.1.1\SyncWatch-0.1.1-x64.msi dist\release\0.1.1\SyncWatch-0.1.1-x64.msi.sig dist\release\0.1.1\latest.json dist\release\0.1.1\latest.json.sig dist\release\0.1.1\SHA256SUMS --repo ramdn99/syncwatch --title "SyncWatch 0.1.1" --notes-file dist\release\0.1.1\release-notes.md --verify-tag
```

  or remove the tag everywhere and run the release script again:

```
git push origin :refs/tags/v0.1.1
git tag -d v0.1.1
```

- **The key leaked:**
  - make a new key pair as in (a);
  - publish one release that contains the NEW public key but is signed with the OLD key;
  - from then on sign with the new key, and delete the old key.

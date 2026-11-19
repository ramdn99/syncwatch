# Release checklist

Before every release (the release script checks the items marked with "script").

## Once, before the first release

- [ ] Updater key made and backed up (key file and password in a password manager and an
      offline copy): `docs/release-guide.md` (a).
- [ ] `apps/desktop/src-tauri/updater-pubkey.txt` holds your public key, committed and
      pushed (script).
- [ ] The repository is public, and the full-history scan is clean (script; report in
      `docs/public-repo-check.md`).
- [ ] The license obligations for the bundled libmpv/FFmpeg build double-checked by you
      (`docs/legal-notes.md`; not legal advice).

## Every release

- [ ] All work committed and pushed; `main` matches `origin/main` (script).
- [ ] Version bumped everywhere (`scripts/bump-version.ps1`; script checks).
- [ ] `CHANGELOG.md` has a `## X.Y.Z - date` section in plain words (script).
- [ ] The clean-machine test passed for this build, or at least its quick part
      (`docs/clean-machine-test.md`).
- [ ] Dry run passes:
      `pwsh -NoProfile -File scripts/release.ps1 -Version X.Y.Z -UpdaterKey D:\keys\syncwatch-updater.key -DryRun`.
- [ ] Real run: `pwsh -NoProfile -File scripts/release.ps1 -Version X.Y.Z -UpdaterKey D:\keys\syncwatch-updater.key`.
- [ ] The script printed the release URL and "latest.json is reachable, parses and matches the
      MSI".
- [ ] On the Releases page: the release is marked Latest and the notes read well.
- [ ] One real update from the previous version, for example in Windows Sandbox
      (`docs/release-guide.md` (e)).

## If something goes wrong

`docs/release-guide.md` (f): turn the release into a draft at once, then publish a fixed,
higher version. Never reuse a version number.

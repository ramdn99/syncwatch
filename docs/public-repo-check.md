# Public repository check

Date: 2026-11-19, before Milestone 9 work. Repository: ramdn99/syncwatch (public), one branch
(`main`), no tags, 37 commits.

## Secrets

| Check | Result |
|---|---|
| gitleaks over the whole history (`secret-scan.ps1 -Mode History`) | No leaks (37 commits) |
| Value scan for the real values from `.env.local` and `.env.ice.local` (4 variants, values never shown) | Not found in any commit |
| `.env*`, updater key files, `.claude/` (including `settings.json`), `graphify-out/`, `*.dpapi`, `*.pem`, `*.key` ever committed | Never |
| Media files ever committed | None (only generated PNG icons) |
| Remote refs | Only `main`; nothing else is published |

Nothing needs to be rotated or rewritten.

## Personal details visible in history

These are not secrets, but they are public now:
- **Windows user name `ramdn`** in machine paths:
  - `CLAUDE.md`, `scripts/check-dev-env.ps1` and `scripts/secret-scan.ps1` (tool locations
    under `C:\Users\ramdn\AppData\Local\...`);
  - about 180 occurrences across older commits.
  It matches the GitHub account name.
- **The commit author name and email** (the git `user.name` and `user.email` of every
  commit).
  - To hide the email in future commits, set `git config user.email` to your GitHub "noreply"
    address. Past commits keep the old address unless history is rewritten.
  - Rewriting history is your decision; nothing was rewritten.
- **Test media names** (`D:\media\test.mkv`, `old.mkv`, `test-mismatch.mkv`) and the drive
  layout (`D:\dev`, `D:\Rust`, `D:\libs\mpv`) in docs and scripts. No content, only names.
- **Fake test values** that look like credentials:
  - `user:pass@relay.example`;
  - `pw@myapp.metered.live`;
  - `user@x.example.invalid`.
  None of these is a real account.

## Repository files added in this check

- `README.md`: what it is, installing from Releases, the SmartScreen note, and building.
- `THIRD-PARTY-NOTICES.md`.
- `docs/legal-notes.md`: GPL and codec notes (not legal advice).
- `LICENSE` (GPL-3.0) was already present.

Publishing the complete source is how the project intends to meet the GPL's source
availability for the bundled GPL libmpv build. This is not legal advice: double-check the
obligations, for example whether to also point to, or offer, the exact corresponding source of
the libraries inside the libmpv build.

# Updater test fixtures

Made with a throwaway key (`tauri signer generate`, password `test`). The private key was
never stored in the repository; only `test.key.pub` is here.

- `SyncWatch-0.2.0-x64.msi`: 8 KiB of random bytes standing in for an MSI (not an installer).
- `SyncWatch-0.2.0-x64.msi.sig`: its signature (`tauri signer sign`).
- `latest.json` and `latest.json.sig`: a signed manifest offering 0.2.0 with that file's
  SHA-256 and signature.

Used by the unit tests in `src/updater.rs`. The local update server for the end-to-end test is
`scripts/dev/update-lab.ps1`, which makes its own fresh test key each run.

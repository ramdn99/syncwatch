# ADR 0002: Loading libmpv and the Vulkan loader by absolute path

- Status: Accepted (Milestone 1)
- Spec: section 1 "Vulkan", section 3 "libmpv by absolute path", pitfall 9

## Context

`libmpv-2.dll` (shinchiro MinGW build) statically imports `vulkan-1.dll`, a GPU-driver DLL that is
missing on clean machines. Without it, loading libmpv fails before mpv can fall back to D3D11.

## Decision

1. `syncwatch.exe` links `mpv.lib` with `/DELAYLOAD:libmpv-2.dll` (+ `delayimp.lib`). The process
   starts without libmpv, so it can pick the Vulkan loader first or show an error dialog.
2. At startup (`native/loader.rs`), before any mpv call:
   - `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)`: drop CWD and PATH from the
     process DLL search.
   - Load the system `vulkan-1.dll` with `LOAD_LIBRARY_SEARCH_SYSTEM32` only. If that fails, load
     `<exe>\vulkan\vulkan-1.dll` by absolute path with
     `LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32`.
   - Load `<exe>\libmpv-2.dll` by absolute path with the same flags. The Windows loader resolves
     libmpv's `vulkan-1.dll` import from the module already in memory. The later delay-load
     `LoadLibrary("libmpv-2.dll")` also returns the already-loaded module.
   - If either load fails, show a plain `MessageBoxW` error and exit with code 1.
3. Bundled loader: official LunarG Vulkan Runtime Components 1.4.363.0 (x64 `vulkan-1.dll`,
   Authenticode-signed by LunarG, Inc.). The URL and SHA-256 for both the zip and the DLL are pinned in
   `third_party/pins.json`; the license (`VulkanRT-License.txt`, Apache-2.0 + MIT) ships in `licenses\`.
4. `SYNCWATCH_FORCE_BUNDLED_VULKAN=1` skips the system copy. The development machine has a GPU driver
   (System32 `vulkan-1.dll` exists), so this is the only local way to exercise the bundled path.
5. `scripts/verify-imports.ps1` allows only system DLLs. `vulkan-1.dll` is the one exception, and
   only for `libmpv-2.dll`. The VC++ runtime is forbidden (static CRT); a negative self-test exists.

## Consequences

- The real proof (no GPU driver, mpv falls back to D3D11) is the Milestone 9 clean-VM gate.
- A newer libmpv build must be pinned deliberately (`pins.json`, `docs/libmpv-build.md`).

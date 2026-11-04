# libmpv build used by SyncWatch

| Item | Value |
|---|---|
| Source | shinchiro/mpv-winbuild-cmake GitHub release `20261101` |
| Archive | `mpv-dev-x86_64-20261101-git-48fca90e8c.7z` |
| Archive SHA-256 | `30D3F9FA121CE24638610A9AE52F8957743F9DEA416DD37DC4FF7E43589EF843` |
| mpv version | `v0.41.0-1374-g48fca90e8` (PE timestamp 2026-11-01) |
| `libmpv-2.dll` SHA-256 | `F40293DC5F4012F3F7BCC79DE2B9BFB66309B87DBD33A47EE49EBFF716EB8154` |
| Toolchain | MinGW-w64 (imports only Windows system DLLs, `api-ms-win-crt-*`, and `vulkan-1.dll`) |
| License | GPL (the build includes GPL components). SyncWatch is therefore GPL-3.0. |

## Note on the build date

The project brief named the `20261018` build (`git-01e2533f31`). On 2026-11-04 the DLL in
`D:\libs\mpv` was compared with both official archives:

- `20261018`: different DLL (`82BAE3B6…`, mpv `v0.41.0-1205-g01e2533f3`).
- `20261101`: byte-identical DLL, `libmpv.dll.a` and headers.

The installed files are the official **20261101** build. That is what is pinned. The client API headers
are identical between the two builds.

## Where the files live (developer machine)

`LIBMPV_DIR=D:\libs\mpv` is read-only for all scripts and builds:

```
D:\libs\mpv\libmpv-2.dll
D:\libs\mpv\libmpv.dll.a
D:\libs\mpv\include\mpv\*.h
D:\libs\mpv\mpv.def   (hand-generated, 206 exports)
D:\libs\mpv\mpv.lib   (hand-generated import library for MSVC)
D:\libs\mpv\mpv.exp
```

The dev build copies `libmpv-2.dll` next to `target\<profile>\syncwatch.exe`. It never writes into
`D:\libs\mpv`.

## Regenerating mpv.def and mpv.lib (only after deliberately switching builds)

The archive ships no MSVC import library. Open the VS developer shell first (one command per line):

```
& "D:\VS\BuildTools\Common7\Tools\Launch-VsDevShell.ps1" -Arch amd64 -HostArch amd64
cd D:\libs\mpv
dumpbin /exports libmpv-2.dll > exports.txt
```

Build `mpv.def` from the export names (`LIBRARY libmpv-2.dll`, `EXPORTS`, one name per line). Then:

```
lib /def:mpv.def /machine:x64 /out:mpv.lib
```

After switching builds, update `third_party/pins.json` (`libmpv` section) and this file. Then run:

```
pwsh -NoProfile -File scripts/fetch-third-party.ps1
pwsh -NoProfile -File scripts/verify-imports.ps1
```

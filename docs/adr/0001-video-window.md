# ADR 0001: Video surface = borderless owner window under the transparent main window

- Status: Accepted (Milestone 1, re-verified 2026-11-04)
- Spec: section 1 "Video embedding", section 7 "mpv window always matches the CENTER region"

## Context

The UI is a WebView2 page (Tauri 2). mpv must draw video inside the center column, with HTML overlay
controls on top. Verified in the previous build and not re-opened here:

- A `wid` CHILD window placed under a transparent WebView2 area is NOT visible (the transparent region
  shows the window background, not the sibling child).
- libmpv's render API is not used (decision in spec section 1).

So mpv renders (via `wid`) into its own borderless top-level window placed directly UNDER the
transparent main window. The center column of the page is transparent (`.video-hole`); every other
region paints an opaque surface.

## The ownership problem (spec conflict, resolved with the user)

The spec asked for an mpv window "owned by the main window and placed directly under it". That is
impossible in Win32: an owned window is ALWAYS above its owner in the z-order. Approved alternative:

**Inverted ownership: the video window OWNS the main window** (`SetWindowLongPtrW(main,
GWLP_HWNDPARENT, video)`).

- The OS itself then keeps the main window above the video window.
- The main window keeps its taskbar button through `WS_EX_APPWINDOW`. tao (Tauri's windowing layer,
  0.37.2) sets WS_EX_APPWINDOW whenever the window is on the taskbar, and re-applies it on style changes.
- The video window is `WS_POPUP | WS_CLIPCHILDREN`, `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`. It has no
  taskbar or Alt-Tab entry, never takes activation (`MA_NOACTIVATE`), and is never topmost.
- **Fallback** (the documented second option, always active): on every `WM_WINDOWPOSCHANGED`,
  `WM_SIZE` and `WM_DPICHANGED` of the main window (comctl32 `SetWindowSubclass`), the video window is
  re-placed with `SetWindowPos(video, hWndInsertAfter = main, ...)`, i.e. directly under main.
- Destroying the main window (WM_NCDESTROY) destroys the video window and releases the subclass.

## Rules

1. Placement is a pure function `decide(main_visible, main_minimized, wanted, center, client_origin)`
   in `apps/desktop/src-tauri/src/native/video_surface.rs` (unit-tested).
2. The video window is NEVER shown while the main window is hidden or minimized. This is what keeps
   "showing mpv's popup must not restore a minimized main window" true. The previous build had that bug.
3. The page reports the CENTER rect in physical client pixels (`toPhysical` rounds edges, not sizes,
   so adjacent columns never gap or overlap), via ResizeObserver, window resize, `devicePixelRatio`
   changes (DPI / monitor moves) and every layout state change. Rust adds the client origin.
4. All Win32 calls run on the UI thread (`run_on_main_thread`). No lock is held across Win32 calls
   that can send messages (avoids cross-thread deadlocks).
5. The main window starts hidden (`visible: false`). It is shown after the first paint (two
   `requestAnimationFrame`s, then the `app_ready` command), so there is no white flash. A 4 s safety
   net shows it anyway.

## Verification

- `video_surface::imp::tests::popup_does_not_restore_minimized_main_window` (real Win32 windows,
  off-screen) checks:
  - inverted owner (`GW_OWNER`);
  - video directly under main (`GW_HWNDNEXT`);
  - alignment within 1 px after show and after a move;
  - video hidden while main is minimized;
  - a show request while minimized leaves main minimized and the video hidden;
  - restore shows the video;
  - destroying main destroys the video window.
- `scripts/dev/alignment-check.ps1` drives real mouse/keyboard input (resizers, collapse, move,
  resize, fullscreen) against a running dev build and compares the mpv window with the rect the UI
  reports (1 px tolerance, no overlap with side columns). It runs only when the developer is away from
  the machine.

## Consequences / open items

- Multi-monitor with mixed DPI depends on the page re-reporting after `devicePixelRatio` changes. There
  can be one stale frame between the native move and the new rect; the alignment check measures after
  things settle.
- Exclusive fullscreen is not used (tao borderless fullscreen), so the owner/owned pair stays valid.

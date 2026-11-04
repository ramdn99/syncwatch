//! The mpv video surface: a borderless top-level window that sits DIRECTLY UNDER the
//! transparent main (WebView2) window and always matches the CENTER column (ADR 0001).
//!
//! Ownership is inverted: the video window OWNS the main window. Win32 always keeps an owned
//! window above its owner, so the OS itself keeps the main window above the video. We still
//! re-assert "directly under" on every WM_WINDOWPOSCHANGED of the main window (the documented
//! fallback), never use topmost, and never show the video window while the main window is
//! minimized or hidden. That last rule prevents the old "popup restores a minimized main
//! window" bug; see the regression test below.
//!
//! Threading: all Win32 calls must run on the UI thread (callers use `run_on_main_thread`).
//! No lock is held while calling Win32 functions that can send messages.

use serde::{Deserialize, Serialize};

/// A rectangle in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
    #[cfg(test)]
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }
}

/// What the surface should do after a state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Hide,
    /// Place at this SCREEN rect, directly under the main window, and make visible.
    Place(Rect),
}

/// Pure decision function (unit-tested).
///
/// * `center` is the center column in CLIENT coordinates of the main window.
/// * `client_origin` is the main window's client (0,0) in screen coordinates.
pub fn decide(
    main_visible: bool,
    main_minimized: bool,
    wanted: bool,
    center: Option<Rect>,
    client_origin: (i32, i32),
) -> Action {
    if !main_visible || main_minimized || !wanted {
        return Action::Hide;
    }
    match center {
        Some(c) if !c.is_empty() => Action::Place(c.offset(client_origin.0, client_origin.1)),
        _ => Action::Hide,
    }
}

#[cfg(windows)]
pub use imp::VideoSurface;

#[cfg(windows)]
mod imp {
    use super::{Action, Rect, decide};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BLACK_BRUSH, ClientToScreen, GetStockObject, HBRUSH, InvalidateRect,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_HWNDPARENT,
        GetWindowRect, IsIconic, IsWindow, IsWindowVisible, MA_NOACTIVATE, RegisterClassExW,
        SW_HIDE, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_SHOWWINDOW, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, WM_DPICHANGED, WM_MOUSEACTIVATE, WM_NCDESTROY, WM_SIZE,
        WM_WINDOWPOSCHANGED, WNDCLASSEXW, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_POPUP,
    };
    use windows::core::{PCWSTR, w};

    const CLASS_NAME: PCWSTR = w!("SyncWatchVideoSurface");
    const SUBCLASS_ID: usize = 0x5357_5653; // "SWVS"

    #[derive(Default)]
    struct Inner {
        center: Option<Rect>,
        wanted: bool,
        last_screen: Option<Rect>,
        columns: Vec<Rect>,
        fullscreen: bool,
        seq: u64,
        /// Last inputs to `decide` (dev probe diagnostics).
        last_inputs: (bool, bool, bool),
    }

    /// Owns the video HWND and keeps it aligned under the main window.
    pub struct VideoSurface {
        main: isize,
        video: isize,
        inner: Mutex<Inner>,
        syncing: AtomicBool,
        probe: Option<PathBuf>,
    }

    fn hwnd(v: isize) -> HWND {
        HWND(v as *mut core::ffi::c_void)
    }

    unsafe extern "system" fn video_wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        // SAFETY: forwarding to the default procedure with the original arguments.
        unsafe { DefWindowProcW(h, msg, wp, lp) }
    }

    fn register_class() -> bool {
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        *REGISTERED.get_or_init(|| {
            // SAFETY: standard class registration with static strings and a stock brush.
            unsafe {
                let Ok(module) = GetModuleHandleW(None) else {
                    return false;
                };
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(video_wndproc),
                    hInstance: module.into(),
                    hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                RegisterClassExW(&wc) != 0
            }
        })
    }

    unsafe extern "system" fn main_subclass_proc(
        h: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
        _id: usize,
        refdata: usize,
    ) -> LRESULT {
        // SAFETY: refdata is the Arc<VideoSurface> pointer leaked in `attach`; it stays valid
        // until WM_NCDESTROY below reclaims it.
        let surface = unsafe { &*(refdata as *const VideoSurface) };
        // SAFETY: forwarding the original message first, so state (iconic, visible) is current.
        let r = unsafe { DefSubclassProc(h, msg, wp, lp) };
        match msg {
            WM_WINDOWPOSCHANGED | WM_SIZE | WM_DPICHANGED => surface.sync(),
            WM_NCDESTROY => {
                // SAFETY: removing our own subclass and reclaiming the leaked Arc exactly once.
                unsafe {
                    let _ = RemoveWindowSubclass(h, Some(main_subclass_proc), SUBCLASS_ID);
                    let _ = DestroyWindow(hwnd(surface.video));
                    drop(Arc::from_raw(refdata as *const VideoSurface));
                }
            }
            _ => {}
        }
        r
    }

    impl VideoSurface {
        /// Creates the video window and attaches it under `main` (inverted ownership).
        /// `probe` (dev only) receives a JSON layout snapshot after every placement.
        pub fn attach(main: HWND, probe: Option<PathBuf>) -> Result<Arc<Self>, String> {
            if !register_class() {
                return Err("could not register the video window class".into());
            }
            let mut rc = RECT::default();
            // SAFETY: valid main HWND; output struct on the stack.
            unsafe {
                let _ = GetWindowRect(main, &mut rc);
            }
            // SAFETY: creating a hidden, non-activating, borderless popup with our class.
            let video = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    CLASS_NAME,
                    w!("SyncWatch video"),
                    WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                    rc.left,
                    rc.top,
                    (rc.right - rc.left).max(1),
                    (rc.bottom - rc.top).max(1),
                    None,
                    None,
                    GetModuleHandleW(None).ok().map(Into::into),
                    None,
                )
            }
            .map_err(|e| format!("could not create the video window: {e}"))?;

            let surface = Arc::new(Self {
                main: main.0 as isize,
                video: video.0 as isize,
                inner: Mutex::new(Inner::default()),
                syncing: AtomicBool::new(false),
                probe,
            });
            // SAFETY: GWLP_HWNDPARENT on a top-level window sets its OWNER. The Arc pointer
            // handed to the subclass is reclaimed in WM_NCDESTROY.
            unsafe {
                SetWindowLongPtrW(main, GWLP_HWNDPARENT, video.0 as isize);
                let raw = Arc::into_raw(Arc::clone(&surface)) as usize;
                if !SetWindowSubclass(main, Some(main_subclass_proc), SUBCLASS_ID, raw).as_bool() {
                    drop(Arc::from_raw(raw as *const VideoSurface));
                    return Err("could not subclass the main window".into());
                }
            }
            surface.sync();
            Ok(surface)
        }

        /// The HWND mpv renders into (`wid`).
        pub fn video_hwnd(&self) -> isize {
            self.video
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
            self.inner.lock().unwrap_or_else(|p| p.into_inner())
        }

        /// Sets the center column (client px) and the side columns (for the dev probe).
        pub fn set_layout(&self, center: Rect, columns: Vec<Rect>, fullscreen: bool) {
            {
                let mut g = self.lock();
                g.center = Some(center);
                g.columns = columns;
                g.fullscreen = fullscreen;
            }
            self.sync();
        }

        /// Whether the app wants video visible at all (true once the UI has painted).
        /// Requesting visibility never shows anything while the main window is minimized.
        pub fn set_wanted(&self, wanted: bool) {
            self.lock().wanted = wanted;
            self.sync();
        }

        /// Re-evaluates and applies placement. Safe to call any time on the UI thread.
        pub fn sync(&self) {
            if self.syncing.swap(true, Ordering::SeqCst) {
                return;
            }
            let main = hwnd(self.main);
            let video = hwnd(self.video);
            // SAFETY: read-only queries on our own windows.
            let (visible, minimized, origin) = unsafe {
                let mut p = POINT { x: 0, y: 0 };
                let _ = ClientToScreen(main, &mut p);
                (
                    IsWindowVisible(main).as_bool(),
                    IsIconic(main).as_bool(),
                    (p.x, p.y),
                )
            };
            let (wanted, center) = {
                let mut g = self.lock();
                g.last_inputs = (visible, minimized, g.wanted);
                (g.wanted, g.center)
            };
            match decide(visible, minimized, wanted, center, origin) {
                Action::Hide => {
                    // SAFETY: hiding our own window; never touches the main window.
                    unsafe {
                        if IsWindowVisible(video).as_bool() {
                            let _ = ShowWindow(video, SW_HIDE);
                        }
                    }
                    self.lock().last_screen = None;
                }
                Action::Place(r) => {
                    // SAFETY: inserting the video window directly after (= under) main.
                    // SWP_NOACTIVATE: never steal focus. Shown only when main is restored.
                    unsafe {
                        let _ = SetWindowPos(
                            video,
                            Some(main),
                            r.x,
                            r.y,
                            r.w,
                            r.h,
                            SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                        );
                        let _ = InvalidateRect(Some(video), None, true);
                    }
                    self.lock().last_screen = Some(r);
                }
            }
            self.syncing.store(false, Ordering::SeqCst);
            self.write_probe(origin);
        }

        /// Actual screen rect of the video window, if visible.
        pub fn actual_rect(&self) -> Option<Rect> {
            let video = hwnd(self.video);
            // SAFETY: read-only queries on our own window.
            unsafe {
                if !IsWindow(Some(video)).as_bool() || !IsWindowVisible(video).as_bool() {
                    return None;
                }
                let mut rc = RECT::default();
                GetWindowRect(video, &mut rc).ok()?;
                Some(Rect {
                    x: rc.left,
                    y: rc.top,
                    w: rc.right - rc.left,
                    h: rc.bottom - rc.top,
                })
            }
        }

        fn write_probe(&self, origin: (i32, i32)) {
            let Some(path) = &self.probe else { return };
            let actual = self.actual_rect();
            let snapshot = {
                let mut g = self.lock();
                g.seq += 1;
                serde_json::json!({
                    "seq": g.seq,
                    "mainHwnd": self.main,
                    "videoHwnd": self.video,
                    "clientOrigin": [origin.0, origin.1],
                    "expected": g.last_screen,
                    "actual": actual,
                    "columns": g.columns.iter().map(|c| c.offset(origin.0, origin.1)).collect::<Vec<_>>(),
                    "fullscreen": g.fullscreen,
                    "inputs": { "mainVisible": g.last_inputs.0, "mainMinimized": g.last_inputs.1, "wanted": g.last_inputs.2, "center": g.center },
                })
            };
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, snapshot.to_string()).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use windows::Win32::UI::WindowsAndMessaging::{
            GW_HWNDNEXT, GW_OWNER, GetWindow, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_NOSIZE,
            SWP_NOZORDER, WS_EX_APPWINDOW, WS_OVERLAPPEDWINDOW,
        };

        unsafe extern "system" fn test_main_proc(
            h: HWND,
            msg: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            // SAFETY: default handling for the test window.
            unsafe { DefWindowProcW(h, msg, wp, lp) }
        }

        /// A stand-in for the Tauri main window, placed far off-screen.
        fn create_test_main() -> HWND {
            static CLASS: OnceLock<()> = OnceLock::new();
            let class = w!("SyncWatchTestMain");
            // SAFETY: test-only window class and window.
            unsafe {
                let module = GetModuleHandleW(None).unwrap();
                CLASS.get_or_init(|| {
                    let wc = WNDCLASSEXW {
                        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                        lpfnWndProc: Some(test_main_proc),
                        hInstance: module.into(),
                        lpszClassName: class,
                        ..Default::default()
                    };
                    RegisterClassExW(&wc);
                });
                CreateWindowExW(
                    WS_EX_APPWINDOW,
                    class,
                    w!("SyncWatch test main"),
                    WS_OVERLAPPEDWINDOW,
                    -30000,
                    -30000,
                    800,
                    600,
                    None,
                    None,
                    Some(module.into()),
                    None,
                )
                .unwrap()
            }
        }

        fn client_origin(main: HWND) -> (i32, i32) {
            let mut p = POINT { x: 0, y: 0 };
            // SAFETY: read-only query.
            unsafe {
                let _ = ClientToScreen(main, &mut p);
            }
            (p.x, p.y)
        }

        /// Regression: requesting the video "popup" while the main window is minimized must NOT
        /// restore the main window and must not show the video window. After restore, the video
        /// is visible, directly under main, owned-inverted, and aligned within 1 px.
        #[test]
        fn popup_does_not_restore_minimized_main_window() {
            let main = create_test_main();
            let s = VideoSurface::attach(main, None).unwrap();
            let video = hwnd(s.video_hwnd());
            let center = Rect {
                x: 200,
                y: 10,
                w: 400,
                h: 300,
            };
            s.set_layout(center, vec![], false);
            s.set_wanted(true);

            // SAFETY: test-only window manipulation on windows created by this thread.
            unsafe {
                // Hidden main => no video.
                assert!(!IsWindowVisible(video).as_bool());

                let _ = ShowWindow(main, SW_SHOWNOACTIVATE);
                assert!(IsWindowVisible(video).as_bool(), "video shown with main");
                assert_eq!(
                    GetWindow(main, GW_OWNER).unwrap(),
                    video,
                    "inverted ownership"
                );
                assert_eq!(
                    GetWindow(main, GW_HWNDNEXT).unwrap(),
                    video,
                    "directly under main"
                );
                let o = client_origin(main);
                let a = s.actual_rect().unwrap();
                let e = center.offset(o.0, o.1);
                assert!((a.x - e.x).abs() <= 1 && (a.y - e.y).abs() <= 1);
                assert!((a.w - e.w).abs() <= 1 && (a.h - e.h).abs() <= 1);

                // Minimize main => video hidden.
                let _ = ShowWindow(main, SW_SHOWMINNOACTIVE);
                assert!(IsIconic(main).as_bool());
                assert!(
                    !IsWindowVisible(video).as_bool(),
                    "video hidden while minimized"
                );

                // The "popup" request (e.g. a file finished loading) while minimized.
                s.set_wanted(true);
                s.set_layout(center, vec![], false);
                s.sync();
                assert!(IsIconic(main).as_bool(), "main must stay minimized");
                assert!(!IsWindowVisible(video).as_bool(), "video must stay hidden");

                // Restore => visible and aligned again.
                let _ = ShowWindow(main, SW_SHOWNOACTIVATE);
                assert!(!IsIconic(main).as_bool());
                assert!(IsWindowVisible(video).as_bool());

                // Move main => video follows within 1 px.
                let _ = SetWindowPos(
                    main,
                    None,
                    -29000,
                    -29500,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let o = client_origin(main);
                let a = s.actual_rect().unwrap();
                assert!((a.x - (o.0 + center.x)).abs() <= 1);
                assert!((a.y - (o.1 + center.y)).abs() <= 1);
                assert_eq!(GetWindow(main, GW_HWNDNEXT).unwrap(), video);

                // Destroying main tears the video window down too.
                let _ = DestroyWindow(main);
                assert!(!IsWindow(Some(video)).as_bool());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Rect = Rect {
        x: 300,
        y: 0,
        w: 800,
        h: 600,
    };

    #[test]
    fn hidden_or_minimized_main_hides_video() {
        assert_eq!(decide(false, false, true, Some(C), (0, 0)), Action::Hide);
        assert_eq!(decide(true, true, true, Some(C), (0, 0)), Action::Hide);
        assert_eq!(decide(true, false, false, Some(C), (0, 0)), Action::Hide);
    }

    #[test]
    fn empty_or_unknown_center_hides_video() {
        assert_eq!(decide(true, false, true, None, (0, 0)), Action::Hide);
        let empty = Rect { w: 0, ..C };
        assert_eq!(decide(true, false, true, Some(empty), (0, 0)), Action::Hide);
    }

    #[test]
    fn places_center_in_screen_coordinates() {
        assert_eq!(
            decide(true, false, true, Some(C), (100, 50)),
            Action::Place(Rect {
                x: 400,
                y: 50,
                w: 800,
                h: 600
            })
        );
    }

    #[test]
    fn rect_intersection() {
        let left = Rect {
            x: 0,
            y: 0,
            w: 300,
            h: 600,
        };
        assert!(!left.intersects(&C));
        assert!(left.intersects(&Rect { x: 299, ..C }));
    }
}

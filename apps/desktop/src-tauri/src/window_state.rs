//! Main window size and position (Milestone 8 persistence), in config/window.json. Restored
//! before the window is first shown, only if it still fits a connected monitor (a monitor may
//! have been unplugged). Nothing from a room is stored here (ADR 0006).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub maximized: bool,
}

/// A monitor's work area in physical pixels: (x, y, w, h).
pub type Area = (i32, i32, u32, u32);

pub const MIN_W: u32 = 960;
pub const MIN_H: u32 = 600;

/// The state to restore, if it is usable: at least the minimum size, and its title bar (the top
/// 40 px band, 100 px wide) visible on some monitor so the user can grab it.
pub fn usable(s: WindowState, monitors: &[Area]) -> Option<WindowState> {
    if s.w < MIN_W || s.h < MIN_H || s.w > 16_384 || s.h > 16_384 {
        return None;
    }
    let grab = monitors.iter().find(|&&(mx, my, mw, mh)| {
        let (mr, mb) = (mx + mw as i32, my + mh as i32);
        let (l, r) = (s.x.max(mx), (s.x + s.w as i32).min(mr));
        let (t, b) = (s.y.max(my), (s.y + 40).min(mb));
        r - l >= 100 && b - t >= 20
    })?;
    // Never larger than the monitor the title bar is on (it may have a smaller resolution or
    // a higher scale than when the size was saved); keep it on that monitor.
    let &(mx, my, mw, mh) = grab;
    let (w, h) = (s.w.min(mw.max(MIN_W)), s.h.min(mh.max(MIN_H)));
    let x = s.x.clamp(mx - (w as i32 - 100), mx + mw as i32 - 100);
    let y = s.y.clamp(my, (my + mh as i32 - 40).max(my));
    Some(WindowState { x, y, w, h, ..s })
}

pub fn path(config: &Path) -> PathBuf {
    config.join("window.json")
}

pub fn load(config: &Path) -> Option<WindowState> {
    serde_json::from_slice(&std::fs::read(path(config)).ok()?).ok()
}

pub fn save(config: &Path, s: &WindowState) {
    if let Ok(b) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(path(config), b);
    }
}

/// Restores the saved state onto the (still hidden) main window.
pub fn restore(window: &tauri::WebviewWindow, config: &Path) {
    let Some(saved) = load(config) else { return };
    let monitors: Vec<Area> = window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let (p, z) = (m.position(), m.size());
            (p.x, p.y, z.width, z.height)
        })
        .collect();
    let Some(s) = usable(saved, &monitors) else {
        tracing::info!("saved window position is off-screen: using the default");
        return;
    };
    let _ = window.set_size(tauri::PhysicalSize::new(s.w, s.h));
    let _ = window.set_position(tauri::PhysicalPosition::new(s.x, s.y));
    if s.maximized {
        let _ = window.maximize();
    }
}

/// Reads the current state (the restored size when maximized is kept from before).
pub fn capture(window: &tauri::Window, previous: Option<WindowState>) -> Option<WindowState> {
    let maximized = window.is_maximized().unwrap_or(false);
    if window.is_minimized().unwrap_or(false) {
        return previous;
    }
    if maximized {
        return previous.map(|p| WindowState {
            maximized: true,
            ..p
        });
    }
    let p = window.outer_position().ok()?;
    let z = window.outer_size().ok()?;
    Some(WindowState {
        x: p.x,
        y: p.y,
        w: z.width,
        h: z.height,
        maximized: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Area = (0, 0, 1920, 1040);
    const RIGHT: Area = (1920, 0, 2560, 1400);
    fn st(x: i32, y: i32, w: u32, h: u32) -> WindowState {
        WindowState {
            x,
            y,
            w,
            h,
            maximized: false,
        }
    }

    #[test]
    fn restores_only_what_the_user_can_reach() {
        assert!(usable(st(100, 100, 1440, 860), &[MAIN]).is_some());
        // On the second monitor while it is connected; not once it is unplugged.
        assert!(usable(st(2100, 50, 1440, 860), &[MAIN, RIGHT]).is_some());
        assert!(usable(st(2100, 50, 1440, 860), &[MAIN]).is_none());
        // Title bar above the top of every screen: unreachable.
        assert!(usable(st(100, -500, 1440, 860), &[MAIN]).is_none());
        // Mostly off the left edge but the title bar still grabbable.
        assert!(usable(st(-1300, 100, 1440, 860), &[MAIN]).is_some());
        // Below the minimum size or absurd sizes are ignored.
        assert!(usable(st(0, 0, 500, 400), &[MAIN]).is_none());
        assert!(usable(st(0, 0, 50_000, 900), &[MAIN]).is_none());
        // No monitors known: nothing restored.
        assert!(usable(st(100, 100, 1440, 860), &[]).is_none());
        // Saved larger than the monitor (another resolution or scale): shrunk to fit it.
        let fit = usable(st(0, 0, 2028, 1362), &[MAIN]).unwrap();
        assert_eq!((fit.w, fit.h), (1920, 1040));
        // A title bar above the screen top is moved down onto it.
        let y = usable(st(100, -10, 1440, 860), &[MAIN]).map(|s| s.y);
        assert!(y.is_none() || y == Some(0));
    }

    #[test]
    fn saved_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("sw-winstate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let s = WindowState {
            x: -20,
            y: 30,
            w: 1500,
            h: 900,
            maximized: true,
        };
        save(&dir, &s);
        assert_eq!(load(&dir), Some(s));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

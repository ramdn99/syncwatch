//! Dark native title bar. DWMWA_USE_IMMERSIVE_DARK_MODE is attribute 20 on Windows 10 20H1+
//! and the undocumented 19 on 1809-1909; Windows 11 additionally accepts a caption color.

use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::core::BOOL;

const DWMWA_USE_IMMERSIVE_DARK_MODE: i32 = 20;
const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1: i32 = 19;
const DWMWA_CAPTION_COLOR: i32 = 35;
const DWMWA_TEXT_COLOR: i32 = 36;

/// `#RRGGBB` -> COLORREF (0x00BBGGRR).
pub const fn colorref(rgb: u32) -> COLORREF {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    COLORREF(r | (g << 8) | (b << 16))
}

/// Returns true if dark mode was applied by either attribute.
pub fn apply(hwnd: HWND) -> bool {
    let on = BOOL(1);
    let set = |attr: i32, ptr: *const core::ffi::c_void, size: u32| {
        // SAFETY: valid window handle and a pointer to a value of the declared size.
        unsafe { DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attr), ptr, size) }.is_ok()
    };
    let size = std::mem::size_of::<BOOL>() as u32;
    let ok = set(DWMWA_USE_IMMERSIVE_DARK_MODE, (&raw const on).cast(), size)
        || set(
            DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1,
            (&raw const on).cast(),
            size,
        );
    // Windows 11 only; silently ignored elsewhere.
    let caption = colorref(0x0B0B0D);
    let text = colorref(0xF2F2F4);
    let cs = std::mem::size_of::<COLORREF>() as u32;
    let _ = set(DWMWA_CAPTION_COLOR, (&raw const caption).cast(), cs);
    let _ = set(DWMWA_TEXT_COLOR, (&raw const text).cast(), cs);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorref_is_bgr() {
        assert_eq!(colorref(0x0B0B0D).0, 0x000D0B0B);
        assert_eq!(colorref(0xE8A33D).0, 0x003DA3E8);
    }
}

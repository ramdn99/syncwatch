//! Windows-native pieces: DLL loading, the mpv video surface, title-bar theming.

pub mod loader;
#[cfg(windows)]
pub mod title_bar;
pub mod video_surface;

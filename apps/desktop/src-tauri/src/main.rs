// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod commands;
mod connection;
mod logging;
mod media;
mod native;
mod netlab;
mod paths;
mod room;
mod roommedia;
#[cfg(test)]
mod secrets_for_tests;
mod torrent;

use paths::AppPaths;

fn main() {
    let paths = AppPaths::resolve();
    paths.ensure();
    // Debug builds with a dev identity: a separate WebView2 user-data folder per instance.
    // Instances of one app otherwise share ONE WebView2 browser process, so killing one dev
    // window (the host-crash test) would take the others' web views down with it.
    if cfg!(debug_assertions)
        && std::env::var("SYNCWATCH_DEV_IDENTITY").is_ok_and(|v| !v.is_empty())
        && std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none()
    {
        // SAFETY: single-threaded at this point (before logging, Tauri or any other thread).
        unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", paths.root.join("webview2")) };
    }
    logging::init(&paths.logs);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        portable = paths.portable,
        "SyncWatch starting"
    );

    #[cfg(windows)]
    {
        let force_bundled = std::env::var("SYNCWATCH_FORCE_BUNDLED_VULKAN").as_deref() == Ok("1");
        match native::loader::load(&paths.exe_dir, force_bundled) {
            Ok(libs) => {
                tracing::info!(vulkan = ?libs.vulkan, libmpv = %libs.libmpv.display(), "native libraries loaded");
                commands::run(paths, libs);
            }
            Err(e) => {
                tracing::error!(error = %e, "native library load failed");
                native::loader::error_dialog(&e.to_string());
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("SyncWatch supports Windows only.");
    }
}

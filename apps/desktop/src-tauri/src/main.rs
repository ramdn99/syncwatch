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
mod prefs;
mod room;
mod roommedia;
#[cfg(test)]
mod secrets_for_tests;
mod torrent;
mod window_state;

use paths::AppPaths;

fn main() {
    let paths = AppPaths::resolve();
    paths.ensure();
    // WebView2's own profile (local storage, caches) lives in the app's data folder: in portable
    // mode <exe>\data\webview2, never next to the exe or in another AppData folder. For dev
    // identities this also gives each instance its own WebView2 browser process (killing one dev
    // window, the host-crash test, must not take the others' web views down).
    // An empty value counts as unset (WebView2 then falls back to a folder next to the exe).
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_none_or(|v| v.is_empty()) {
        // SAFETY: single-threaded at this point (before logging, Tauri or any other thread).
        unsafe { std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", paths.root.join("webview2")) };
    }
    logging::init(&paths.logs);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        portable = paths.portable,
        "SyncWatch starting"
    );
    // No full paths in logs (spec section 11): only whether the profile is in the data folder.
    tracing::info!(
        in_data_folder = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")
            .is_some_and(|v| std::path::Path::new(&v) == paths.root.join("webview2")),
        "web view data folder"
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

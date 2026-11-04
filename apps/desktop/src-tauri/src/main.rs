// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod connection;
mod logging;
mod media;
mod native;
mod paths;
#[cfg(test)]
mod secrets_for_tests;

use paths::AppPaths;

fn main() {
    let paths = AppPaths::resolve();
    paths.ensure();
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

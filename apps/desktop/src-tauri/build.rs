//! Build script: Tauri codegen, libmpv link setup (delay-load), and placing the runtime files
//! next to the executable (`target/<profile>`), in the same layout as the installed app:
//! `libmpv-2.dll`, `vulkan\vulkan-1.dll`, `fonts\NotoSans-*.ttf`. Never writes into LIBMPV_DIR.
//!
//! The copies are idempotent and ordered: one fixed list, processed in order, each written to a
//! temporary file in the destination folder and then renamed over the target, with retries
//! (a virus scanner or a running dev app can hold a file for a moment). A file that is already
//! identical is left alone. Errors are reported with the reason, never swallowed.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// (source relative to the repo root or LIBMPV_DIR, destination relative to target/<profile>).
const THIRD_PARTY: &[(&str, &str)] = &[
    (
        "third_party/cache/vulkan/vulkan-1.dll",
        "vulkan/vulkan-1.dll",
    ),
    (
        "third_party/cache/fonts/NotoSans-Regular.ttf",
        "fonts/NotoSans-Regular.ttf",
    ),
    (
        "third_party/cache/fonts/NotoSans-Bold.ttf",
        "fonts/NotoSans-Bold.ttf",
    ),
    (
        "third_party/cache/fonts/NotoSans-Italic.ttf",
        "fonts/NotoSans-Italic.ttf",
    ),
    (
        "third_party/cache/fonts/NotoSans-BoldItalic.ttf",
        "fonts/NotoSans-BoldItalic.ttf",
    ),
];

fn main() {
    println!("cargo:rerun-if-env-changed=LIBMPV_DIR");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // libmpv-2.dll is loaded explicitly by absolute path at startup (ADR 0002); the import
        // is delay-loaded so the process can start, show an error dialog, or pre-load the
        // Vulkan loader first.
        println!("cargo:rustc-link-arg=/DELAYLOAD:libmpv-2.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }

    if let Some(profile_dir) = profile_dir() {
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
        let repo = manifest.join("../../..");
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        if let Ok(libmpv) = std::env::var("LIBMPV_DIR") {
            files.push((
                Path::new(&libmpv).join("libmpv-2.dll"),
                profile_dir.join("libmpv-2.dll"),
            ));
        } else {
            println!("cargo:warning=LIBMPV_DIR is not set; libmpv-2.dll is not placed");
        }
        for (src, dst) in THIRD_PARTY {
            files.push((repo.join(src), profile_dir.join(dst)));
        }
        for (src, dst) in &files {
            println!("cargo:rerun-if-changed={}", src.display());
            if let Err(e) = place(src, dst) {
                println!("cargo:warning={e}");
            }
        }
    }

    tauri_build::build();
}

/// `target/<profile>` derived from OUT_DIR (`target/<profile>/build/<pkg>/out`).
fn profile_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}

/// Places `src` at `dst` unless an identical file is already there.
fn place(src: &Path, dst: &Path) -> Result<(), String> {
    let src_bytes = std::fs::read(src).map_err(|e| {
        format!(
            "{} not found ({e}); run scripts/fetch-third-party.ps1",
            src.display()
        )
    })?;
    if std::fs::read(dst).is_ok_and(|d| d == src_bytes) {
        return Ok(());
    }
    let parent = dst
        .parent()
        .ok_or_else(|| format!("no parent folder for {}", dst.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut last = String::new();
    for attempt in 0..8u32 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(250 * u64::from(attempt)));
        }
        match std::fs::write(&tmp, &src_bytes).and_then(|()| std::fs::rename(&tmp, dst)) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(format!(
        "could not place {} at {} after 8 tries: {last}",
        src.display(),
        dst.display()
    ))
}

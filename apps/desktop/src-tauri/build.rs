//! Build script: Tauri codegen, libmpv link setup (delay-load), and copying the runtime DLLs
//! next to the dev executable. Never writes into LIBMPV_DIR.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=LIBMPV_DIR");
    println!("cargo:rerun-if-changed=../../../third_party/cache/vulkan/vulkan-1.dll");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // libmpv-2.dll is loaded explicitly by absolute path at startup (ADR 0002); the import
        // is delay-loaded so the process can start, show an error dialog, or pre-load the
        // Vulkan loader first.
        println!("cargo:rustc-link-arg=/DELAYLOAD:libmpv-2.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }

    if let Some(profile_dir) = profile_dir() {
        if let Ok(libmpv) = std::env::var("LIBMPV_DIR") {
            copy_if_changed(
                &Path::new(&libmpv).join("libmpv-2.dll"),
                &profile_dir.join("libmpv-2.dll"),
            );
        }
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
        let vulkan = manifest.join("../../../third_party/cache/vulkan/vulkan-1.dll");
        if vulkan.exists() {
            copy_if_changed(&vulkan, &profile_dir.join("vulkan").join("vulkan-1.dll"));
        } else {
            println!(
                "cargo:warning=bundled Vulkan loader not found; run scripts/fetch-third-party.ps1"
            );
        }
    }

    tauri_build::build();
}

/// `target/<profile>` derived from OUT_DIR (`target/<profile>/build/<pkg>/out`).
fn profile_dir() -> Option<PathBuf> {
    let out = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(Path::to_path_buf)
}

fn copy_if_changed(src: &Path, dst: &Path) {
    let Ok(src_meta) = std::fs::metadata(src) else {
        println!("cargo:warning=missing {}", src.display());
        return;
    };
    if let Ok(dst_meta) = std::fs::metadata(dst)
        && dst_meta.len() == src_meta.len()
        && dst_meta.modified().ok() >= src_meta.modified().ok()
    {
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(src, dst) {
        println!(
            "cargo:warning=could not copy {} -> {}: {e}",
            src.display(),
            dst.display()
        );
    }
}

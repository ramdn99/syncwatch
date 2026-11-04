//! Lets the linker find the hand-generated `mpv.lib` and delay-loads `libmpv-2.dll` for this
//! crate's own test binaries, so tests that never touch mpv run without the DLL.

fn main() {
    println!("cargo:rerun-if-env-changed=LIBMPV_DIR");
    if std::env::var("CARGO_FEATURE_LIBMPV").is_err() {
        return;
    }
    if let Ok(dir) = std::env::var("LIBMPV_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    } else {
        println!(
            "cargo:warning=LIBMPV_DIR is not set; linking mpv.lib will fail (expected D:\\libs\\mpv)"
        );
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:libmpv-2.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
}

//! Loads the Vulkan loader and libmpv by ABSOLUTE path with safe DLL search flags (ADR 0002).
//!
//! Order matters: `libmpv-2.dll` imports `vulkan-1.dll` statically. We load a system
//! `vulkan-1.dll` (System32 only) if present, else the bundled `<exe>\vulkan\vulkan-1.dll`,
//! BEFORE libmpv; the Windows loader then satisfies libmpv's import with the module already in
//! memory. The executable delay-loads libmpv, so nothing touches it before this runs.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VulkanSource {
    System,
    Bundled(PathBuf),
}

#[derive(Debug, Clone)]
pub struct NativeLibs {
    pub vulkan: VulkanSource,
    pub libmpv: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(
        "SyncWatch could not load the Vulkan loader (vulkan-1.dll).\n\nNeither a system copy nor the bundled copy in\n{0}\ncould be loaded. Please reinstall SyncWatch."
    )]
    Vulkan(String),
    #[error(
        "SyncWatch could not load its video engine (libmpv-2.dll) from\n{0}\n\nPlease reinstall SyncWatch."
    )]
    Libmpv(String),
}

/// Paths the loader will try, in order (pure, unit-tested).
pub fn plan(exe_dir: &Path, force_bundled: bool) -> (bool, PathBuf, PathBuf) {
    (
        !force_bundled,
        exe_dir.join("vulkan").join("vulkan-1.dll"),
        exe_dir.join("libmpv-2.dll"),
    )
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_FLAGS, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW, SetDefaultDllDirectories,
    };
    use windows::core::{HSTRING, w};

    fn load_abs(path: &Path, flags: LOAD_LIBRARY_FLAGS) -> bool {
        if !path.is_absolute() {
            return false;
        }
        let h = HSTRING::from(path.as_os_str());
        // SAFETY: plain Win32 call with a valid, NUL-terminated wide string.
        unsafe { LoadLibraryExW(&h, None, flags) }.is_ok()
    }

    pub fn load(exe_dir: &Path, force_bundled: bool) -> Result<NativeLibs, LoadError> {
        // Remove the current directory and PATH from the default DLL search for this process.
        // SAFETY: process-wide setting, called once at startup before other threads load DLLs.
        unsafe {
            let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
        }
        let (try_system, bundled_vk, libmpv) = plan(exe_dir, force_bundled);

        // SAFETY: loading by name restricted to System32.
        let system_ok = try_system
            && unsafe { LoadLibraryExW(w!("vulkan-1.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .is_ok();
        let vulkan = if system_ok {
            VulkanSource::System
        } else if load_abs(
            &bundled_vk,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        ) {
            VulkanSource::Bundled(bundled_vk)
        } else {
            return Err(LoadError::Vulkan(bundled_vk.display().to_string()));
        };

        if !load_abs(
            &libmpv,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        ) {
            return Err(LoadError::Libmpv(libmpv.display().to_string()));
        }
        Ok(NativeLibs { vulkan, libmpv })
    }

    /// Plain error dialog (works before any UI exists).
    pub fn error_dialog(message: &str) {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let text = HSTRING::from(message);
        // SAFETY: modal message box with valid strings and no owner.
        unsafe {
            MessageBoxW(None, &text, w!("SyncWatch"), MB_OK | MB_ICONERROR);
        }
    }
}

#[cfg(windows)]
pub use imp::{error_dialog, load};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_uses_absolute_paths_next_to_exe() {
        let (sys, vk, mpv) = plan(Path::new(r"C:\Apps\SyncWatch"), false);
        assert!(sys);
        assert_eq!(vk, PathBuf::from(r"C:\Apps\SyncWatch\vulkan\vulkan-1.dll"));
        assert_eq!(mpv, PathBuf::from(r"C:\Apps\SyncWatch\libmpv-2.dll"));
        let (sys, _, _) = plan(Path::new(r"C:\Apps\SyncWatch"), true);
        assert!(!sys);
    }

    #[test]
    fn errors_are_plain_and_name_the_file() {
        let e = LoadError::Libmpv(r"C:\x\libmpv-2.dll".into()).to_string();
        assert!(e.contains("libmpv-2.dll") && e.contains("reinstall"));
    }
}

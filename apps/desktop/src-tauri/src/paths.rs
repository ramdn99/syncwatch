//! Per-user data locations. Never writes to the install directory, except in portable mode
//! (`portable.flag` next to the exe), where everything lives in `<exe>\data`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub exe_dir: PathBuf,
    pub root: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
    pub cache: PathBuf,
    pub portable: bool,
}

pub const APP_FOLDER: &str = "SyncWatch";

impl AppPaths {
    pub fn resolve() -> Self {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        let portable = exe_dir.join("portable.flag").is_file();
        let local = directories::BaseDirs::new().map(|b| b.data_local_dir().to_path_buf());
        Self::from_parts(exe_dir, portable, local)
    }

    /// Pure constructor, used by tests.
    pub fn from_parts(exe_dir: PathBuf, portable: bool, local_app_data: Option<PathBuf>) -> Self {
        let root = if portable {
            exe_dir.join("data")
        } else {
            local_app_data
                .unwrap_or_else(std::env::temp_dir)
                .join(APP_FOLDER)
        };
        Self {
            config: root.join("config"),
            logs: root.join("logs"),
            cache: root.join("cache"),
            root,
            exe_dir,
            portable,
        }
    }

    pub fn ensure(&self) {
        for d in [&self.config, &self.logs, &self.cache] {
            let _ = std::fs::create_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_mode_uses_local_app_data() {
        let p = AppPaths::from_parts(
            PathBuf::from(r"C:\Programs\SyncWatch"),
            false,
            Some(PathBuf::from(r"C:\Users\u\AppData\Local")),
        );
        assert_eq!(p.root, PathBuf::from(r"C:\Users\u\AppData\Local\SyncWatch"));
        assert!(!p.logs.starts_with(&p.exe_dir));
    }

    #[test]
    fn portable_mode_stays_next_to_exe() {
        let p = AppPaths::from_parts(PathBuf::from(r"E:\SyncWatch"), true, None);
        assert_eq!(p.root, PathBuf::from(r"E:\SyncWatch\data"));
        assert!(p.cache.starts_with(r"E:\SyncWatch\data"));
    }
}

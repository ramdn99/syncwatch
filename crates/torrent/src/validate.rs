//! Torrent file names and paths are untrusted (spec section 10/11). A torrent is refused when
//! ANY of its paths is unsafe: librqbit creates files under the torrent folder from these
//! components, and pieces on file boundaries write into neighbouring files.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("empty path")]
    Empty,
    #[error("path too deep")]
    TooDeep,
    #[error("path too long")]
    TooLong,
    #[error("unsafe path component")]
    BadComponent,
    #[error("reserved Windows name")]
    Reserved,
}

pub const MAX_DEPTH: usize = 16;
pub const MAX_COMPONENT_BYTES: usize = 200;
pub const MAX_PATH_CHARS: usize = 240;

/// Video containers a room can play from a torrent.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "webm", "avi", "mov", "ts", "m2ts", "mts", "ogv", "wmv", "flv", "mpg",
    "mpeg",
];

fn is_reserved(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end()
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.chars().count() == 4
        && stem
            .chars()
            .skip(3)
            .all(|c| c.is_ascii_digit() || matches!(c, '¹' | '²' | '³')))
}

/// Checks one path component.
pub fn check_component(c: &str) -> Result<(), PathError> {
    if c.is_empty() || c == "." || c == ".." {
        return Err(PathError::BadComponent);
    }
    if c.len() > MAX_COMPONENT_BYTES {
        return Err(PathError::TooLong);
    }
    if c.chars().any(|ch| {
        ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Err(PathError::BadComponent);
    }
    // Windows strips trailing dots and spaces, so "a." and "a" would collide.
    if c.ends_with('.') || c.ends_with(' ') || c.starts_with(' ') {
        return Err(PathError::BadComponent);
    }
    if is_reserved(c) {
        return Err(PathError::Reserved);
    }
    Ok(())
}

/// Checks a relative path given as components (torrent `path` list, plus the torrent name for
/// multi-file torrents).
pub fn check_path<S: AsRef<str>>(components: &[S]) -> Result<(), PathError> {
    if components.is_empty() {
        return Err(PathError::Empty);
    }
    if components.len() > MAX_DEPTH {
        return Err(PathError::TooDeep);
    }
    let mut chars = 0;
    for c in components {
        check_component(c.as_ref())?;
        chars += c.as_ref().chars().count() + 1;
    }
    if chars > MAX_PATH_CHARS {
        return Err(PathError::TooLong);
    }
    Ok(())
}

pub fn is_video(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTENSIONS.iter().any(|v| ext.eq_ignore_ascii_case(v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        assert!(check_path(&["Sintel", "sintel-2048-surround.mp4"]).is_ok());
        assert!(check_path(&["Season 1", "Ep 01 - Ünïcode ☃.mkv"]).is_ok());
    }

    #[test]
    fn rejects_traversal_absolute_and_odd_names() {
        for bad in [
            vec![".."],
            vec!["a", "..", "b.mkv"],
            vec!["."],
            vec![""],
            vec!["C:", "x.mkv"],
            vec!["a/b.mkv"],
            vec!["a\\b.mkv"],
            vec!["trailing."],
            vec!["trailing "],
            vec![" leading"],
            vec!["bell\u{7}.mkv"],
            vec!["what?.mkv"],
        ] {
            assert!(check_path(&bad).is_err(), "{bad:?}");
        }
        assert_eq!(check_path::<&str>(&[]), Err(PathError::Empty));
    }

    #[test]
    fn rejects_reserved_windows_names() {
        for r in [
            "CON", "con.mkv", "Aux", "NUL.txt", "COM1", "lpt9.mp4", "COM¹",
        ] {
            assert_eq!(check_component(r), Err(PathError::Reserved), "{r}");
        }
        assert!(check_component("CONTENT.mkv").is_ok());
        assert!(check_component("COM10").is_ok());
    }

    #[test]
    fn rejects_overlong_and_deep() {
        let long = "a".repeat(MAX_COMPONENT_BYTES + 1);
        assert_eq!(check_path(&[long.as_str()]), Err(PathError::TooLong));
        let deep = vec!["d"; MAX_DEPTH + 1];
        assert_eq!(check_path(&deep), Err(PathError::TooDeep));
        let wide = vec!["x".repeat(100); 3];
        assert_eq!(check_path(&wide), Err(PathError::TooLong));
    }

    #[test]
    fn video_extensions() {
        assert!(is_video("Movie.MKV"));
        assert!(is_video("a.b.mp4"));
        assert!(!is_video("sample.nfo"));
        assert!(!is_video("noext"));
    }
}

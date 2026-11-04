//! Media descriptors. The room carries only DESCRIPTIONS of media; each member plays their own
//! copy (local file or their own torrent download). No paths are ever sent.

use crate::ProtocolError;
use crate::limits::*;
use serde::{Deserialize, Serialize};

/// Bytes hashed from the head and from the tail of a file for its identity.
pub const IDENTITY_SAMPLE_BYTES: u64 = 4 * 1024 * 1024;

/// Identity of a local media file: blake3 over (first N MB || last N MB || size), plus size and
/// duration. Cheap to compute on a multi-GB file; good enough to detect a different file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileIdentity {
    pub sample_blake3: [u8; 32],
    pub size: u64,
    /// Container duration in ms, when known.
    pub duration_ms: Option<u64>,
}

impl FileIdentity {
    /// Two identities match when hash and size match; durations (if both known) must agree
    /// within 1 s (demuxers can report slightly different durations).
    pub fn matches(&self, other: &FileIdentity) -> bool {
        if self.sample_blake3 != other.sample_blake3 || self.size != other.size {
            return false;
        }
        match (self.duration_ms, other.duration_ms) {
            (Some(a), Some(b)) => a.abs_diff(b) <= 1000,
            _ => true,
        }
    }
}

/// 20-byte BitTorrent v1 infohash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Infohash(pub [u8; 20]);

impl Infohash {
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalMedia {
    /// File name only (never a path), for the "Choose my file" panel.
    pub name: String,
    pub identity: FileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentMedia {
    pub infohash: Infohash,
    pub file_index: u32,
    /// Selected file's name (sanitized) and size.
    pub name: String,
    pub size: u64,
    /// The magnet link the host shared (members confirm before downloading).
    pub magnet: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaDescriptor {
    LocalFile(LocalMedia),
    Torrent(TorrentMedia),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    LocalFile,
    Torrent,
}

/// Maximum magnet length accepted.
pub const MAX_MAGNET_BYTES: usize = 8 * 1024;

impl MediaDescriptor {
    pub fn kind(&self) -> MediaKind {
        match self {
            MediaDescriptor::LocalFile(_) => MediaKind::LocalFile,
            MediaDescriptor::Torrent(_) => MediaKind::Torrent,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            MediaDescriptor::LocalFile(l) => &l.name,
            MediaDescriptor::Torrent(t) => &t.name,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            MediaDescriptor::LocalFile(l) => {
                check_file_name(&l.name)?;
            }
            MediaDescriptor::Torrent(t) => {
                check_file_name(&t.name)?;
                if t.magnet.len() > MAX_MAGNET_BYTES {
                    return Err(ProtocolError::Limit("magnet"));
                }
                let parsed =
                    crate::magnet::parse(&t.magnet).map_err(|_| ProtocolError::Limit("magnet"))?;
                if parsed.infohash != t.infohash {
                    return Err(ProtocolError::Limit("magnet infohash mismatch"));
                }
            }
        }
        Ok(())
    }
}

/// A shareable file name: non-empty, bounded, no path separators, no control characters, not
/// "." / "..".
pub fn check_file_name(name: &str) -> Result<(), ProtocolError> {
    if name.is_empty() || name.len() > MAX_FILE_NAME_BYTES {
        return Err(ProtocolError::Limit("file name length"));
    }
    if name == "."
        || name == ".."
        || name.contains(['/', '\\', ':'])
        || name.chars().any(char::is_control)
    {
        return Err(ProtocolError::Limit("file name characters"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(h: u8, size: u64, dur: Option<u64>) -> FileIdentity {
        FileIdentity {
            sample_blake3: [h; 32],
            size,
            duration_ms: dur,
        }
    }

    #[test]
    fn identity_matching() {
        assert!(id(1, 10, Some(5000)).matches(&id(1, 10, Some(5600))));
        assert!(!id(1, 10, Some(5000)).matches(&id(1, 10, Some(7000))));
        assert!(!id(1, 10, None).matches(&id(2, 10, None)));
        assert!(!id(1, 10, None).matches(&id(1, 11, None)));
        assert!(id(1, 10, None).matches(&id(1, 10, Some(1))));
    }

    #[test]
    fn file_names_cannot_be_paths() {
        assert!(check_file_name("Movie (2024).mkv").is_ok());
        for bad in ["", ".", "..", "a/b.mkv", r"C:\x.mkv", "a\u{0}b", "x:y"] {
            assert!(check_file_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_file_name(&"a".repeat(256)).is_err());
    }
}

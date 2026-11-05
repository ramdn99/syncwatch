//! Change-source support (spec section 6): the Rust magnet parser is authoritative, local
//! files are identified by blake3 over head+tail+size, and the resulting `ChangeMedia` request
//! is validated through the real protocol encoder. No torrent network activity exists yet.

use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use syncwatch_protocol::magnet::{self, MagnetError};
use syncwatch_protocol::*;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagnetInfo {
    ok: bool,
    infohash: Option<String>,
    display_name: Option<String>,
    trackers: usize,
    error: Option<&'static str>,
}

fn magnet_error_code(e: MagnetError) -> &'static str {
    match e {
        MagnetError::NotMagnet => "notMagnet",
        MagnetError::TooLong => "tooLong",
        MagnetError::NoInfohash => "noInfohash",
        MagnetError::BadInfohash => "badInfohash",
        MagnetError::ConflictingInfohash => "conflictingInfohash",
        MagnetError::V2Only => "v2Only",
        MagnetError::BadEncoding => "badEncoding",
    }
}

#[tauri::command]
pub fn magnet_inspect(text: String) -> MagnetInfo {
    match magnet::parse(&text) {
        Ok(m) => MagnetInfo {
            ok: true,
            infohash: Some(m.infohash.to_hex()),
            display_name: m.display_name,
            trackers: m.trackers.len(),
            error: None,
        },
        Err(e) => MagnetInfo {
            ok: false,
            infohash: None,
            display_name: None,
            trackers: 0,
            error: Some(magnet_error_code(e)),
        },
    }
}

/// blake3 over (first N bytes || last N bytes || size), see `protocol::media`.
pub fn identify_file(path: &Path) -> std::io::Result<FileIdentity> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let n = IDENTITY_SAMPLE_BYTES.min(size);
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    f.seek(SeekFrom::Start(size - n))?;
    f.read_exact(&mut buf)?;
    h.update(&buf);
    h.update(&size.to_le_bytes());
    Ok(FileIdentity {
        sample_blake3: *h.finalize().as_bytes(),
        size,
        duration_ms: None,
    })
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceInput {
    Local { path: String },
    Magnet { text: String },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedChange {
    kind: &'static str,
    title: String,
    size: Option<u64>,
    infohash: Option<String>,
    /// Encoded size of the ChangeMedia request frame (proves it passes protocol validation).
    frame_bytes: usize,
    /// The torrent engine arrives in Milestone 7; nothing is sent to any network before then.
    torrent_engine_available: bool,
}

pub fn file_name(path: &Path) -> String {
    let n = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Names must pass protocol::check_file_name (no separators or control characters).
    let cleaned: String = n
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .take(200)
        .collect();
    if cleaned.is_empty() {
        "video".into()
    } else {
        cleaned
    }
}

#[tauri::command]
pub async fn prepare_change_media(source: SourceInput) -> Result<PreparedChange, String> {
    let (descriptor, kind) = match source {
        SourceInput::Local { path } => {
            let p = std::path::PathBuf::from(path);
            let identity =
                tauri::async_runtime::spawn_blocking(move || identify_file(&p).map(|id| (id, p)))
                    .await
                    .map_err(|_| "The file could not be read.".to_owned())?
                    .map_err(|_| "The file could not be read.".to_owned())?;
            let (identity, p) = identity;
            (
                MediaDescriptor::LocalFile(LocalMedia {
                    name: file_name(&p),
                    identity,
                }),
                "local",
            )
        }
        SourceInput::Magnet { text } => {
            let m = magnet::parse(&text).map_err(|e| magnet_error_code(e).to_owned())?;
            let name = m
                .display_name
                .clone()
                .map(|d| file_name(Path::new(&d)))
                .unwrap_or_else(|| format!("torrent-{}", &m.infohash.to_hex()[..8]));
            (
                MediaDescriptor::Torrent(TorrentMedia {
                    infohash: m.infohash,
                    file_index: 0,
                    name,
                    size: 0,
                    magnet: text.trim().to_owned(),
                }),
                "magnet",
            )
        }
    };
    let frame = encode(&Message::Request(Request::ChangeMedia {
        request_id: 1,
        descriptor: descriptor.clone(),
    }))
    .map_err(|e| format!("This source cannot be shared: {e}"))?;
    let (size, infohash) = match &descriptor {
        MediaDescriptor::LocalFile(l) => (Some(l.identity.size), None),
        MediaDescriptor::Torrent(t) => (None, Some(t.infohash.to_hex())),
    };
    Ok(PreparedChange {
        kind,
        title: descriptor.display_name().to_owned(),
        size,
        infohash,
        frame_bytes: frame.len(),
        torrent_engine_available: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_small_and_large_files() {
        let dir = std::env::temp_dir().join(format!("sw-ident-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.bin");
        std::fs::write(&small, b"hello").unwrap();
        let a = identify_file(&small).unwrap();
        assert_eq!(a.size, 5);
        let big = dir.join("big.bin");
        let data: Vec<u8> = (0..(9 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&big, &data).unwrap();
        let b1 = identify_file(&big).unwrap();
        // Changing a byte in the middle (not sampled) keeps the identity; head or tail changes it.
        let mut mid = data.clone();
        // Head sample = [0, 4 MB), tail sample = [5 MB, 9 MB): 4.5 MB is not sampled.
        mid[4 * 1024 * 1024 + 512 * 1024] ^= 1;
        std::fs::write(&big, &mid).unwrap();
        assert_eq!(identify_file(&big).unwrap(), b1);
        let mut head = data.clone();
        head[10] ^= 1;
        std::fs::write(&big, &head).unwrap();
        assert_ne!(identify_file(&big).unwrap(), b1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn magnet_inspect_uses_the_rust_parser() {
        let ok = magnet_inspect(
            "magnet:?xt=urn:btih:c12fe1c06bba254a9dc9f519b335aa7c1367a88a&dn=x.mkv".into(),
        );
        assert!(ok.ok);
        assert_eq!(ok.display_name.as_deref(), Some("x.mkv"));
        let bad = magnet_inspect("magnet:?xt=urn:btmh:1220aa".into());
        assert_eq!(bad.error, Some("v2Only"));
    }

    #[test]
    fn file_names_are_sanitized_for_the_protocol() {
        assert_eq!(file_name(Path::new(r"C:\x\My Movie.mkv")), "My Movie.mkv");
        assert!(check_file_name(&file_name(Path::new("a\u{7}b.mkv"))).is_ok());
    }
}

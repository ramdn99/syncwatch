//! The room's media side (Milestone 6, ADR 0007): the real mpv player and the external files.
//!
//! * Player events become sync-core observations ([`player_obs`]), and the position is polled
//!   (every 100 ms) as `PlayerObs::Position`. sync-core decides everything else: scheduled
//!   starts, drift correction, the ready gate. Playback commands from the UI never touch the
//!   player directly in a room: they are room requests, and the player only executes
//!   [`PlayerCmd`]s, so there is no feedback loop from remote-caused player changes.
//! * External subtitle/audio files: the host's files are validated (extension, size ceiling),
//!   text subtitles are converted to UTF-8, hashed with blake3 and served from a per-room cache.
//!   Members write chunks in order into the same per-room cache. The cache is deleted when
//!   the room ends.

#![cfg(windows)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use syncwatch_player::{Player, PlayerEvent, Track, TrackKind as PTrackKind, TrackSelection};
use syncwatch_protocol::limits::CHUNK_BYTES;
use syncwatch_protocol::*;
use syncwatch_sync_core::{LocalTrack, PlayerCmd, PlayerObs};

pub const SUB_EXT: [&str; 6] = ["srt", "ass", "ssa", "vtt", "sub", "sup"];
pub const AUDIO_EXT: [&str; 12] = [
    "mka", "aac", "ac3", "eac3", "dts", "flac", "mp3", "opus", "ogg", "wav", "m4a", "thd",
];
const TEXT_SUB_EXT: [&str; 5] = ["srt", "ass", "ssa", "vtt", "sub"];
/// Above this size the host is warned before sending to relayed members (Metered quota).
pub const RELAYED_WARN_BYTES: u64 = 25 * 1024 * 1024;

fn kind_of(k: ExternalKind) -> PTrackKind {
    match k {
        ExternalKind::Audio => PTrackKind::Audio,
        ExternalKind::Subtitle => PTrackKind::Sub,
    }
}

/// Lower-case, case-insensitive key for matching mpv's `external-filename` to our files.
fn path_key(p: &str) -> String {
    p.replace('/', "\\").to_lowercase()
}

pub struct Upload {
    pub to: MemberId,
    pub id: ExternalId,
    pub offset: u64,
}

#[derive(Default)]
pub struct MediaCtx {
    pub room_dir: PathBuf,
    /// Host: our own (prepared) files by manifest id.
    host_files: HashMap<ExternalId, PathBuf>,
    /// Host: prepared files waiting for sync-core to assign their manifest id.
    pending_host: Vec<([u8; 32], PathBuf)>,
    /// Member: verified files in the per-room cache.
    member_files: HashMap<ExternalId, PathBuf>,
    writers: HashMap<ExternalId, (File, PathBuf)>,
    /// mpv file name -> manifest id, for the external tracks we added.
    added: HashMap<String, ExternalId>,
    pub uploads: Vec<Upload>,
}

impl MediaCtx {
    pub fn new(room_dir: PathBuf) -> Self {
        Self {
            room_dir,
            ..Self::default()
        }
    }

    /// Deletes the per-room cache (room end: nothing may survive into the next room).
    pub fn clear(&mut self) {
        self.writers.clear();
        self.uploads.clear();
        self.host_files.clear();
        self.member_files.clear();
        self.pending_host.clear();
        self.added.clear();
        if self.room_dir.as_os_str().is_empty() {
            return;
        }
        match std::fs::remove_dir_all(&self.room_dir) {
            Ok(()) => tracing::info!("per-room cache deleted"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(error = %e, "could not delete the per-room cache"),
        }
    }

    #[cfg(test)]
    pub fn has_cache_files(&self) -> bool {
        self.room_dir.is_dir()
            && std::fs::read_dir(&self.room_dir).is_ok_and(|mut d| d.next().is_some())
    }

    fn entry_path(&self, id: ExternalId, format: &str) -> PathBuf {
        let fmt: String = format
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect();
        self.room_dir.join(format!(
            "ext-{}.{}",
            id.0,
            if fmt.is_empty() { "bin" } else { &fmt }
        ))
    }

    /// We became the host (succession): serve the files we downloaded as a member.
    pub fn promote_member_files(&mut self) {
        for (id, path) in self.member_files.drain() {
            self.host_files.entry(id).or_insert(path);
        }
    }

    /// Host: remember a prepared file until its manifest id is known.
    pub fn host_prepared(&mut self, blake3: [u8; 32], path: PathBuf) {
        self.pending_host.push((blake3, path));
    }

    /// Host: match prepared files to the manifest entries sync-core created.
    pub fn sync_manifest(&mut self, manifest: &[ExternalEntry]) {
        let mut keep = Vec::new();
        for (hash, path) in self.pending_host.drain(..) {
            match manifest
                .iter()
                .find(|e| e.blake3 == hash && !self.host_files.contains_key(&e.external_id))
            {
                Some(e) => {
                    self.host_files.insert(e.external_id, path);
                }
                None => keep.push((hash, path)),
            }
        }
        self.pending_host = keep;
    }

    /// Member: a verified identical file is already in the per-room cache (e.g. after a
    /// reconnect or a media change that kept the subtitle): reuse it, no transfer.
    pub fn cached_identical(&mut self, e: &ExternalEntry) -> bool {
        let path = self.entry_path(e.external_id, &e.format);
        if self.member_files.contains_key(&e.external_id) {
            return true;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return false;
        };
        if bytes.len() as u64 == e.size && *blake3::hash(&bytes).as_bytes() == e.blake3 {
            self.member_files.insert(e.external_id, path);
            return true;
        }
        false
    }

    fn file_for(&self, id: ExternalId) -> Option<&PathBuf> {
        self.host_files
            .get(&id)
            .or_else(|| self.member_files.get(&id))
    }

    /// Writes an in-order, already validated chunk to the per-room cache.
    pub fn write_chunk(
        &mut self,
        id: ExternalId,
        format: &str,
        offset: u64,
        data: &[u8],
    ) -> std::io::Result<()> {
        if offset == 0 || !self.writers.contains_key(&id) {
            std::fs::create_dir_all(&self.room_dir)?;
            let path = self.entry_path(id, format);
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .open(&path)?;
            f.seek(SeekFrom::Start(offset))?;
            self.writers.insert(id, (f, path));
        }
        if let Some((f, _)) = self.writers.get_mut(&id) {
            f.seek(SeekFrom::Start(offset))?;
            f.write_all(data)?;
        }
        Ok(())
    }

    pub fn complete(&mut self, id: ExternalId) {
        if let Some((f, path)) = self.writers.remove(&id) {
            let _ = f.sync_all();
            self.member_files.insert(id, path);
        }
    }

    pub fn discard(&mut self, id: ExternalId) {
        if let Some((_, path)) = self.writers.remove(&id) {
            let _ = std::fs::remove_file(path);
        }
        if let Some(p) = self.member_files.remove(&id) {
            let _ = std::fs::remove_file(p);
        }
    }

    pub fn start_upload(&mut self, to: MemberId, id: ExternalId, offset: u64) {
        self.uploads.retain(|u| !(u.to == to && u.id == id));
        if self.host_files.contains_key(&id) {
            self.uploads.push(Upload { to, id, offset });
        } else {
            tracing::warn!(id = id.0, "upload requested for an unknown external file");
        }
    }

    pub fn cancel_upload(&mut self, to: MemberId, id: ExternalId) {
        self.uploads.retain(|u| !(u.to == to && u.id == id));
    }

    /// Reads the next chunk of an upload (None when finished or unreadable).
    pub fn next_chunk(&mut self, i: usize) -> Option<Message> {
        let u = self.uploads.get(i)?;
        let path = self.host_files.get(&u.id)?;
        let mut f = File::open(path).ok()?;
        f.seek(SeekFrom::Start(u.offset)).ok()?;
        let mut buf = vec![0u8; CHUNK_BYTES];
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        buf.truncate(n);
        let msg = Message::FileChunk {
            external_id: u.id,
            offset: u.offset,
            data: buf,
        };
        if let Some(u) = self.uploads.get_mut(i) {
            u.offset += n as u64;
        }
        Some(msg)
    }

    /// Converts player tracks for sync-core, tagging the externals we added with their id.
    pub fn local_tracks(&self, tracks: &[Track]) -> Vec<LocalTrack> {
        tracks
            .iter()
            .filter_map(|t| {
                let kind = match t.kind {
                    PTrackKind::Audio => TrackKind::Audio,
                    PTrackKind::Sub => TrackKind::Sub,
                    PTrackKind::Video => return None,
                };
                Some(LocalTrack {
                    id: t.id,
                    kind,
                    ff_index: if t.external { None } else { t.ff_index },
                    lang: t.lang.clone(),
                    title: t.title.clone(),
                    codec: t.codec.clone(),
                    external_id: t
                        .external_filename
                        .as_deref()
                        .and_then(|f| self.added.get(&path_key(f)).copied()),
                })
            })
            .collect()
    }

    /// The room-level reference of a local track (for the host's track lists).
    pub fn track_ref(&self, t: &Track) -> Option<TrackRef> {
        let kind = match t.kind {
            PTrackKind::Audio => TrackKind::Audio,
            PTrackKind::Sub => TrackKind::Sub,
            PTrackKind::Video => return None,
        };
        if let Some(id) = t
            .external_filename
            .as_deref()
            .and_then(|f| self.added.get(&path_key(f)).copied())
        {
            return Some(TrackRef::External { external_id: id });
        }
        Some(TrackRef::Embedded {
            ff_index: t.ff_index,
            kind,
            lang: t.lang.clone(),
            title: t.title.clone(),
            codec: t.codec.clone(),
        })
    }

    /// Executes a sync-core player command on the real player.
    pub fn player_cmd(&mut self, player: &dyn Player, chosen: Option<&Path>, cmd: PlayerCmd) {
        if matches!(
            cmd,
            PlayerCmd::SelectSub(_) | PlayerCmd::SelectAudio(_) | PlayerCmd::AddExternal { .. }
        ) {
            tracing::debug!(cmd = ?cmd, "track command");
        }
        let r = match cmd {
            PlayerCmd::LoadLocal => match chosen {
                Some(p) => player.load(&p.to_string_lossy()),
                None => {
                    tracing::warn!("load requested but no local file was chosen");
                    Ok(())
                }
            },
            PlayerCmd::LoadTorrentStream { .. } => {
                tracing::info!("torrent streaming arrives in Milestone 7");
                Ok(())
            }
            PlayerCmd::Stop => player.unload(),
            PlayerCmd::SetPaused(p) => player.set_paused(p),
            PlayerCmd::Seek(ms) => player.seek(ms.max(0) as f64 / 1000.0),
            PlayerCmd::SetSpeed(p) => player.set_speed(p as f64 / 1000.0),
            PlayerCmd::SelectAudio(id) => player.select_track(
                PTrackKind::Audio,
                id.map(TrackSelection::Id).unwrap_or(TrackSelection::Off),
            ),
            PlayerCmd::SelectSub(id) => player.select_track(
                PTrackKind::Sub,
                id.map(TrackSelection::Id).unwrap_or(TrackSelection::Off),
            ),
            PlayerCmd::SubDelay(ms) => player.set_sub_delay(ms as f64 / 1000.0),
            PlayerCmd::AudioDelay(ms) => player.set_audio_delay(ms as f64 / 1000.0),
            PlayerCmd::AddExternal { external_id, kind } => {
                match self.file_for(external_id).cloned() {
                    Some(path) => {
                        self.added
                            .insert(path_key(&path.to_string_lossy()), external_id);
                        player.add_external(kind_of(kind), &path, false)
                    }
                    None => {
                        tracing::warn!(id = external_id.0, "external file not in the cache");
                        Ok(())
                    }
                }
            }
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "player command failed");
        }
    }
}

/// Player events that sync-core needs (positions are polled separately).
pub fn player_obs(ctx: &MediaCtx, player: &dyn Player, ev: &PlayerEvent) -> Option<PlayerObs> {
    match ev {
        PlayerEvent::FileLoaded { tracks } => Some(PlayerObs::FileLoaded {
            tracks: ctx.local_tracks(tracks),
            duration_ms: player.state().duration.map(|d| (d * 1000.0) as i64),
        }),
        PlayerEvent::TracksChanged { tracks } if player.state().file_loaded => {
            Some(PlayerObs::Tracks(ctx.local_tracks(tracks)))
        }
        PlayerEvent::SeekDone => Some(PlayerObs::SeekDone),
        PlayerEvent::EndFile {
            reason: syncwatch_player::EndReason::Error,
        } => Some(PlayerObs::LoadFailed),
        _ => None,
    }
}

/// Periodic position report for sync-core.
pub fn position_obs(player: &dyn Player) -> Option<PlayerObs> {
    let s = player.state();
    if !s.file_loaded {
        return None;
    }
    Some(PlayerObs::Position {
        position_ms: (s.time_pos? * 1000.0) as i64,
        paused: s.paused,
        buffering: s.paused_for_cache,
    })
}

// ---------------------------------------------------------------------------- host files ----

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedExternal {
    pub kind: &'static str,
    pub display_name: String,
    pub format: String,
    pub size: u64,
    #[serde(skip)]
    pub blake3: [u8; 32],
    #[serde(skip)]
    pub path: PathBuf,
    /// Text subtitle converted from another encoding.
    pub converted_from: Option<String>,
}

/// Validates the host's file (extension, size ceiling), converts text subtitles to UTF-8 and
/// copies it into the per-room cache, then hashes it. Friendly errors.
pub fn prepare_external(
    room_dir: &Path,
    src: &Path,
    kind: ExternalKind,
    no_limit: bool,
) -> Result<PreparedExternal, String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let allowed: &[&str] = match kind {
        ExternalKind::Subtitle => &SUB_EXT,
        ExternalKind::Audio => &AUDIO_EXT,
    };
    if !allowed.contains(&ext.as_str()) {
        return Err(format!(
            "That file type is not supported here. Use one of: {}.",
            allowed.join(", ")
        ));
    }
    let meta = std::fs::metadata(src).map_err(|_| "That file could not be read.".to_owned())?;
    let ceiling = match kind {
        ExternalKind::Subtitle => limits::DEFAULT_MAX_EXTERNAL_SUB,
        ExternalKind::Audio => limits::DEFAULT_MAX_EXTERNAL_AUDIO,
    };
    if !no_limit && meta.len() > ceiling {
        return Err(format!(
            "That file is larger than {} MB. Turn on \"No limit\" to send it anyway.",
            ceiling / (1024 * 1024)
        ));
    }
    std::fs::create_dir_all(room_dir)
        .map_err(|_| "The room cache could not be created.".to_owned())?;
    let name = crate::media::file_name(src);
    let stamp = blake3::hash(src.to_string_lossy().as_bytes()).to_hex()[..12].to_owned();
    let dest = room_dir.join(format!("host-{stamp}.{ext}"));
    let mut converted_from = None;
    if kind == ExternalKind::Subtitle && TEXT_SUB_EXT.contains(&ext.as_str()) {
        let bytes = std::fs::read(src).map_err(|_| "That file could not be read.".to_owned())?;
        let (utf8, from) = to_utf8(&bytes);
        converted_from = from;
        std::fs::write(&dest, utf8)
            .map_err(|_| "The room cache could not be written.".to_owned())?;
    } else {
        std::fs::copy(src, &dest).map_err(|_| "That file could not be copied.".to_owned())?;
    }
    let mut f = File::open(&dest).map_err(|_| "That file could not be read.".to_owned())?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut size = 0u64;
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|_| "That file could not be read.".to_owned())?;
        if n == 0 {
            break;
        }
        size += n as u64;
        h.update(&buf[..n]);
    }
    Ok(PreparedExternal {
        kind: match kind {
            ExternalKind::Subtitle => "sub",
            ExternalKind::Audio => "audio",
        },
        display_name: name,
        format: ext,
        size,
        blake3: *h.finalize().as_bytes(),
        path: dest,
        converted_from,
    })
}

/// Text subtitles to UTF-8 (spec section 9). Returns the UTF-8 bytes and, when converted, the
/// source encoding's name. A UTF-8 BOM is dropped; valid UTF-8 is kept as is.
pub fn to_utf8(bytes: &[u8]) -> (Vec<u8>, Option<String>) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (rest.to_vec(), None);
    }
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        let (text, _, _) = enc.decode(bytes);
        return (text.into_owned().into_bytes(), Some(enc.name().to_owned()));
    }
    if std::str::from_utf8(bytes).is_ok() {
        return (bytes.to_vec(), None);
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(bytes, true);
    let enc = det.guess(None, chardetng::Utf8Detection::Allow);
    let (text, _, _) = enc.decode(bytes);
    (text.into_owned().into_bytes(), Some(enc.name().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_become_utf8() {
        // Windows-1252 "café" in an SRT line.
        let cp1252 = b"1\r\n00:00:01,000 --> 00:00:02,000\r\ncaf\xe9 cr\xe8me\r\n";
        let (out, from) = to_utf8(cp1252);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "1\r\n00:00:01,000 --> 00:00:02,000\r\ncafé crème\r\n"
        );
        assert!(from.is_some());
        let (out, from) = to_utf8("\u{feff}héllo".as_bytes());
        assert_eq!(out, "héllo".as_bytes());
        assert!(from.is_none());
        let (out, from) = to_utf8("plain ünïcode".as_bytes());
        assert_eq!(out, "plain ünïcode".as_bytes());
        assert!(from.is_none());
    }

    #[test]
    fn prepare_checks_type_and_ceiling_and_copies_to_the_room_cache() {
        let dir = std::env::temp_dir().join(format!("sw-ext-{}", std::process::id()));
        let room = dir.join("room");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("subs.srt");
        std::fs::write(&src, b"1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        let p = prepare_external(&room, &src, ExternalKind::Subtitle, false).unwrap();
        assert!(p.path.starts_with(&room));
        assert_eq!(p.size, std::fs::metadata(&p.path).unwrap().len());
        assert_eq!(
            p.blake3,
            *blake3::hash(&std::fs::read(&p.path).unwrap()).as_bytes()
        );
        let exe = dir.join("evil.exe");
        std::fs::write(&exe, b"MZ").unwrap();
        assert!(prepare_external(&room, &exe, ExternalKind::Subtitle, false).is_err());
        assert!(
            prepare_external(&room, &src, ExternalKind::Audio, false).is_err(),
            "srt is not audio"
        );
        // Ceiling: a 21 MB subtitle is refused unless No limit is on.
        let big = dir.join("big.ass");
        let f = File::create(&big).unwrap();
        f.set_len(21 * 1024 * 1024).unwrap();
        assert!(
            prepare_external(&room, &big, ExternalKind::Subtitle, false)
                .unwrap_err()
                .contains("No limit")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chunks_write_in_order_and_the_cache_is_deleted() {
        let dir = std::env::temp_dir().join(format!("sw-cache-{}", std::process::id()));
        let mut ctx = MediaCtx::new(dir.join("room-1"));
        ctx.write_chunk(ExternalId(3), "srt", 0, b"hello ").unwrap();
        ctx.write_chunk(ExternalId(3), "srt", 6, b"world").unwrap();
        ctx.complete(ExternalId(3));
        let p = ctx.file_for(ExternalId(3)).unwrap().clone();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello world");
        let entry = ExternalEntry {
            external_id: ExternalId(3),
            kind: ExternalKind::Subtitle,
            display_name: "x.srt".into(),
            format: "srt".into(),
            size: 11,
            blake3: *blake3::hash(b"hello world").as_bytes(),
            added_at_epoch: TrackEpoch(1),
        };
        assert!(ctx.cached_identical(&entry), "identical file: no transfer");
        assert!(ctx.has_cache_files());
        ctx.clear();
        assert!(
            !dir.join("room-1").exists(),
            "per-room cache deleted at room end"
        );
        assert!(ctx.file_for(ExternalId(3)).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! Host-owned track state (spec section 9).

use crate::ProtocolError;
use crate::ids::*;
use crate::limits::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Audio,
    Sub,
}

/// A reference to a track that every member can resolve against their own copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackRef {
    /// Embedded stream: matched by ff-index first, then by kind+lang+title+codec.
    Embedded {
        ff_index: Option<i64>,
        kind: TrackKind,
        lang: Option<String>,
        title: Option<String>,
        codec: Option<String>,
    },
    /// A file from the room's external manifest.
    External { external_id: ExternalId },
}

/// Audio/subtitle selection. `Default` = whatever the player picks for a freshly loaded file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Selection {
    Default,
    Off,
    Track(TrackRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExternalKind {
    Audio,
    Subtitle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalEntry {
    pub external_id: ExternalId,
    pub kind: ExternalKind,
    /// Sanitized file name for display.
    pub display_name: String,
    /// Lower-case extension, e.g. "ass", "srt", "mka".
    pub format: String,
    pub size: u64,
    pub blake3: [u8; 32],
    pub added_at_epoch: TrackEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackState {
    pub track_epoch: TrackEpoch,
    pub audio: Selection,
    pub subtitle: Selection,
    pub sub_delay_ms: i32,
    pub audio_delay_ms: i32,
    pub manifest: Vec<ExternalEntry>,
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            track_epoch: TrackEpoch(0),
            audio: Selection::Default,
            subtitle: Selection::Default,
            sub_delay_ms: 0,
            audio_delay_ms: 0,
            manifest: Vec::new(),
        }
    }
}

/// Allowed external formats (spec section 11: extension allowlist).
pub const SUB_FORMATS: &[&str] = &["ass", "ssa", "srt", "vtt", "sub", "sup", "idx"];
pub const AUDIO_FORMATS: &[&str] = &[
    "mka", "aac", "ac3", "eac3", "dts", "flac", "mp3", "opus", "ogg", "wav", "m4a", "thd",
];

/// Delays are bounded to +-10 minutes.
pub const MAX_DELAY_MS: i32 = 600_000;

fn opt_len(s: &Option<String>, max: usize) -> bool {
    s.as_ref().is_none_or(|v| v.len() <= max)
}

impl TrackRef {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let TrackRef::Embedded {
            lang, title, codec, ..
        } = self
            && !(opt_len(lang, MAX_LANG_BYTES)
                && opt_len(title, MAX_TITLE_BYTES)
                && opt_len(codec, MAX_CODEC_BYTES))
        {
            return Err(ProtocolError::Limit("track ref"));
        }
        Ok(())
    }
}

impl ExternalEntry {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        crate::media::check_file_name(&self.display_name)?;
        let allowed = match self.kind {
            ExternalKind::Subtitle => SUB_FORMATS,
            ExternalKind::Audio => AUDIO_FORMATS,
        };
        if self.format.len() > MAX_FORMAT_BYTES || !allowed.contains(&self.format.as_str()) {
            return Err(ProtocolError::Limit("external format"));
        }
        Ok(())
    }
}

impl TrackState {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        for s in [&self.audio, &self.subtitle] {
            if let Selection::Track(r) = s {
                r.validate()?;
            }
        }
        if self.sub_delay_ms.abs() > MAX_DELAY_MS || self.audio_delay_ms.abs() > MAX_DELAY_MS {
            return Err(ProtocolError::Limit("delay"));
        }
        if self.manifest.len() > MAX_MANIFEST_ENTRIES {
            return Err(ProtocolError::Limit("manifest size"));
        }
        for e in &self.manifest {
            e.validate()?;
        }
        Ok(())
    }

    pub fn external(&self, id: ExternalId) -> Option<&ExternalEntry> {
        self.manifest.iter().find(|e| e.external_id == id)
    }
}

//! Track model and pure parsing of mpv's `track-list/N/*` sub-properties.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackKind {
    Video,
    Audio,
    Sub,
}

impl TrackKind {
    /// mpv's `type` string for this kind.
    pub fn mpv_type(self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            TrackKind::Sub => "sub",
        }
    }

    /// The mpv property that selects a track of this kind.
    pub fn select_property(self) -> &'static str {
        match self {
            TrackKind::Video => "vid",
            TrackKind::Audio => "aid",
            TrackKind::Sub => "sid",
        }
    }

    pub fn from_mpv(s: &str) -> Option<Self> {
        match s {
            "video" => Some(TrackKind::Video),
            "audio" => Some(TrackKind::Audio),
            "sub" => Some(TrackKind::Sub),
            _ => None,
        }
    }
}

/// Selection of a track: a specific mpv track id, or off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "id", rename_all = "camelCase")]
pub enum TrackSelection {
    Id(i64),
    Off,
}

impl TrackSelection {
    /// Value to write to `aid` / `sid`.
    pub fn to_mpv_value(self) -> String {
        match self {
            TrackSelection::Id(id) => id.to_string(),
            TrackSelection::Off => "no".to_owned(),
        }
    }
}

/// One track as reported by mpv.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    /// mpv's local track id (unique per kind, NOT stable across members).
    pub id: i64,
    pub kind: TrackKind,
    /// FFmpeg stream index inside the container (stable across members with the same file).
    pub ff_index: Option<i64>,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
    pub selected: bool,
    pub external: bool,
    pub default: bool,
    pub forced: bool,
    /// Channel count for audio, if known.
    pub channels: Option<i64>,
}

impl Track {
    /// Human-readable label, e.g. "English · SDH · subrip".
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(lang) = &self.lang {
            parts.push(lang.clone());
        }
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if parts.is_empty() {
            parts.push(format!("Track {}", self.id));
        }
        if let Some(codec) = &self.codec {
            parts.push(codec.clone());
        }
        parts.join(" · ")
    }
}

/// A raw property value as read from mpv.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    Str(String),
    Int(i64),
    Flag(bool),
}

/// Reads the track list using a property getter (`name -> value`). Pure, so it can be tested
/// without libmpv. Tracks with an unknown type or missing id are skipped; the count is
/// capped to avoid pathological files.
pub fn read_track_list<F>(get: F) -> Vec<Track>
where
    F: Fn(&str) -> Option<RawValue>,
{
    const MAX_TRACKS: i64 = 512;
    let count = match get("track-list/count") {
        Some(RawValue::Int(n)) => n.clamp(0, MAX_TRACKS),
        _ => 0,
    };
    let s = |name: String| match get(&name) {
        Some(RawValue::Str(v)) if !v.is_empty() => Some(v),
        _ => None,
    };
    let i = |name: String| match get(&name) {
        Some(RawValue::Int(v)) => Some(v),
        _ => None,
    };
    let f = |name: String| matches!(get(&name), Some(RawValue::Flag(true)));

    let mut out = Vec::new();
    for n in 0..count {
        let p = format!("track-list/{n}");
        let Some(kind) = s(format!("{p}/type"))
            .as_deref()
            .and_then(TrackKind::from_mpv)
        else {
            continue;
        };
        let Some(id) = i(format!("{p}/id")) else {
            continue;
        };
        out.push(Track {
            id,
            kind,
            ff_index: i(format!("{p}/ff-index")),
            lang: s(format!("{p}/lang")),
            title: s(format!("{p}/title")),
            codec: s(format!("{p}/codec")),
            selected: f(format!("{p}/selected")),
            external: f(format!("{p}/external")),
            default: f(format!("{p}/default")),
            forced: f(format!("{p}/forced")),
            channels: i(format!("{p}/demux-channel-count")),
        });
    }
    out
}

/// The selected track of a kind, if any.
pub fn selected(tracks: &[Track], kind: TrackKind) -> Option<&Track> {
    tracks.iter().find(|t| t.kind == kind && t.selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake(map: &[(&str, RawValue)]) -> impl Fn(&str) -> Option<RawValue> + use<> {
        let m: HashMap<String, RawValue> = map
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn parses_tracks_and_skips_unknown() {
        let get = fake(&[
            ("track-list/count", RawValue::Int(4)),
            ("track-list/0/type", RawValue::Str("video".into())),
            ("track-list/0/id", RawValue::Int(1)),
            ("track-list/0/ff-index", RawValue::Int(0)),
            ("track-list/1/type", RawValue::Str("audio".into())),
            ("track-list/1/id", RawValue::Int(1)),
            ("track-list/1/lang", RawValue::Str("jpn".into())),
            ("track-list/1/codec", RawValue::Str("eac3".into())),
            ("track-list/1/selected", RawValue::Flag(true)),
            ("track-list/2/type", RawValue::Str("sub".into())),
            ("track-list/2/id", RawValue::Int(1)),
            ("track-list/2/title", RawValue::Str("SDH".into())),
            ("track-list/2/external", RawValue::Flag(true)),
            ("track-list/3/type", RawValue::Str("weird".into())),
            ("track-list/3/id", RawValue::Int(9)),
        ]);
        let t = read_track_list(get);
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].kind, TrackKind::Audio);
        assert!(t[1].selected);
        assert_eq!(t[1].label(), "jpn · eac3");
        assert!(t[2].external);
        assert_eq!(selected(&t, TrackKind::Audio).map(|x| x.id), Some(1));
        assert_eq!(selected(&t, TrackKind::Sub), None);
    }

    #[test]
    fn count_is_capped_and_negative_is_zero() {
        assert!(read_track_list(fake(&[("track-list/count", RawValue::Int(-5))])).is_empty());
        let t = read_track_list(fake(&[("track-list/count", RawValue::Int(1_000_000))]));
        assert!(t.is_empty());
    }

    #[test]
    fn selection_values() {
        assert_eq!(TrackSelection::Id(3).to_mpv_value(), "3");
        assert_eq!(TrackSelection::Off.to_mpv_value(), "no");
        assert_eq!(TrackKind::Sub.select_property(), "sid");
    }

    #[test]
    fn untitled_track_label() {
        let t = Track {
            id: 7,
            kind: TrackKind::Sub,
            ff_index: None,
            lang: None,
            title: None,
            codec: None,
            selected: false,
            external: false,
            default: false,
            forced: false,
            channels: None,
        };
        assert_eq!(t.label(), "Track 7");
    }
}

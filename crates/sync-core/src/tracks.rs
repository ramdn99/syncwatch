//! Track resolution and application (spec section 9).
//!
//! The host owns [`TrackState`]. Each participant resolves every [`TrackRef`] against its OWN
//! player's track list: embedded refs by ff-index first, then by kind+lang+title+codec;
//! external refs through the app-provided `LocalTrack::external_id`. Unresolvable refs produce a
//! non-blocking warning and an "unresolved" status. While an external file is still
//! downloading, the previous track stays selected.

use crate::types::*;
use std::collections::HashSet;
use syncwatch_protocol::*;

/// Resolves a ref to a local track id.
pub fn resolve(r: &TrackRef, local: &[LocalTrack]) -> Option<i64> {
    match r {
        TrackRef::External { external_id } => local
            .iter()
            .find(|t| t.external_id == Some(*external_id))
            .map(|t| t.id),
        TrackRef::Embedded {
            ff_index,
            kind,
            lang,
            title,
            codec,
        } => {
            let embedded = || {
                local
                    .iter()
                    .filter(|t| t.kind == *kind && t.external_id.is_none())
            };
            if let Some(ff) = ff_index
                && let Some(t) = embedded().find(|t| t.ff_index == Some(*ff))
            {
                return Some(t.id);
            }
            embedded()
                .find(|t| t.lang == *lang && t.title == *title && t.codec == *codec)
                .map(|t| t.id)
        }
    }
}

/// Builds the ref describing a local track (used by the host's UI selections).
pub fn track_ref_for(t: &LocalTrack) -> TrackRef {
    match t.external_id {
        Some(external_id) => TrackRef::External { external_id },
        None => TrackRef::Embedded {
            ff_index: t.ff_index,
            kind: t.kind,
            lang: t.lang.clone(),
            title: t.title.clone(),
            codec: t.codec.clone(),
        },
    }
}

/// What has been sent to the player for one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applied {
    Untouched,
    Off,
    Id(i64),
}

#[derive(Debug, Clone)]
pub struct TrackApplier {
    pub desired: TrackState,
    local: Vec<LocalTrack>,
    loaded: bool,
    /// Externals verified and handed to the player.
    externals_ready: HashSet<ExternalId>,
    /// Externals still downloading (keep the previous track meanwhile).
    externals_pending: HashSet<ExternalId>,
    audio: Applied,
    sub: Applied,
    sub_delay: Option<i32>,
    audio_delay: Option<i32>,
    pub status: TrackSyncStatus,
    warned: HashSet<TrackKind>,
}

impl Default for TrackApplier {
    fn default() -> Self {
        Self {
            desired: TrackState::default(),
            local: Vec::new(),
            loaded: false,
            externals_ready: HashSet::new(),
            externals_pending: HashSet::new(),
            audio: Applied::Untouched,
            sub: Applied::Untouched,
            sub_delay: None,
            audio_delay: None,
            status: TrackSyncStatus::InSync,
            warned: HashSet::new(),
        }
    }
}

impl TrackApplier {
    /// New media: forget everything about the old file.
    pub fn reset(&mut self, desired: TrackState) {
        *self = TrackApplier {
            desired,
            ..TrackApplier::default()
        };
    }

    pub fn local_tracks(&self) -> &[LocalTrack] {
        &self.local
    }

    pub fn set_desired(&mut self, desired: TrackState, out: &mut Out) {
        if desired.track_epoch < self.desired.track_epoch {
            return; // stale
        }
        self.warned.clear();
        self.desired = desired;
        self.apply(out);
    }

    pub fn on_loaded(&mut self, tracks: Vec<LocalTrack>, out: &mut Out) {
        self.loaded = true;
        self.local = tracks;
        self.audio = Applied::Untouched;
        self.sub = Applied::Untouched;
        self.sub_delay = None;
        self.audio_delay = None;
        // Externals already in the cache must be re-added to the freshly loaded file.
        let ready: Vec<ExternalId> = self.externals_ready.iter().copied().collect();
        for id in ready {
            if let Some(e) = self.desired.external(id) {
                out.player(PlayerCmd::AddExternal {
                    external_id: id,
                    kind: e.kind,
                });
            }
        }
        self.apply(out);
    }

    pub fn on_unloaded(&mut self) {
        self.loaded = false;
        self.local.clear();
    }

    pub fn on_tracks(&mut self, tracks: Vec<LocalTrack>, out: &mut Out) {
        self.local = tracks;
        self.apply(out);
    }

    pub fn external_pending(&mut self, id: ExternalId) {
        self.externals_pending.insert(id);
    }

    /// The external file is verified; add it to the player (selection follows once the player
    /// reports the new track).
    pub fn external_ready(&mut self, id: ExternalId, out: &mut Out) {
        self.externals_pending.remove(&id);
        if self.externals_ready.insert(id)
            && self.loaded
            && let Some(e) = self.desired.external(id)
        {
            out.player(PlayerCmd::AddExternal {
                external_id: id,
                kind: e.kind,
            });
        }
        self.apply(out);
    }

    pub fn external_abandoned(&mut self, id: ExternalId, out: &mut Out) {
        self.externals_pending.remove(&id);
        self.apply(out);
    }

    fn resolve_selection(&self, sel: &Selection) -> Result<Option<Applied>, bool> {
        // Ok(None) = leave as is; Ok(Some(x)) = apply x; Err(pending) = cannot resolve.
        match sel {
            Selection::Default => Ok(None),
            Selection::Off => Ok(Some(Applied::Off)),
            Selection::Track(r) => match resolve(r, &self.local) {
                Some(id) => Ok(Some(Applied::Id(id))),
                None => {
                    let pending = matches!(r, TrackRef::External { external_id }
                        if self.externals_pending.contains(external_id)
                            || (self.externals_ready.contains(external_id)));
                    Err(pending)
                }
            },
        }
    }

    fn apply(&mut self, out: &mut Out) {
        if !self.loaded {
            self.status = TrackSyncStatus::Applying;
            return;
        }
        let mut unresolved = false;
        let mut pending = false;
        for kind in [TrackKind::Audio, TrackKind::Sub] {
            let sel = match kind {
                TrackKind::Audio => self.desired.audio.clone(),
                TrackKind::Sub => self.desired.subtitle.clone(),
            };
            match self.resolve_selection(&sel) {
                Ok(None) => {}
                Ok(Some(target)) => {
                    let slot = match kind {
                        TrackKind::Audio => &mut self.audio,
                        TrackKind::Sub => &mut self.sub,
                    };
                    if *slot != target {
                        *slot = target;
                        let v = match target {
                            Applied::Id(id) => Some(id),
                            _ => None,
                        };
                        out.player(match kind {
                            TrackKind::Audio => PlayerCmd::SelectAudio(v),
                            TrackKind::Sub => PlayerCmd::SelectSub(v),
                        });
                    }
                }
                Err(true) => pending = true, // keep the previous track while downloading
                Err(false) => {
                    unresolved = true;
                    if self.warned.insert(kind) {
                        out.ui(UiEvent::TrackUnresolved { kind });
                    }
                }
            }
        }
        if self.sub_delay != Some(self.desired.sub_delay_ms) {
            self.sub_delay = Some(self.desired.sub_delay_ms);
            out.player(PlayerCmd::SubDelay(self.desired.sub_delay_ms));
        }
        if self.audio_delay != Some(self.desired.audio_delay_ms) {
            self.audio_delay = Some(self.desired.audio_delay_ms);
            out.player(PlayerCmd::AudioDelay(self.desired.audio_delay_ms));
        }
        self.status = if unresolved {
            TrackSyncStatus::Unresolved
        } else if pending {
            TrackSyncStatus::Applying
        } else {
            TrackSyncStatus::InSync
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn lt(
        id: i64,
        kind: TrackKind,
        ff: i64,
        lang: &str,
        title: Option<&str>,
    ) -> LocalTrack {
        LocalTrack {
            id,
            kind,
            ff_index: Some(ff),
            lang: Some(lang.into()),
            title: title.map(Into::into),
            codec: Some("subrip".into()),
            external_id: None,
        }
    }

    #[test]
    fn ff_index_first_then_metadata() {
        let local = vec![
            lt(1, TrackKind::Sub, 3, "eng", None),
            lt(2, TrackKind::Sub, 5, "eng", Some("SDH")),
        ];
        let by_ff = TrackRef::Embedded {
            ff_index: Some(5),
            kind: TrackKind::Sub,
            lang: Some("eng".into()),
            title: Some("SDH".into()),
            codec: Some("subrip".into()),
        };
        assert_eq!(resolve(&by_ff, &local), Some(2));
        // Different ff-index (remux) but same metadata: fallback match.
        let remux = TrackRef::Embedded {
            ff_index: Some(9),
            kind: TrackKind::Sub,
            lang: Some("eng".into()),
            title: Some("SDH".into()),
            codec: Some("subrip".into()),
        };
        assert_eq!(resolve(&remux, &local), Some(2));
        let missing = TrackRef::Embedded {
            ff_index: Some(9),
            kind: TrackKind::Sub,
            lang: Some("jpn".into()),
            title: None,
            codec: None,
        };
        assert_eq!(resolve(&missing, &local), None);
    }

    #[test]
    fn applier_selects_once_and_warns_on_unresolved() {
        let mut a = TrackApplier::default();
        let mut out = Out::default();
        let local = vec![
            lt(1, TrackKind::Sub, 3, "eng", None),
            lt(7, TrackKind::Audio, 1, "jpn", None),
        ];
        let mut st = TrackState {
            track_epoch: TrackEpoch(1),
            subtitle: Selection::Track(track_ref_for(&local[0])),
            audio: Selection::Off,
            ..TrackState::default()
        };
        a.on_loaded(local.clone(), &mut out);
        a.set_desired(st.clone(), &mut out);
        let cmds: Vec<_> = std::mem::take(&mut out.0);
        assert!(cmds.contains(&Effect::Player(PlayerCmd::SelectSub(Some(1)))));
        assert!(cmds.contains(&Effect::Player(PlayerCmd::SelectAudio(None))));
        assert_eq!(a.status, TrackSyncStatus::InSync);
        // Same state again: nothing to do.
        a.on_tracks(local.clone(), &mut out);
        assert!(out.0.is_empty());
        // Unresolvable: warning once, previous selection kept.
        st.track_epoch = TrackEpoch(2);
        st.subtitle = Selection::Track(TrackRef::Embedded {
            ff_index: Some(99),
            kind: TrackKind::Sub,
            lang: Some("xx".into()),
            title: None,
            codec: None,
        });
        a.set_desired(st, &mut out);
        assert_eq!(
            out.0,
            vec![Effect::Ui(UiEvent::TrackUnresolved {
                kind: TrackKind::Sub
            })]
        );
        assert_eq!(a.status, TrackSyncStatus::Unresolved);
    }

    #[test]
    fn external_waits_for_download_then_selects() {
        let mut a = TrackApplier::default();
        let mut out = Out::default();
        let entry = ExternalEntry {
            external_id: ExternalId(4),
            kind: ExternalKind::Subtitle,
            display_name: "x.ass".into(),
            format: "ass".into(),
            size: 10,
            blake3: [0; 32],
            added_at_epoch: TrackEpoch(1),
        };
        let st = TrackState {
            track_epoch: TrackEpoch(1),
            subtitle: Selection::Track(TrackRef::External {
                external_id: ExternalId(4),
            }),
            manifest: vec![entry],
            ..TrackState::default()
        };
        a.on_loaded(vec![lt(1, TrackKind::Sub, 3, "eng", None)], &mut out);
        a.external_pending(ExternalId(4));
        a.set_desired(st, &mut out);
        assert_eq!(a.status, TrackSyncStatus::Applying);
        assert!(
            !out.0
                .iter()
                .any(|e| matches!(e, Effect::Player(PlayerCmd::SelectSub(_))))
        );
        out.0.clear();
        a.external_ready(ExternalId(4), &mut out);
        assert!(out.0.contains(&Effect::Player(PlayerCmd::AddExternal {
            external_id: ExternalId(4),
            kind: ExternalKind::Subtitle
        })));
        let mut with_ext = vec![lt(1, TrackKind::Sub, 3, "eng", None)];
        with_ext.push(LocalTrack {
            id: 2,
            external_id: Some(ExternalId(4)),
            ff_index: None,
            ..lt(2, TrackKind::Sub, 0, "eng", None)
        });
        out.0.clear();
        a.on_tracks(with_ext, &mut out);
        assert!(
            out.0
                .contains(&Effect::Player(PlayerCmd::SelectSub(Some(2))))
        );
        assert_eq!(a.status, TrackSyncStatus::InSync);
    }
}

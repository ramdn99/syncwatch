//! Member-side download of the host's external subtitle/audio files (spec section 9).
//!
//! Only manifest entries can be requested or received. Chunks must arrive in order and fit the
//! manifest size; the whole file must match the manifest blake3. Subtitles are fetched before
//! audio. No rate limiter: the transport's priority queue keeps control traffic responsive.

use crate::types::*;
use std::collections::{HashSet, VecDeque};
use syncwatch_protocol::*;

struct Incoming {
    id: ExternalId,
    kind: ExternalKind,
    size: u64,
    blake3: [u8; 32],
    received: u64,
    hasher: blake3::Hasher,
}

/// Why a chunk was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkError {
    /// Not a manifest entry, or not requested: protocol violation.
    NotRequested,
    /// Wrong offset or exceeds the manifest size: protocol violation.
    OutOfBounds,
}

#[derive(Default)]
pub struct Downloads {
    queue: VecDeque<ExternalId>,
    active: Option<Incoming>,
    /// Audio downloads preempted by a subtitle; resumed from their offset afterwards.
    suspended: Vec<Incoming>,
    done: HashSet<ExternalId>,
    declined: HashSet<ExternalId>,
}

impl std::fmt::Debug for Downloads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Downloads")
            .field("queue", &self.queue)
            .field(
                "active",
                &self.active.as_ref().map(|a| (a.id, a.received, a.size)),
            )
            .field("done", &self.done)
            .finish()
    }
}

impl Downloads {
    /// Media change without carrying externals over: drop everything (partial files deleted).
    pub fn reset(&mut self, out: &mut Out) {
        if let Some(a) = self.active.take() {
            out.push(Effect::DiscardExternal { external_id: a.id });
        }
        for s in self.suspended.drain(..) {
            out.push(Effect::DiscardExternal { external_id: s.id });
        }
        *self = Downloads::default();
    }

    pub fn is_done(&self, id: ExternalId) -> bool {
        self.done.contains(&id)
    }

    /// New manifest from the host: queue entries we neither have nor declined.
    pub fn on_manifest(
        &mut self,
        manifest: &[ExternalEntry],
        tracks: &mut crate::tracks::TrackApplier,
        out: &mut Out,
    ) {
        // Entries removed from the manifest are dropped.
        let ids: HashSet<ExternalId> = manifest.iter().map(|e| e.external_id).collect();
        self.queue.retain(|id| ids.contains(id));
        if self.active.as_ref().is_some_and(|a| !ids.contains(&a.id))
            && let Some(a) = self.active.take()
        {
            out.push(Effect::DiscardExternal { external_id: a.id });
        }
        let mut new: Vec<&ExternalEntry> = manifest
            .iter()
            .filter(|e| {
                !self.done.contains(&e.external_id)
                    && !self.declined.contains(&e.external_id)
                    && !self.queue.contains(&e.external_id)
                    && self.active.as_ref().is_none_or(|a| a.id != e.external_id)
            })
            .collect();
        new.retain(|e| self.suspended.iter().all(|s| s.id != e.external_id));
        // Subtitles first: they go to the FRONT of the queue and preempt an audio download.
        new.sort_by_key(|e| (e.kind == ExternalKind::Audio, e.external_id));
        let mut subs = Vec::new();
        for e in new {
            tracks.external_pending(e.external_id);
            if e.kind == ExternalKind::Subtitle {
                subs.push(e.external_id);
            } else {
                self.queue.push_back(e.external_id);
            }
        }
        for id in subs.into_iter().rev() {
            self.queue.push_front(id);
        }
        if self
            .active
            .as_ref()
            .is_some_and(|a| a.kind == ExternalKind::Audio)
            && self
                .queue
                .front()
                .and_then(|id| manifest.iter().find(|e| e.external_id == *id))
                .is_some_and(|e| e.kind == ExternalKind::Subtitle)
            && let Some(a) = self.active.take()
        {
            out.push(Effect::SendToHost(Message::FileCancel {
                external_id: a.id,
            }));
            self.suspended.push(a);
        }
        self.start_next(manifest, out);
    }

    fn start_next(&mut self, manifest: &[ExternalEntry], out: &mut Out) {
        while self.active.is_none() {
            // Pending subtitles first, then preempted audio (resumed), then the rest.
            let next_is_sub = self
                .queue
                .front()
                .and_then(|id| manifest.iter().find(|e| e.external_id == *id))
                .is_some_and(|e| e.kind == ExternalKind::Subtitle);
            if !next_is_sub && let Some(s) = self.suspended.pop() {
                out.push(Effect::SendToHost(Message::FileRequest {
                    external_id: s.id,
                    offset: s.received,
                }));
                self.active = Some(s);
                return;
            }
            let Some(id) = self.queue.pop_front() else {
                return;
            };
            let Some(e) = manifest.iter().find(|e| e.external_id == id) else {
                continue;
            };
            self.active = Some(Incoming {
                id,
                kind: e.kind,
                size: e.size,
                blake3: e.blake3,
                received: 0,
                hasher: blake3::Hasher::new(),
            });
            out.push(Effect::SendToHost(Message::FileRequest {
                external_id: id,
                offset: 0,
            }));
        }
    }

    /// The app already holds an identical file.
    pub fn on_cached(
        &mut self,
        id: ExternalId,
        manifest: &[ExternalEntry],
        tracks: &mut crate::tracks::TrackApplier,
        out: &mut Out,
    ) {
        if manifest.iter().all(|e| e.external_id != id) {
            return;
        }
        self.queue.retain(|x| *x != id);
        if self.active.as_ref().is_some_and(|a| a.id == id) {
            self.active = None;
            out.push(Effect::SendToHost(Message::FileCancel { external_id: id }));
        }
        if self.done.insert(id) {
            out.push(Effect::SendToHost(Message::FileHave { external_id: id }));
            tracks.external_ready(id, out);
        }
        self.start_next(manifest, out);
    }

    /// The member cancels their own download ("declined").
    pub fn cancel(
        &mut self,
        id: ExternalId,
        manifest: &[ExternalEntry],
        tracks: &mut crate::tracks::TrackApplier,
        out: &mut Out,
    ) {
        self.declined.insert(id);
        self.queue.retain(|x| *x != id);
        self.suspended.retain(|s| s.id != id);
        if self.active.as_ref().is_some_and(|a| a.id == id) {
            self.active = None;
            out.push(Effect::DiscardExternal { external_id: id });
        }
        out.push(Effect::SendToHost(Message::FileCancel { external_id: id }));
        tracks.external_abandoned(id, out);
        self.start_next(manifest, out);
    }

    pub fn on_chunk(
        &mut self,
        id: ExternalId,
        offset: u64,
        data: Vec<u8>,
        manifest: &[ExternalEntry],
        tracks: &mut crate::tracks::TrackApplier,
        out: &mut Out,
    ) -> Result<(), ChunkError> {
        let Some(a) = self.active.as_mut().filter(|a| a.id == id) else {
            // Late chunks for something we cancelled or finished are harmless; anything else
            // (non-manifest ids, never requested) is a violation.
            if self.declined.contains(&id)
                || self.done.contains(&id)
                || self.suspended.iter().any(|s| s.id == id)
            {
                return Ok(());
            }
            return Err(ChunkError::NotRequested);
        };
        let len = data.len() as u64;
        if offset < a.received {
            // Duplicate from an upload that was cancelled and resumed: already have it.
            return Ok(());
        }
        if offset != a.received || a.received + len > a.size {
            return Err(ChunkError::OutOfBounds);
        }
        a.hasher.update(&data);
        a.received += len;
        let (received, size) = (a.received, a.size);
        out.push(Effect::WriteExternalChunk {
            external_id: id,
            offset,
            data,
        });
        out.ui(UiEvent::ExternalProgress {
            external_id: id,
            received,
            size,
        });
        if received == size {
            let Some(a) = self.active.take() else {
                return Ok(());
            };
            if *a.hasher.finalize().as_bytes() == a.blake3 {
                self.done.insert(id);
                out.push(Effect::ExternalComplete { external_id: id });
                tracks.external_ready(id, out);
            } else {
                out.push(Effect::DiscardExternal { external_id: id });
                out.ui(UiEvent::ExternalFailed(id));
                tracks.external_abandoned(id, out);
            }
            self.start_next(manifest, out);
        }
        Ok(())
    }

    /// Download progress for status reports.
    pub fn progress_permille(&self) -> Option<u16> {
        self.active
            .as_ref()
            .map(|a| ((a.received * 1000) / a.size.max(1)).min(1000) as u16)
    }

    /// After a reconnect: resume the active download where it stopped.
    pub fn resume(&self, out: &mut Out) {
        if let Some(a) = &self.active {
            out.push(Effect::SendToHost(Message::FileRequest {
                external_id: a.id,
                offset: a.received,
            }));
        }
    }
}

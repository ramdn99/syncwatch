//! Host-side ready gate (spec section 9): after a seek, a media change or buffering, playback
//! (re)starts only once the members that hold the media are ready, or after a timeout, or when
//! the host chooses "resume without X". Members that are missing the file or still verifying
//! never block others unless the host enabled "wait for everyone".

use std::collections::HashSet;
use syncwatch_protocol::*;

#[derive(Debug, Clone)]
pub struct Pending {
    pub since_us: i64,
    /// Members the host chose to resume without (for this gate only).
    pub ignore: HashSet<MemberId>,
}

#[derive(Debug, Clone)]
pub struct Gate {
    pub wait_for_everyone: bool,
    pub timeout_us: i64,
    pub pending: Option<Pending>,
    /// Members the room resumed without (timeout or "resume without X"): they do not pause the
    /// room again by buffering until they have reported ready once.
    pub excused: HashSet<MemberId>,
}

impl Gate {
    pub fn new(timeout_us: i64) -> Self {
        Self {
            wait_for_everyone: false,
            timeout_us,
            pending: None,
            excused: HashSet::new(),
        }
    }

    /// Why `status` blocks a (re)start of `room`, if it does.
    pub fn blocks(
        &self,
        status: Option<&MemberStatus>,
        room: &RoomPlayback,
    ) -> Option<NotReadyReason> {
        let Some(s) = status else {
            return Some(NotReadyReason::Loading);
        };
        if s.media_epoch != room.media_epoch {
            return Some(NotReadyReason::Loading);
        }
        match s.media {
            MediaStatus::Ready | MediaStatus::Mismatch => {
                if s.buffering {
                    Some(NotReadyReason::Buffering)
                } else if s.ready_epoch < room.epoch {
                    Some(NotReadyReason::Loading)
                } else {
                    None
                }
            }
            MediaStatus::Missing => self
                .wait_for_everyone
                .then_some(NotReadyReason::MissingFile),
            MediaStatus::Verifying => self.wait_for_everyone.then_some(NotReadyReason::Verifying),
            MediaStatus::Downloading { .. } => self
                .wait_for_everyone
                .then_some(NotReadyReason::Downloading),
            MediaStatus::NoMedia | MediaStatus::Declined | MediaStatus::Unavailable => None,
        }
    }

    pub fn timed_out(&self, now: i64) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| now - p.since_us >= self.timeout_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(media: MediaStatus, ready: u64, buffering: bool) -> MemberStatus {
        MemberStatus {
            media_epoch: MediaEpoch(1),
            media,
            ready_epoch: Epoch(ready),
            buffering,
            tracks: TrackSyncStatus::InSync,
            rtt_ms: None,
        }
    }

    #[test]
    fn missing_and_verifying_block_only_when_waiting_for_everyone() {
        let room = RoomPlayback {
            epoch: Epoch(5),
            ..RoomPlayback::initial(MediaEpoch(1), 0)
        };
        let mut g = Gate::new(1);
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Missing, 0, false)), &room),
            None
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Verifying, 0, false)), &room),
            None
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Declined, 0, false)), &room),
            None
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Ready, 4, false)), &room),
            Some(NotReadyReason::Loading)
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Ready, 5, true)), &room),
            Some(NotReadyReason::Buffering)
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Mismatch, 5, false)), &room),
            None
        );
        g.wait_for_everyone = true;
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Missing, 0, false)), &room),
            Some(NotReadyReason::MissingFile)
        );
        assert_eq!(
            g.blocks(Some(&st(MediaStatus::Declined, 0, false)), &room),
            None
        );
    }
}

//! SyncWatch torrents (spec section 10, ADRs 0009 and 0010).
//!
//! - [`policy`]: the pure PiecePolicy (cursors, playable, buffering, VBR byte map).
//! - [`ready`]: readiness at the ROOM position (window coverage + hysteresis, ADR 0012).
//! - [`juggle`]: cursors -> stream operations (pure diff + a small driver trait).
//! - [`server`]: the loopback HTTP server feeding mpv (stream, then the finished file).
//! - [`announce`]: our own tracker "stopped" announce.
//! - [`cache`], [`validate`]: cache folders, finished registry, size cap; path validation.
//! - [`engine`]: the librqbit adapter (an actor; the session exists only while needed).

pub mod announce;
pub mod cache;
pub mod engine;
pub mod juggle;
pub mod meta;
pub mod policy;
pub mod ready;
pub mod server;
pub mod validate;

pub use engine::{
    COMPLETE_MESSAGE, EngineConfig, EngineError, EngineEvent, EngineHandle, FileEntry, Phase,
    RoomTarget, StopReason, TorrentInfo, TorrentStatus,
};
pub use ready::{Readiness, ReadyConfig};

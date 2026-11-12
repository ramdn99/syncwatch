//! Size limits for every field of every message (spec section 11: validate and size-limit every
//! incoming message).

/// Largest encoded frame accepted (a 16 KiB chunk plus headers fits comfortably).
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// External-file chunk size (spec section 8: 16 KiB chunks).
pub const CHUNK_BYTES: usize = 16 * 1024;
/// Protocol ceiling for torrent METADATA offered by the host (the .torrent / info dictionary,
/// never media). The default cap actually used is 5 MB (configurable below this ceiling).
pub const MAX_TORRENT_META_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_TORRENT_META_CAP: usize = 5 * 1024 * 1024;
/// Display names: bytes on the wire (graphemes are checked by sync-core).
pub const MAX_NAME_BYTES: usize = 128;
/// Chat text bytes on the wire (500 graphemes are enforced by sync-core).
pub const MAX_CHAT_BYTES: usize = 4096;
pub const MAX_CHAT_GRAPHEMES: usize = 500;
pub const MAX_FILE_NAME_BYTES: usize = 255;
pub const MAX_TITLE_BYTES: usize = 256;
pub const MAX_LANG_BYTES: usize = 16;
pub const MAX_CODEC_BYTES: usize = 32;
pub const MAX_FORMAT_BYTES: usize = 16;
/// Members including the host (configurable down, never up).
pub const MAX_MEMBERS: usize = 12;
pub const MAX_MANIFEST_ENTRIES: usize = 32;
/// Snapshot carries at most this many recent chat messages.
pub const SNAPSHOT_CHAT: usize = 200;
/// Host keeps at most this many chat messages in memory.
pub const HISTORY_CHAT: usize = 2000;
pub const MAX_REASON_BYTES: usize = 200;
/// Sanity ceilings for external files (spec section 1; "No limit" is a host UI option).
pub const DEFAULT_MAX_EXTERNAL_AUDIO: u64 = 1024 * 1024 * 1024;
pub const DEFAULT_MAX_EXTERNAL_SUB: u64 = 20 * 1024 * 1024;

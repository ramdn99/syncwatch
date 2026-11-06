//! Per-member permissions (spec section 6). Track control is host-only and has NO permission.
//! Choosing one's own local file needs NO permission either. Chat is NOT a permission: the
//! host mutes members and switches chat on or off for everyone (see `ChatState`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub can_play_pause: bool,
    pub can_seek: bool,
    pub can_change_media: bool,
    pub can_change_speed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Preset {
    EveryoneControls,
    OnlyMe,
    Custom,
}

impl Permissions {
    pub const ALL: Permissions = Permissions {
        can_play_pause: true,
        can_seek: true,
        can_change_media: true,
        can_change_speed: true,
    };

    /// "Only me": members control nothing (they can still chat unless muted or chat is off).
    pub const NONE: Permissions = Permissions {
        can_play_pause: false,
        can_seek: false,
        can_change_media: false,
        can_change_speed: false,
    };

    pub fn for_preset(p: Preset, custom: Permissions) -> Permissions {
        match p {
            Preset::EveryoneControls => Permissions::ALL,
            Preset::OnlyMe => Permissions::NONE,
            Preset::Custom => custom,
        }
    }
}

impl Default for Permissions {
    /// Default for new joiners: everyone controls playback.
    fn default() -> Self {
        Permissions::ALL
    }
}

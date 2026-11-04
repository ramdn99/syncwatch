//! Per-member permissions (spec section 6). Track control is host-only and has NO permission.
//! Choosing one's own local file needs NO permission either.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub can_play_pause: bool,
    pub can_seek: bool,
    pub can_change_media: bool,
    pub can_change_speed: bool,
    pub can_chat: bool,
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
        can_chat: true,
    };

    /// "Only me": members can only chat (can_chat stays ON by default).
    pub const CHAT_ONLY: Permissions = Permissions {
        can_play_pause: false,
        can_seek: false,
        can_change_media: false,
        can_change_speed: false,
        can_chat: true,
    };

    pub fn for_preset(p: Preset, custom: Permissions) -> Permissions {
        match p {
            Preset::EveryoneControls => Permissions::ALL,
            Preset::OnlyMe => Permissions::CHAT_ONLY,
            Preset::Custom => custom,
        }
    }
}

impl Default for Permissions {
    /// Default for new joiners: everyone controls playback, chat on.
    fn default() -> Self {
        Permissions::ALL
    }
}

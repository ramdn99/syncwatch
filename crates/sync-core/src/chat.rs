//! Host-side chat engine (spec section 9 "Chat").
//!
//! PLAIN TEXT only: text is never interpreted, so markup and script stay inert (the UI renders
//! it as text). Control and bidi-override characters are stripped. History lives in memory only
//! (max 2000, snapshots carry the last 200) and moves with the host role.

use crate::timing::CHAT_CLAIM_TOLERANCE_MS;
use std::collections::{HashMap, HashSet, VecDeque};
use syncwatch_protocol::limits::*;
use syncwatch_protocol::*;
use unicode_segmentation::UnicodeSegmentation;

pub const RATE_MAX: usize = 5;
pub const RATE_WINDOW_US: i64 = 5_000_000;
pub const MAX_NEWLINES: usize = 10;
pub const MAX_NAME_GRAPHEMES: usize = 32;

/// Bidi controls and invisible direction marks (Trojan-source style spoofing).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Strips control (except newline) and bidi characters, limits newlines, trims.
pub fn sanitize_text(text: &str) -> Result<String, ChatRejectReason> {
    if text.len() > MAX_CHAT_BYTES {
        return Err(ChatRejectReason::TooLong);
    }
    let mut newlines = 0;
    let cleaned: String = text
        .chars()
        .filter(|c| !is_bidi_control(*c))
        .filter_map(|c| {
            if c == '\n' {
                newlines += 1;
                Some(if newlines > MAX_NEWLINES { ' ' } else { '\n' })
            } else if c == '\r' || c == '\t' {
                Some(' ')
            } else if c.is_control() {
                None
            } else {
                Some(c)
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err(ChatRejectReason::Empty);
    }
    if trimmed.graphemes(true).count() > MAX_CHAT_GRAPHEMES {
        return Err(ChatRejectReason::TooLong);
    }
    Ok(trimmed.to_owned())
}

/// Display names: same cleaning, single line, at most 32 graphemes; empty -> "Guest".
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    let t: String = cleaned
        .trim()
        .graphemes(true)
        .take(MAX_NAME_GRAPHEMES)
        .collect();
    if t.trim().is_empty() {
        "Guest".to_owned()
    } else {
        t.trim().to_owned()
    }
}

/// Context the host supplies for one submission.
#[derive(Debug, Clone, Copy)]
pub struct Submit<'a> {
    pub sender: MemberId,
    pub display_name: &'a str,
    pub is_host: bool,
    pub can_chat: bool,
    pub mono_us: i64,
    pub wall_ms: i64,
    pub media_epoch: MediaEpoch,
    /// Room position at the host's now (paused position when paused); None without media.
    pub room_pos_ms: Option<MediaMs>,
    pub claimed_ms: Option<MediaMs>,
    pub client_id: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ChatEngine {
    pub config: ChatConfig,
    history: VecDeque<ChatMessage>,
    next_seq: u64,
    muted: HashSet<MemberId>,
    recent: HashMap<MemberId, VecDeque<i64>>,
    last_accepted: HashMap<MemberId, i64>,
}

impl ChatEngine {
    pub fn from_handover(
        history: Vec<ChatMessage>,
        config: ChatConfig,
        next_seq: u64,
        muted: Vec<MemberId>,
    ) -> Self {
        Self {
            config,
            history: history.into_iter().collect(),
            next_seq,
            muted: muted.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn submit(&mut self, s: Submit<'_>, text: &str) -> Result<ChatMessage, ChatRejectReason> {
        if !s.is_host {
            if !self.config.enabled {
                return Err(ChatRejectReason::Disabled);
            }
            if self.muted.contains(&s.sender) {
                return Err(ChatRejectReason::Muted);
            }
            if !s.can_chat {
                return Err(ChatRejectReason::NoPermission);
            }
        }
        let text = sanitize_text(text)?;
        if !s.is_host {
            let q = self.recent.entry(s.sender).or_default();
            while q.front().is_some_and(|t| s.mono_us - t >= RATE_WINDOW_US) {
                q.pop_front();
            }
            if q.len() >= RATE_MAX {
                return Err(ChatRejectReason::RateLimited);
            }
            if self.config.slow_mode_secs > 0
                && let Some(last) = self.last_accepted.get(&s.sender)
                && s.mono_us - last < self.config.slow_mode_secs as i64 * 1_000_000
            {
                return Err(ChatRejectReason::SlowMode);
            }
            q.push_back(s.mono_us);
            self.last_accepted.insert(s.sender, s.mono_us);
        }
        // Timestamp: honor the claim only near the host's room position.
        let media_time_ms = s.room_pos_ms.map(|room| match s.claimed_ms {
            Some(c) if (c - room).abs() <= CHAT_CLAIM_TOLERANCE_MS && c >= 0 => c,
            _ => room,
        });
        let seq = self.next_seq;
        self.next_seq += 1;
        let msg = ChatMessage {
            id: ChatId(seq),
            seq,
            host_wall_ms: s.wall_ms,
            sender: s.sender,
            display_name: s.display_name.to_owned(),
            media_epoch: s.media_epoch,
            media_time_ms,
            text,
            client_id: s.client_id,
        };
        self.history.push_back(msg.clone());
        while self.history.len() > HISTORY_CHAT {
            self.history.pop_front();
        }
        Ok(msg)
    }

    pub fn moderate(&mut self, m: Moderation) -> bool {
        match m {
            Moderation::Delete(id) => {
                let before = self.history.len();
                self.history.retain(|x| x.id != id);
                before != self.history.len()
            }
            Moderation::Clear => {
                self.history.clear();
                true
            }
            Moderation::Mute(who) => self.muted.insert(who),
            Moderation::Unmute(who) => self.muted.remove(&who),
        }
    }

    pub fn is_muted(&self, m: MemberId) -> bool {
        self.muted.contains(&m)
    }

    pub fn muted(&self) -> Vec<MemberId> {
        self.muted.iter().copied().collect()
    }

    pub fn snapshot(&self) -> Vec<ChatMessage> {
        let skip = self.history.len().saturating_sub(SNAPSHOT_CHAT);
        self.history.iter().skip(skip).cloned().collect()
    }

    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.iter().cloned().collect()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize_text("  hi  ").unwrap(), "hi");
        assert_eq!(sanitize_text("   \n\t "), Err(ChatRejectReason::Empty));
        assert_eq!(sanitize_text("a\u{202E}gnp.exe").unwrap(), "agnp.exe");
        assert_eq!(sanitize_text("a\u{0007}b\u{0000}c").unwrap(), "abc");
        assert_eq!(
            sanitize_text("<script>alert(1)</script>").unwrap(),
            "<script>alert(1)</script>"
        );
        let many = "x\n".repeat(20);
        assert_eq!(
            sanitize_text(&many).unwrap().matches('\n').count(),
            MAX_NEWLINES
        );
    }

    #[test]
    fn graphemes_not_bytes() {
        // A family emoji is ~25 bytes but one grapheme.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}";
        assert_eq!(
            sanitize_text(&family.repeat(100)).map(|s| s.graphemes(true).count()),
            Ok(100)
        );
        assert!(sanitize_text(&"é".repeat(500)).is_ok());
        assert_eq!(
            sanitize_text(&"e".repeat(501)),
            Err(ChatRejectReason::TooLong)
        );
    }

    #[test]
    fn names() {
        assert_eq!(sanitize_name("  \u{202E}Bob\u{0007} "), "Bob");
        assert_eq!(sanitize_name(""), "Guest");
        assert_eq!(sanitize_name(&"n".repeat(40)).len(), 32);
    }
}

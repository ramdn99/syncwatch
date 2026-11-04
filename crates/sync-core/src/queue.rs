//! Outbound priority queue used by the transport (spec section 8): control, chat and
//! heartbeats always go before file chunks. There is NO rate limiter: bulk data uses the whole
//! link whenever nothing more urgent is waiting.

use std::collections::VecDeque;
use syncwatch_protocol::{Message, Priority};

#[derive(Debug, Default)]
pub struct OutboundQueue {
    queues: [VecDeque<Message>; 4],
}

impl OutboundQueue {
    pub fn push(&mut self, m: Message) {
        self.queues[m.priority() as usize].push_back(m);
    }

    /// Next message to put on the wire: highest priority first, FIFO within a class.
    pub fn pop(&mut self) -> Option<Message> {
        self.queues.iter_mut().find_map(VecDeque::pop_front)
    }

    pub fn len(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bulk messages waiting (the uploader refills below a low watermark).
    pub fn bulk_len(&self) -> usize {
        self.queues[Priority::Bulk as usize].len()
    }
}

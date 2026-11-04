#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)] // integration-test helpers

use syncwatch_protocol::*;
use syncwatch_sync_core::sim::{NodeConfig, World};
use syncwatch_sync_core::*;

pub const HOST: usize = 0;

/// Host + `n` members that all hold the file; the host opened it and everyone loaded it.
pub fn room(n: usize, seed: u64) -> (World, Vec<usize>) {
    room_with(n, seed, |_, _| {})
}

pub fn room_with(
    n: usize,
    seed: u64,
    tweak: impl Fn(usize, &mut NodeConfig),
) -> (World, Vec<usize>) {
    let mut host = NodeConfig::named("Host");
    host.file = Some(World::sample_identity());
    let mut w = World::new(seed, host);
    let mut ids = Vec::new();
    for i in 0..n {
        let mut c = NodeConfig::named(&format!("M{i}"));
        c.file = Some(World::sample_identity());
        tweak(i, &mut c);
        ids.push(w.add_member(c));
    }
    w.run_for(300_000);
    w.ui(
        HOST,
        UiAction::ChangeMedia {
            descriptor: World::local_descriptor(),
            own_file: true,
        },
    );
    w.run_for(2_000_000);
    (w, ids)
}

pub fn host(w: &World) -> &HostMachine {
    match &w.nodes[w.host_idx()].peer {
        Peer::Host(h) => h,
        Peer::Member(_) => panic!("node is not host"),
    }
}

pub fn member(w: &World, idx: usize) -> &MemberMachine {
    match &w.nodes[idx].peer {
        Peer::Member(m) => m,
        Peer::Host(_) => panic!("node is host"),
    }
}

pub fn chat_texts(w: &World, idx: usize) -> Vec<String> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::ChatMessage(m) => Some(m.text.clone()),
            _ => None,
        })
        .collect()
}

pub fn chat_msgs(w: &World, idx: usize) -> Vec<ChatMessage> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::ChatMessage(m) => Some(m.clone()),
            _ => None,
        })
        .collect()
}

pub fn rejections(w: &World, idx: usize) -> Vec<ChatRejectReason> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::ChatRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect()
}

pub fn notices(w: &World, idx: usize) -> Vec<Notice> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::Notice(n) => Some(*n),
            _ => None,
        })
        .collect()
}

pub fn clock(mono_us: i64) -> Clock {
    Clock {
        mono_us,
        wall_ms: 0,
    }
}

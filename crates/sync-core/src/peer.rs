//! A peer is either the host or a member; host transfer swaps the role in place, keeping the
//! local participant (player state, media, tracks) untouched.

use crate::host::{HostConfig, HostMachine};
use crate::member::MemberMachine;
use crate::types::*;
use syncwatch_protocol::*;

#[derive(Debug)]
pub enum Peer {
    Host(Box<HostMachine>),
    Member(Box<MemberMachine>),
}

impl Peer {
    pub fn host(cfg: HostConfig, now: i64) -> Self {
        Peer::Host(Box::new(HostMachine::new(cfg, now)))
    }

    pub fn member(identity: Identity, name: &str) -> Self {
        Peer::Member(Box::new(MemberMachine::new(identity, name)))
    }

    pub fn is_host(&self) -> bool {
        matches!(self, Peer::Host(_))
    }

    pub fn local(&self) -> &crate::local::Local {
        match self {
            Peer::Host(h) => &h.local,
            Peer::Member(m) => &m.local,
        }
    }

    pub fn next_deadline(&self) -> Option<i64> {
        match self {
            Peer::Host(h) => h.next_deadline(),
            Peer::Member(m) => m.next_deadline(),
        }
    }

    pub fn handle(
        &mut self,
        clock: Clock,
        ev: Event,
        identity: Identity,
        name: &str,
    ) -> Vec<Effect> {
        let effects = match self {
            Peer::Host(h) => h.handle(clock, ev),
            Peer::Member(m) => m.handle(clock, ev),
        };
        // Role changes after a handover.
        let next = match self {
            Peer::Host(h) => h.take_handover().map(|hand| {
                let local = h.local.clone();
                Peer::Member(Box::new(MemberMachine::from_former_host(
                    identity, name, local, &hand,
                )))
            }),
            Peer::Member(m) => m.take_handover().map(|mut hand| {
                // The room was anchored in the OLD host's clock: convert to ours.
                hand.playback.anchor_host_us = m
                    .clock
                    .local_for_host(hand.playback.anchor_host_us)
                    .unwrap_or(clock.mono_us);
                let local = m.local.clone();
                let cfg = HostConfig::new(identity, name);
                Peer::Host(Box::new(HostMachine::from_handover(
                    hand,
                    local,
                    cfg,
                    clock.mono_us,
                )))
            }),
        };
        let mut effects = effects;
        if let Some(n) = next {
            *self = n;
            if let Peer::Host(h) = self {
                effects.extend(h.after_takeover(clock.mono_us));
            }
        }
        effects
    }
}

//! `syncwatch-sim`: runs a scripted room in the deterministic simulator and prints a report.
//!
//! Scenario: host + N members (one muted in chat, one without the file), playback starts, a
//! seek, a speed change, a late joiner, chat traffic, and an external subtitle transfer.
//! Exit code 1 if steady-state divergence exceeds the threshold or a protocol violation occurs.

#![allow(clippy::print_stdout)]

use clap::Parser;
use syncwatch_protocol::*;
use syncwatch_sync_core::sim::{LinkConfig, NodeConfig, World};
use syncwatch_sync_core::{UiAction, UiEvent};

#[derive(Parser, Debug)]
#[command(about = "Simulate a SyncWatch room")]
struct Args {
    /// Members besides the host (at least 3 for the full scenario).
    #[arg(long, default_value_t = 4)]
    members: usize,
    /// One-way latency per member link (ms).
    #[arg(long, default_value_t = 60)]
    latency_ms: i64,
    #[arg(long, default_value_t = 20)]
    jitter_ms: i64,
    /// Loss on the unreliable channel (heartbeats, clock pings), 0..1.
    #[arg(long, default_value_t = 0.05)]
    loss: f64,
    /// Max local clock offset per member (s); members get spread offsets.
    #[arg(long, default_value_t = 30)]
    skew_s: i64,
    /// Player rate drift per member (ppm, spread +-).
    #[arg(long, default_value_t = 300.0)]
    player_drift_ppm: f64,
    #[arg(long, default_value_t = 7)]
    seed: u64,
    /// Simulated duration (s).
    #[arg(long, default_value_t = 120)]
    duration_s: i64,
    /// Fail if steady-state divergence exceeds this (ms).
    #[arg(long, default_value_t = 200.0)]
    max_divergence_ms: f64,
}

fn main() {
    let a = Args::parse();
    let members = a.members.clamp(3, limits::MAX_MEMBERS - 2);
    let mut host = NodeConfig::named("Host");
    host.file = Some(World::sample_identity());
    let mut w = World::new(a.seed, host);

    let mut idx = Vec::new();
    for i in 0..members {
        let mut c = NodeConfig::named(&format!("Member{}", i + 1));
        let spread = if members > 1 {
            (i as f64 / (members - 1) as f64) * 2.0 - 1.0
        } else {
            0.0
        };
        c.clock_offset_us = (spread * a.skew_s as f64 * 1e6) as i64;
        c.clock_drift_ppm = spread * 50.0;
        c.player_drift_ppm = -spread * a.player_drift_ppm;
        c.link = LinkConfig {
            latency_us: a.latency_ms * 1000,
            jitter_us: a.jitter_ms * 1000,
            loss: a.loss,
        };
        // Member 2 is missing the file.
        c.file = if i == 1 {
            None
        } else {
            Some(World::sample_identity())
        };
        idx.push(w.add_member(c));
    }
    let h = w.host_idx();
    w.run_for(500_000);
    w.ui(
        h,
        UiAction::ChangeMedia {
            descriptor: World::local_descriptor(),
            own_file: true,
        },
    );
    // Member 1 is muted in chat by the host.
    let muted = w.nodes[idx[0]].id;
    w.ui(h, UiAction::Moderate(Moderation::Mute(muted)));
    w.run_for(2_000_000);
    w.ui(h, UiAction::Play);
    w.run_for(10_000_000);

    let mut report: Vec<(String, f64)> = Vec::new();
    let mut worst_steady = 0.0f64;
    let total = a.duration_s * 1_000_000;
    let mut t = 0;
    let mut seeked = false;
    let mut sped = false;
    let mut late: Option<usize> = None;
    while t < total {
        w.run_for(1_000_000);
        t += 1_000_000;
        let d = w.max_divergence_ms();
        let s = t / 1_000_000;
        // Steady state: ignore the 6 s after each disruption.
        let disrupted = (seeked && (30..36).contains(&s))
            || (sped && (50..56).contains(&s))
            || (late.is_some() && (70..80).contains(&s));
        if !disrupted {
            worst_steady = worst_steady.max(d);
        }
        if s % 10 == 0 {
            report.push((format!("t={s:>4}s"), d));
        }
        match s {
            30 => {
                w.ui(idx[2], UiAction::Seek(600_000));
                seeked = true;
            }
            40 => w.ui(
                idx[0],
                UiAction::SendChat {
                    client_id: 1,
                    text: "muted member says hi".into(),
                },
            ),
            41 => w.ui(
                idx[2],
                UiAction::SendChat {
                    client_id: 1,
                    text: "great scene".into(),
                },
            ),
            50 => {
                w.ui(h, UiAction::SetSpeed(1250));
                sped = true;
            }
            60 => {
                w.ui(h, UiAction::SetSpeed(1000));
                let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
                w.host_add_external(ExternalKind::Subtitle, "extra.ass", "ass", bytes);
            }
            70 => {
                let mut c = NodeConfig::named("LateJoiner");
                c.file = Some(World::sample_identity());
                c.clock_offset_us = -7_000_000;
                c.link.latency_us = a.latency_ms * 1000;
                late = Some(w.add_member(c));
            }
            _ => {}
        }
    }

    println!(
        "SyncWatch simulation: host + {members} members (+1 late joiner), seed {}",
        a.seed
    );
    println!(
        "latency {} ms +-{} ms, loss {:.0}% (unreliable), clock skew +-{} s, player drift +-{} ppm",
        a.latency_ms,
        a.jitter_ms,
        a.loss * 100.0,
        a.skew_s,
        a.player_drift_ppm
    );
    println!();
    println!("{:<10} {:>18}", "time", "max divergence");
    for (label, d) in &report {
        println!("{label:<10} {d:>15.1} ms");
    }
    println!();
    let mut violations = 0;
    for n in &w.nodes {
        let chat = n
            .ui_events()
            .filter(|u| matches!(u, UiEvent::ChatMessage(_)))
            .count();
        let status = n.peer.local().media;
        println!(
            "{:<11} id={:<3} loaded={:<5} pos={:>9.0} ms  media={:?}  chat-msgs={}  externals={}  violations={}",
            n.cfg.name,
            n.id.0,
            n.player.loaded,
            n.player.pos_ms,
            status,
            chat,
            n.ext_complete.len(),
            n.violations.len()
        );
        for v in &n.violations {
            println!("            violation: {v}");
        }
        violations += n.violations.len();
    }
    println!();
    println!("traffic: {:?}", w.traffic);
    println!(
        "worst steady-state divergence: {worst_steady:.1} ms (limit {:.0} ms)",
        a.max_divergence_ms
    );
    if let Some(l) = late {
        println!(
            "late joiner position: {:.0} ms vs host {:.0} ms",
            w.pos(l),
            w.pos(w.host_idx())
        );
    }
    if worst_steady > a.max_divergence_ms || violations > 0 {
        println!("RESULT: FAIL");
        std::process::exit(1);
    }
    println!("RESULT: PASS");
}

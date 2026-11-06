#![allow(clippy::unwrap_used, clippy::expect_used)] // integration-test helpers
//! Chat rules (spec section 9 and 12).

mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::*;

fn say(w: &mut sim::World, idx: usize, cid: u32, text: &str) {
    w.ui(
        idx,
        UiAction::SendChat {
            client_id: cid,
            text: text.into(),
        },
    );
}

use syncwatch_sync_core::sim;

fn mute(w: &mut syncwatch_sync_core::sim::World, idx: usize, muted: bool) {
    let m = w.nodes[idx].id;
    w.ui(
        HOST,
        UiAction::Moderate(if muted {
            Moderation::Mute(m)
        } else {
            Moderation::Unmute(m)
        }),
    );
    w.run_for(300_000);
}

fn chat_on(w: &mut syncwatch_sync_core::sim::World, enabled: bool) {
    w.ui(
        HOST,
        UiAction::SetChatConfig(ChatConfig {
            enabled,
            ..ChatConfig::default()
        }),
    );
    w.run_for(300_000);
}

#[test]
fn muted_member_is_rejected_others_are_not_and_still_see_everything() {
    let (mut w, ids) = room(2, 41);
    mute(&mut w, ids[0], true);
    assert!(
        member(&w, ids[0]).chat_state.muted,
        "the muted member is told at once"
    );
    assert!(!member(&w, ids[1]).chat_state.muted);
    say(&mut w, ids[0], 1, "muted talk");
    say(&mut w, ids[1], 1, "free talk");
    say(&mut w, HOST, 1, "host talk");
    w.run_for(500_000);
    assert_eq!(rejections(&w, ids[0]), vec![ChatRejectReason::Muted]);
    assert!(rejections(&w, ids[1]).is_empty());
    for n in [HOST, ids[0], ids[1]] {
        let texts = chat_texts(&w, n);
        assert!(!texts.contains(&"muted talk".to_owned()), "not relayed");
        assert!(
            texts.contains(&"free talk".to_owned()),
            "the muted member sees chat too"
        );
        assert!(texts.contains(&"host talk".to_owned()));
    }
    // Unmuting takes effect immediately.
    mute(&mut w, ids[0], false);
    assert!(!member(&w, ids[0]).chat_state.muted);
    say(&mut w, ids[0], 2, "back");
    w.run_for(500_000);
    assert!(chat_texts(&w, ids[1]).contains(&"back".to_owned()));
}

#[test]
fn chat_off_rejects_every_member_but_the_host_and_everyone_still_receives() {
    let (mut w, ids) = room(2, 43);
    mute(&mut w, ids[1], true);
    chat_on(&mut w, false);
    for &m in &ids {
        assert!(
            !member(&w, m).chat_state.config.enabled,
            "every member learns chat is off"
        );
    }
    say(&mut w, ids[0], 1, "while off");
    say(&mut w, HOST, 1, "host while off");
    w.run_for(500_000);
    assert_eq!(rejections(&w, ids[0]), vec![ChatRejectReason::ChatDisabled]);
    for n in [HOST, ids[0], ids[1]] {
        assert!(!chat_texts(&w, n).contains(&"while off".to_owned()));
        assert!(
            chat_texts(&w, n).contains(&"host while off".to_owned()),
            "the host still reaches everyone"
        );
    }
    // On again: everyone except the muted member can chat.
    chat_on(&mut w, true);
    say(&mut w, ids[0], 2, "on again");
    say(&mut w, ids[1], 2, "still muted");
    w.run_for(500_000);
    assert!(chat_texts(&w, HOST).contains(&"on again".to_owned()));
    assert!(!chat_texts(&w, HOST).contains(&"still muted".to_owned()));
    assert_eq!(rejections(&w, ids[1]), vec![ChatRejectReason::Muted]);
}

#[test]
fn mute_survives_a_reconnect() {
    let (mut w, ids) = room(1, 44);
    let m = ids[0];
    mute(&mut w, m, true);
    w.disconnect(m);
    w.run_for(2_000_000);
    w.nodes[m].connected = true;
    let (id, identity) = (w.nodes[m].id, w.nodes[m].identity);
    let h = w.host_idx();
    w.deliver_event(
        h,
        Event::PeerJoined {
            member: id,
            identity,
            display_name: "again".into(),
        },
    );
    w.run_for(500_000);
    assert!(
        member(&w, m).chat_state.muted,
        "the reconnect snapshot says muted"
    );
    say(&mut w, m, 9, "after reconnect");
    w.run_for(500_000);
    assert!(rejections(&w, m).contains(&ChatRejectReason::Muted));
}

#[test]
fn mute_survives_a_host_transfer() {
    let (mut w, ids) = room(2, 45);
    mute(&mut w, ids[1], true);
    w.ui(HOST, UiAction::MakeHost(w.nodes[ids[0]].id));
    w.run_for(2_000_000);
    assert!(w.nodes[ids[0]].peer.is_host());
    say(&mut w, ids[1], 3, "after transfer");
    w.run_for(500_000);
    assert!(
        !chat_texts(&w, ids[0]).contains(&"after transfer".to_owned()),
        "the new host enforces the mute"
    );
    assert!(rejections(&w, ids[1]).contains(&ChatRejectReason::Muted));
}

#[test]
fn a_late_joiner_gets_the_chat_flag_and_its_own_mute() {
    let (mut w, _ids) = room(1, 46);
    chat_on(&mut w, false);
    let late = w.add_member(syncwatch_sync_core::sim::NodeConfig::named("Late"));
    w.run_for(500_000);
    assert!(
        !member(&w, late).chat_state.config.enabled,
        "the snapshot carries chat off"
    );
    assert!(!member(&w, late).chat_state.muted);
}

#[test]
fn every_change_and_every_snapshot_carries_the_chat_state() {
    let (mut w, ids) = room(2, 47);
    let base = w.nodes[HOST].sent.len();
    chat_on(&mut w, false);
    let sent: Vec<Message> = w.nodes[HOST].sent[base..]
        .iter()
        .map(|(_, m)| m.clone())
        .collect();
    let states = sent
        .iter()
        .filter(|m| matches!(m, Message::Command(Command::ChatState(s)) if !s.config.enabled))
        .count();
    assert_eq!(
        states,
        ids.len(),
        "one ChatState per member on a global change"
    );
    let base = w.nodes[HOST].sent.len();
    mute(&mut w, ids[0], true);
    let sent: Vec<Message> = w.nodes[HOST].sent[base..]
        .iter()
        .map(|(_, m)| m.clone())
        .collect();
    assert_eq!(
        sent.iter()
            .filter(|m| matches!(m, Message::Command(Command::ChatState(s)) if s.muted))
            .count(),
        1,
        "the muted member gets its state"
    );
    let welcome = w.nodes[HOST].sent.iter().rev().find_map(|(_, m)| match m {
        Message::Welcome(s) => Some(s.chat_state),
        _ => None,
    });
    assert!(welcome.is_some(), "snapshots carry chat_state");
}

#[test]
fn a_rejection_corrects_the_member_state() {
    let (mut w, ids) = room(1, 48);
    let m = ids[0];
    mute(&mut w, m, true);
    // Simulate a member whose state was lost (stale UI): the rejection fixes it.
    if let syncwatch_sync_core::Peer::Member(mm) = &mut w.nodes[m].peer {
        mm.chat_state.muted = false;
    }
    say(&mut w, m, 5, "stale");
    w.run_for(500_000);
    assert!(member(&w, m).chat_state.muted);
    assert!(
        w.nodes[m]
            .ui_events()
            .any(|u| matches!(u, UiEvent::ChatState(s) if s.muted))
    );
}
#[test]
fn ordering_and_echo_to_the_sender() {
    let (mut w, ids) = room(3, 42);
    for (i, &node) in ids.iter().enumerate() {
        say(&mut w, node, 10 + i as u32, &format!("msg {i}"));
        say(&mut w, HOST, 20 + i as u32, &format!("host {i}"));
    }
    w.run_for(1_000_000);
    let reference: Vec<(u64, String)> = chat_msgs(&w, HOST)
        .iter()
        .map(|m| (m.seq, m.text.clone()))
        .collect();
    assert_eq!(reference.len(), 6);
    assert!(
        reference.windows(2).all(|p| p[0].0 < p[1].0),
        "host stamps increasing seq"
    );
    for i in &ids {
        let got: Vec<(u64, String)> = chat_msgs(&w, *i)
            .iter()
            .map(|m| (m.seq, m.text.clone()))
            .collect();
        assert_eq!(got, reference, "same order everywhere");
    }
    // The sender receives its own message back with its client id (reconciles the optimistic copy).
    let echo = chat_msgs(&w, ids[1])
        .into_iter()
        .find(|m| m.sender == w.nodes[ids[1]].id)
        .unwrap();
    assert_eq!(echo.client_id, 11);
}

#[test]
fn timestamp_forgery_is_replaced_by_the_host_position() {
    let (mut w, ids) = room(1, 43);
    w.ui(HOST, UiAction::Seek(600_000));
    w.run_for(1_000_000);
    // Craft ChatSend messages directly (a modified client).
    let from = Sender {
        member: w.nodes[ids[0]].id,
        identity: w.nodes[ids[0]].identity,
    };
    for (cid, claim) in [
        (1, Some(602_000)),
        (2, Some(9_999_000)),
        (3, Some(-50)),
        (4, None),
    ] {
        w.deliver_event(
            HOST,
            Event::Net {
                from,
                msg: Message::ChatSend {
                    client_id: cid,
                    text: format!("c{cid}"),
                    claimed_media_ms: claim,
                },
            },
        );
    }
    let msgs = chat_msgs(&w, HOST);
    let t = |cid: u32| {
        msgs.iter()
            .find(|m| m.client_id == cid)
            .unwrap()
            .media_time_ms
    };
    assert_eq!(t(1), Some(602_000), "honored within 5 s");
    assert_eq!(
        t(2),
        Some(600_000),
        "far claim replaced by the (paused) room position"
    );
    assert_eq!(t(3), Some(600_000), "negative claim replaced");
    assert_eq!(t(4), Some(600_000));
}

#[test]
fn no_timestamp_without_media() {
    let mut host_cfg = sim::NodeConfig::named("Host");
    host_cfg.file = Some(sim::World::sample_identity());
    let mut w = sim::World::new(44, host_cfg);
    let m = w.add_member(sim::NodeConfig::named("M"));
    w.run_for(500_000);
    say(&mut w, m, 1, "hello");
    w.run_for(500_000);
    assert_eq!(chat_msgs(&w, m)[0].media_time_ms, None);
}

#[test]
fn rate_limit_and_slow_mode() {
    let (mut w, ids) = room(1, 45);
    for i in 0..7 {
        say(&mut w, ids[0], i, &format!("spam {i}"));
    }
    w.run_for(1_000_000);
    let delivered = chat_texts(&w, HOST).len();
    assert_eq!(delivered, 5, "5 messages per 5 s");
    assert_eq!(
        rejections(&w, ids[0])
            .iter()
            .filter(|r| **r == ChatRejectReason::RateLimited)
            .count(),
        2
    );
    w.run_for(5_000_000);
    say(&mut w, ids[0], 50, "after window");
    w.run_for(500_000);
    assert_eq!(chat_texts(&w, HOST).len(), 6);
    // Slow mode: one message per 10 s.
    w.ui(
        HOST,
        UiAction::SetChatConfig(ChatConfig {
            slow_mode_secs: 10,
            ..ChatConfig::default()
        }),
    );
    w.run_for(11_000_000);
    say(&mut w, ids[0], 60, "slow 1");
    say(&mut w, ids[0], 61, "slow 2");
    w.run_for(500_000);
    assert!(rejections(&w, ids[0]).contains(&ChatRejectReason::SlowMode));
    // The host is never rate limited.
    for i in 0..8 {
        say(&mut w, HOST, 100 + i, "host talks a lot");
    }
    assert!(rejections(&w, HOST).is_empty());
}

#[test]
fn unicode_limits_and_injection_stay_inert() {
    let (mut w, ids) = room(1, 46);
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    say(&mut w, ids[0], 1, &family.repeat(150)); // 150 graphemes, ~2700 bytes
    say(&mut w, ids[0], 2, &"x".repeat(501));
    say(
        &mut w,
        ids[0],
        3,
        "<img src=x onerror=alert(1)><script>alert(2)</script>",
    );
    say(&mut w, ids[0], 4, "evil\u{202E}txt.exe\u{0000}\u{0007}");
    say(&mut w, ids[0], 5, " \n\t ");
    w.run_for(1_000_000);
    let texts = chat_texts(&w, HOST);
    assert!(texts.contains(&family.repeat(150)), "graphemes, not bytes");
    assert!(
        texts.contains(&"<img src=x onerror=alert(1)><script>alert(2)</script>".to_owned()),
        "stored as plain text, untouched"
    );
    assert!(
        texts.contains(&"eviltxt.exe".to_owned()),
        "bidi and control characters stripped"
    );
    let rej = rejections(&w, ids[0]);
    assert!(rej.contains(&ChatRejectReason::TooLong) && rej.contains(&ChatRejectReason::Empty));
    // Display names are sanitized at join.
    let mut c = sim::NodeConfig::named("\u{202E}Mallory\u{0007}");
    c.file = Some(sim::World::sample_identity());
    let m = w.add_member(c);
    w.run_for(1_000_000);
    say(&mut w, m, 1, "hi");
    w.run_for(500_000);
    assert_eq!(chat_msgs(&w, HOST).last().unwrap().display_name, "Mallory");
}

#[test]
fn delete_clear_mute_and_moderation_only_from_host() {
    let (mut w, ids) = room(2, 47);
    say(&mut w, ids[0], 1, "keep");
    say(&mut w, ids[0], 2, "delete me");
    w.run_for(500_000);
    let del = chat_msgs(&w, HOST)
        .into_iter()
        .find(|m| m.text == "delete me")
        .unwrap()
        .id;
    w.ui(HOST, UiAction::Moderate(Moderation::Delete(del)));
    w.run_for(300_000);
    assert!(
        w.nodes[ids[1]]
            .ui_events()
            .any(|u| *u == UiEvent::ChatDeleted(del))
    );
    assert!(!host(&w).chat_history().iter().any(|m| m.id == del));
    // Mute.
    w.ui(
        HOST,
        UiAction::Moderate(Moderation::Mute(w.nodes[ids[0]].id)),
    );
    w.run_for(300_000);
    say(&mut w, ids[0], 3, "muted");
    w.run_for(500_000);
    assert!(rejections(&w, ids[0]).contains(&ChatRejectReason::Muted));
    assert!(
        member(&w, ids[1])
            .members
            .iter()
            .any(|m| m.id == w.nodes[ids[0]].id && m.chat_muted)
    );
    w.ui(
        HOST,
        UiAction::Moderate(Moderation::Unmute(w.nodes[ids[0]].id)),
    );
    // Clear.
    w.ui(HOST, UiAction::Moderate(Moderation::Clear));
    w.run_for(300_000);
    assert!(
        w.nodes[ids[1]]
            .ui_events()
            .any(|u| *u == UiEvent::ChatCleared)
    );
    assert!(host(&w).chat_history().is_empty());
    // A member trying to moderate: client refuses, crafted message rejected by the host.
    w.ui(ids[1], UiAction::Moderate(Moderation::Clear));
    assert!(notices(&w, ids[1]).contains(&Notice::RequestDenied {
        reason: DenyReason::NoPermission
    }));
}

#[test]
fn local_mute_hides_a_member_only_locally() {
    let (mut w, ids) = room(2, 48);
    w.ui(
        ids[1],
        UiAction::LocalMute {
            member: w.nodes[ids[0]].id,
            muted: true,
        },
    );
    say(&mut w, ids[0], 1, "you cannot see this locally");
    w.run_for(500_000);
    assert!(!chat_texts(&w, ids[1]).contains(&"you cannot see this locally".to_owned()));
    assert!(chat_texts(&w, HOST).contains(&"you cannot see this locally".to_owned()));
}

#[test]
fn chat_history_survives_host_transfer() {
    let (mut w, ids) = room(2, 49);
    for i in 0..4 {
        say(&mut w, ids[i % 2], i as u32, &format!("before {i}"));
    }
    w.run_for(1_000_000);
    let banned_identity = {
        let mut c = sim::NodeConfig::named("Troll");
        c.file = Some(sim::World::sample_identity());
        let t = w.add_member(c);
        w.run_for(500_000);
        let id = w.nodes[t].identity;
        w.ui(HOST, UiAction::Ban(w.nodes[t].id));
        w.run_for(300_000);
        id
    };
    let old_history = host(&w).chat_history();
    w.ui(HOST, UiAction::MakeHost(w.nodes[ids[0]].id));
    w.run_for(2_000_000);
    assert_eq!(w.host_idx(), ids[0], "member 0 is the new host");
    let h = host(&w);
    assert_eq!(
        h.chat_history(),
        old_history,
        "history moved with the host role"
    );
    assert!(h.bans().contains(&banned_identity), "ban list moved too");
    // New messages continue the sequence and reach everyone including the old host.
    say(&mut w, ids[1], 99, "after transfer");
    w.run_for(1_000_000);
    let last = chat_msgs(&w, HOST).into_iter().last().unwrap();
    assert_eq!(last.text, "after transfer");
    assert!(last.seq > old_history.last().unwrap().seq);
}

//! Host controls come from the host's own state (Milestone 9 finding: the preset, "Wait for
//! everyone" and the delays were kept in the UI and showed defaults after collapsing the
//! section). Each value must be reported by the host, move with the host role (transfer and
//! crash succession) and reach late joiners.
mod common;
use common::*;
use syncwatch_protocol::*;
use syncwatch_sync_core::*;

fn last_settings(w: &sim::World, idx: usize) -> Option<HostSettings> {
    w.nodes[idx]
        .ui_events()
        .filter_map(|u| match u {
            UiEvent::HostSettings(s) => Some(*s),
            _ => None,
        })
        .last()
}

fn change_everything(w: &mut sim::World) {
    w.ui(
        HOST,
        UiAction::ApplyPreset {
            preset: Preset::OnlyMe,
            custom: Permissions::NONE,
        },
    );
    w.ui(HOST, UiAction::SetWaitForEveryone(true));
    w.ui(HOST, UiAction::SetSubDelay(300));
    w.ui(HOST, UiAction::SetAudioDelay(-200));
    w.ui(HOST, UiAction::Lock(true));
    w.run_for(500_000);
}

#[test]
fn the_host_reports_every_control_from_its_own_state() {
    let (mut w, _ids) = room(1, 71);
    let s0 = last_settings(&w, HOST).expect("reported at the start");
    assert_eq!(
        (
            s0.preset,
            s0.wait_for_everyone,
            s0.sub_delay_ms,
            s0.audio_delay_ms,
            s0.locked
        ),
        (Preset::EveryoneControls, false, 0, 0, false)
    );
    change_everything(&mut w);
    let s = last_settings(&w, HOST).unwrap();
    assert_eq!(
        (
            s.preset,
            s.wait_for_everyone,
            s.sub_delay_ms,
            s.audio_delay_ms,
            s.locked
        ),
        (Preset::OnlyMe, true, 300, -200, true)
    );
    assert_eq!(s, host(&w).host_settings(), "the event is the host's state");
    // Nothing changes on its own: no new event without a change.
    let n = w.nodes[HOST]
        .ui_events()
        .filter(|u| matches!(u, UiEvent::HostSettings(_)))
        .count();
    w.run_for(2_000_000);
    assert_eq!(
        n,
        w.nodes[HOST]
            .ui_events()
            .filter(|u| matches!(u, UiEvent::HostSettings(_)))
            .count()
    );
}

#[test]
fn the_settings_move_with_a_host_transfer() {
    let (mut w, ids) = room(2, 72);
    change_everything(&mut w);
    w.ui(HOST, UiAction::MakeHost(w.nodes[ids[0]].id));
    w.run_for(2_000_000);
    assert_eq!(w.host_idx(), ids[0]);
    let s = last_settings(&w, ids[0]).expect("the new host reports them");
    assert_eq!(
        (
            s.preset,
            s.wait_for_everyone,
            s.sub_delay_ms,
            s.audio_delay_ms,
            s.locked
        ),
        (Preset::OnlyMe, true, 300, -200, true)
    );
}

#[test]
fn the_successor_state_carries_wait_for_everyone_for_a_crash() {
    let (mut w, _ids) = room(2, 73);
    w.ui(HOST, UiAction::SetWaitForEveryone(true));
    w.run_for(500_000);
    assert!(host(&w).succession_state().wait_for_everyone);
}

#[test]
fn a_late_joiner_gets_the_delays_and_the_permissions() {
    let (mut w, _ids) = room(1, 74);
    change_everything(&mut w);
    w.ui(HOST, UiAction::Lock(false));
    w.run_for(300_000);
    let late = w.add_member(sim::NodeConfig::named("Late"));
    w.run_for(1_000_000);
    let m = member(&w, late);
    assert_eq!(m.local.tracks.desired.sub_delay_ms, 300);
    assert_eq!(m.local.tracks.desired.audio_delay_ms, -200);
    assert_eq!(m.perms, Permissions::NONE, "the room's default (Only me)");
}

// Debug builds only: ADR 0012 lab scenario ("torrentgate"): ready gate and buffering popup in a
// torrent room. scripts/dev/torrent-lab.ps1 -Scenario torrentgate runs it with two windows and
// qBittorrent as the only, throttled seeder. Real UI code path, real mpv, real engine; no mouse or
// keyboard.
//
// The state is read loosely on purpose: the same file also runs against the build from before
// the fix (where the overlay followed the engine's read frontier and the "Buffering..." pill
// followed mpv's paused-for-cache flag) to reproduce the bug.

import { invoke } from '@tauri-apps/api/core';
import { devJoin } from './devjoin';
import { room, torrents, type RoomState } from '../lib/room';
import { act, check, current, has, sleep, waitFor, type Report } from './scenario';

interface Snap {
  state: { fileLoaded: boolean; paused: boolean; pausedForCache?: boolean; timePos: number | null; duration: number | null };
}
interface Wait {
  atMs: number;
  missing: number;
  permille: number;
}
interface Loose {
  stall?: { stalled: boolean; wait: Wait | null };
  torrent: null | { buffering?: unknown; ranges: [number, number][]; size: number; haveBytes: number; phase: string; completeMessage: string | null };
  gate: { active: boolean; waiting: { member: number; name: string; reason: string; wait?: Wait | null }[] };
  members: { isYou: boolean; name: string; media: string; buffering: boolean; wait?: Wait | null; torrent: { phase: string; permille: number } | null }[];
}
const L = (r: RoomState) => r as unknown as Loose;
const snap = () => invoke<Snap>('snapshot');
let cid = 30_000;
const say = (text: string) => room.action({ type: 'chat', clientId: cid++, text: `zqg ${text}` });
const said = (text: string) => has(`zqg ${text}`);
const posMs = (s: Snap) => Math.round((s.state.timePos ?? -1) * 1000);
const playing = (s: Snap) => s.state.fileLoaded && !s.state.paused;
const pct = (r: RoomState) => {
  const t = L(r).torrent;
  return t && t.size > 0 ? Math.round((100 * t.haveBytes) / t.size) : -1;
};
const DURATION_MS = 3_780_000; // the 63-minute lab video (long.mkv)
const byteOf = (r: RoomState, ms: number) => {
  const t = L(r).torrent;
  return t ? Math.floor((ms / DURATION_MS) * t.size) : 0;
};
const covered = (r: RoomState, b: number) => L(r).torrent?.ranges.some(([a, e]) => a <= b && b < e) ?? false;
const fmtRanges = (r: RoomState) =>
  (L(r).torrent?.ranges ?? []).map(([a, e]) => `${(a / 1048576).toFixed(1)}-${(e / 1048576).toFixed(1)}`).join(' ') || '(none)';

/** This window's buffering popup as the UI shows it. Old build: the torrent overlay (engine
 * read frontier) or the raw mpv "Buffering..." pill. */
function popupShown(s: Snap, r: RoomState): boolean {
  const l = L(r);
  if (l.stall) return l.stall.stalled;
  return (l.torrent?.buffering ?? null) != null || !!s.state.pausedForCache;
}
/** The gate message this window shows ("Waiting for ..."): raw gate state with reasons. */
const gateText = (r: RoomState) =>
  L(r).gate.active ? L(r).gate.waiting.map((w) => `${w.name}: ${w.reason}${w.wait ? ` at ${Math.round(w.wait.atMs / 1000)} s, ${Math.floor(w.wait.permille / 10)}% of the window, needs ${(w.wait.missing / 1e6).toFixed(1)} MB` : ''}`).join(', ') || 'everyone' : '';

interface Watch {
  n: number;
  popup: number;
  gate: number;
  reasons: Set<string>;
  advancing: number;
  popupWhileAdvancing: number;
}
/** Samples this window for `ms` (every 250 ms): popup, gate and whether the position advanced. */
async function watch(ms: number, stopEarly?: () => boolean): Promise<Watch> {
  const w: Watch = { n: 0, popup: 0, gate: 0, reasons: new Set(), advancing: 0, popupWhileAdvancing: 0 };
  let last: number | null = null;
  const end = Date.now() + ms;
  while (Date.now() < end && !stopEarly?.()) {
    const [s, r] = [await snap(), await current()];
    const p = posMs(s);
    const adv = last != null && playing(s) && p > last;
    last = p;
    const pop = popupShown(s, r);
    w.n++;
    if (pop) w.popup++;
    if (adv) w.advancing++;
    if (pop && adv) w.popupWhileAdvancing++;
    if (L(r).gate.active) {
      w.gate++;
      w.reasons.add(gateText(r));
    }
    await sleep(250);
  }
  return w;
}
const watchText = (w: Watch) =>
  `${w.n} samples: popup ${w.popup} (while advancing ${w.popupWhileAdvancing}), gate active ${w.gate}${w.reasons.size ? ` [${[...w.reasons].join(' | ')}]` : ''}, advancing ${w.advancing}`;

async function waitSnap(label: string, pred: (s: Snap) => boolean, timeoutMs = 15_000): Promise<Snap> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label} (pos ${s.state.timePos?.toFixed(2)}, paused ${s.state.paused})`);
    await sleep(100);
  }
}

const FAR_MS = 3_000_000; // 50:00: past the download frontier
const BACK_MS = 60_000;
const LATE_MS = 2_400_000; // 40:00: where the room waits when the member rejoins

/** (a) Playing from a region both windows have, while both are still partial. */
async function phaseA(report: Report, who: string) {
  await check(report, `(a) ${who}: playing from a downloaded region while partial: no "waiting for" message and no buffering popup`, async () => {
    const r0 = await current();
    const s0 = await snap();
    report(`INFO (a) ${who} at ${pct(r0)}% downloaded, position ${posMs(s0)} ms, window there: ${covered(r0, byteOf(r0, posMs(s0)))}`);
    if (pct(r0) >= 100) throw new Error('already complete: the check needs a partial download');
    const w = await watch(20_000);
    report(`INFO (a) ${who}: ${watchText(w)}`);
    if (w.advancing < 40) throw new Error(`playback did not run (${w.advancing} advancing samples)`);
    if (w.popup > 0 || w.gate > 0) throw new Error(`popup ${w.popup}, gate ${w.gate} of ${w.n} samples`);
  });
}

export async function gateHost(report: Report, magnet: string, fileName: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2, 60_000);
  await check(report, 'host sets the multi-file magnet as the room source', async () => {
    const info = await torrents.metadata(magnet);
    const f = info.files.find((x) => x.path.endsWith(fileName));
    if (!f) throw new Error('lab video not in the torrent');
    await room.setTorrentMedia(magnet, f.index, f.path, f.size);
  });
  await check(report, 'both stream while downloading', async () => {
    await waitSnap('host loaded', (s) => s.state.fileLoaded, 120_000);
    await waitFor('member streams', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 180_000);
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 30_000);
  });
  await sleep(8000);
  await say('A-start');
  await phaseA(report, 'host');
  await waitFor('member A done', said('A-done'), 60_000);

  // (b) Host seeks where the member has nothing yet.
  await check(report, '(b) host: seek to a region the member does not have; the room waits for it, then resumes in sync', async () => {
    const r = await current();
    report(`INFO (b) host: far target ${FAR_MS} ms (byte ~${byteOf(r, FAR_MS)}), host has it: ${covered(r, byteOf(r, FAR_MS))}`);
    await say('B-seek');
    report(`TS b_seek ${Date.now()}`);
    await act({ type: 'seek', ms: FAR_MS });
    const seen = new Set<string>();
    const end = Date.now() + 240_000;
    for (;;) {
      const [s, rs] = [await snap(), await current()];
      if (L(rs).gate.active) seen.add(gateText(rs));
      if (playing(s) && Math.abs(posMs(s) - FAR_MS) < 30_000) break;
      if (Date.now() > end) throw new Error('did not resume after the far seek');
      await sleep(100);
    }
    report(`TS b_resumed ${Date.now()}`);
    report(`INFO (b) host saw the gate as: ${[...seen].join(' | ') || '(never active)'}`);
    const w = await watch(6000);
    report(`INFO (b) host after resume: ${watchText(w)}`);
    if (w.popupWhileAdvancing > 0) throw new Error('popup while playing');
  });
  await waitFor('member B done', said('B-done'), 60_000);

  // (c) Seek back to a region both have.
  await check(report, '(c) host: seek back to a downloaded region: no buffering wait and no popup', async () => {
    await say('C-seek');
    await act({ type: 'seek', ms: BACK_MS });
    const w = await watch(8000);
    report(`INFO (c) host: ${watchText(w)}`);
    if (w.popup > 0) throw new Error('popup');
    if ([...w.reasons].some((x) => x.includes('buffering'))) throw new Error('waited for buffering');
    // A seek always passes the gate for the time members take to seek (loading); the banner
    // shows only after 600 ms, so a short pass is invisible.
    if (w.gate > 2) throw new Error(`gate active for ${w.gate} samples (> 500 ms)`);
  });
  await waitFor('member C done', said('C-done'), 60_000);

  // (d) The member leaves; the room pauses at 40:00; the member rejoins (late, paused).
  await check(report, '(d) host: room paused at a far position while the member rejoins', async () => {
    await say('D-leave');
    await waitFor('member left', (s) => s.members.length === 1, 60_000);
    await act({ type: 'pause' });
    await act({ type: 'seek', ms: LATE_MS });
    await waitSnap('host at 40:00', (s) => s.state.paused && Math.abs(posMs(s) - LATE_MS) < 2000, 60_000);
    await say('D-rejoin');
    await waitFor('member back and ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 240_000);
    await waitFor('member D ranges', said('D-fetched'), 240_000);
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 120_000);
  });

  // (e) Stop at 100% and the handoff: no popup afterwards.
  await check(report, '(e) host: after 100% and the handoff to the finished file, no popup', async () => {
    await waitFor('own complete', (s) => L(s).torrent?.phase === 'complete', 600_000);
    report(`TS host_complete ${Date.now()}`);
    const w = await watch(10_000);
    report(`INFO (e) host after completion: ${watchText(w)}`);
    if (w.popup > 0) throw new Error('popup after completion');
  });
  await waitFor('member E done', said('E-done'), 600_000);
  report(`TS final ${Date.now()}`);
}

export async function gateMember(report: Report) {
  await check(report, 'member accepts the magnet and streams', async () => {
    await waitFor('confirm magnet', (x) => x.confirmMagnet != null, 120_000);
    await act({ type: 'acceptMagnet' });
    await waitSnap('member loaded', (s) => s.state.fileLoaded, 180_000);
    await waitSnap('member playing', playing, 120_000);
  });
  await waitFor('A start', said('A-start'), 120_000);
  await phaseA(report, 'member');
  await say('A-done');

  await waitFor('B seek', said('B-seek'), 120_000);
  await check(report, '(b) member: the popup shows for the stall only; the room position is fetched first; it resumes in sync and the popup goes', async () => {
    const r0 = await current();
    const target = byteOf(r0, FAR_MS);
    report(`INFO (b) member before: ${pct(r0)}% downloaded, target byte ~${target} covered: ${covered(r0, target)}; ranges MiB ${fmtRanges(r0)}`);
    let popupSeen = false;
    let coveredAt: number | null = null;
    const t0 = Date.now();
    const end = t0 + 240_000;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      if (popupShown(s, r)) popupSeen = true;
      if (coveredAt == null && covered(r, target)) {
        coveredAt = Date.now();
        report(`INFO (b) member: room window arrived after ${coveredAt - t0} ms; ranges MiB ${fmtRanges(r)}`);
      }
      if (playing(s) && Math.abs(posMs(s) - FAR_MS) < 30_000) break;
      if (Date.now() > end) throw new Error('member did not resume');
      await sleep(100);
    }
    report(`TS b_member_resumed ${Date.now()}`);
    report(`INFO (b) member popup seen during the stall: ${popupSeen}`);
    // The popup must be gone once playback runs (within the hide delay).
    await sleep(1000);
    const w = await watch(6000);
    report(`INFO (b) member after resume: ${watchText(w)}`);
    if (coveredAt == null) throw new Error('the room window never arrived');
    if (!popupSeen) throw new Error('no popup during a real stall');
    if (w.popup > 0) throw new Error('popup stayed after playback resumed');
  });
  await say('B-done');

  await waitFor('C seek', said('C-seek'), 120_000);
  await check(report, '(c) member: seek back to a downloaded region: no wait and no popup', async () => {
    const w = await watch(8000);
    report(`INFO (c) member: ${watchText(w)}`);
    if (w.popup > 0) throw new Error('popup');
    if ([...w.reasons].some((x) => x.includes('buffering'))) throw new Error('waited for buffering');
  });
  await say('C-done');

  await waitFor('D leave', said('D-leave'), 120_000);
  await check(report, "(d) member rejoins late while the room is paused at 40:00: the room's part is fetched first", async () => {
    const code = (await current()).roomCode;
    if (!code) throw new Error('no room code');
    await room.leave();
    await waitFor('left', (s) => !s.active, 20_000);
    // Wait until the host is paused at 40:00 (its chat marker arrives after rejoining: poll
    // the room from outside is not possible, so give the host time to pause and seek).
    await sleep(8000);
    await devJoin('Member B', code, null);
    await waitFor('confirm again', (x) => x.confirmMagnet != null, 60_000);
    const t0 = Date.now();
    await act({ type: 'acceptMagnet' });
    let first: string | null = null;
    let at: number | null = null;
    const end = t0 + 240_000;
    for (;;) {
      const r = await current();
      const target = byteOf(r, LATE_MS);
      if (first == null && (L(r).torrent?.ranges.length ?? 0) > 0) first = fmtRanges(r);
      if (covered(r, target)) {
        at = Date.now();
        report(`INFO (d) member: first ranges MiB ${first}; room window (byte ~${target}) there after ${at - t0} ms at ${pct(r)}% overall; ranges MiB ${fmtRanges(r)}`);
        break;
      }
      if (Date.now() > end) throw new Error(`room window never arrived; ranges MiB ${fmtRanges(r)}`);
      await sleep(200);
    }
    // The host's engine on the same PC serves at loopback speed, so overall progress says
    // nothing here (the order is proven with a throttled seeder in the engine test
    // a_member_without_a_player_fetches_the_room_position_first). The room's part must be there
    // quickly, while this player is still waiting (late join, room paused).
    if (at - t0 > 15_000) throw new Error(`room window took ${at - t0} ms`);
  });
  await say('D-fetched');

  await check(report, '(e) member: after 100% and the handoff to the finished file, no popup', async () => {
    await waitFor('own complete', (s) => L(s).torrent?.phase === 'complete', 600_000);
    const w = await watch(10_000);
    report(`INFO (e) member after completion: ${watchText(w)}`);
    if (w.popup > 0) throw new Error('popup after completion');
  });
  await say('E-done');
  await sleep(4000);
}

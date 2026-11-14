// Debug builds only: ADR 0012 (revised) lab scenario ("torrentplay"): the host plays FIRST, the
// member joins a room that is already playing. scripts/dev/torrent-lab.ps1 -Scenario torrentplay
// runs it with two windows and qBittorrent as the only, throttled seeder. Real UI code path,
// real mpv, real engine; no mouse or keyboard. The state is read loosely so the same file also
// runs against the build from before the fix (to reproduce the stuck member).

import { invoke } from '@tauri-apps/api/core';
import { room, torrents, type RoomState } from '../lib/room';
import { act, check, current, has, sleep, waitFor, type Report } from './scenario';

interface Snap {
  state: { fileLoaded: boolean; paused: boolean; pausedForCache?: boolean; timePos: number | null };
}
interface Loose {
  stall?: { stalled: boolean };
  torrent: null | { buffering?: unknown; ranges: [number, number][]; size: number; haveBytes: number; phase: string };
  gate: { active: boolean; waiting: { name: string; reason: string }[] };
}
const L = (r: RoomState) => r as unknown as Loose;
const snap = () => invoke<Snap>('snapshot');
let cid = 40_000;
const say = (text: string) => room.action({ type: 'chat', clientId: cid++, text: `zqp ${text}` });
const said = (text: string) => has(`zqp ${text}`);
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
const rangeAt = (r: RoomState, b: number) => L(r).torrent?.ranges.find(([a, e]) => a <= b && b < e) ?? null;
const MIB = 1048576;
const fmtRanges = (r: RoomState) => (L(r).torrent?.ranges ?? []).map(([a, e]) => `${(a / MIB).toFixed(1)}-${(e / MIB).toFixed(1)}`).join(' ') || '(none)';
function popupShown(s: Snap, r: RoomState): boolean {
  const l = L(r);
  if (l.stall) return l.stall.stalled;
  return (l.torrent?.buffering ?? null) != null || !!s.state.pausedForCache;
}

async function waitSnap(label: string, pred: (s: Snap) => boolean, timeoutMs = 15_000): Promise<Snap> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label} (pos ${s.state.timePos?.toFixed(2)}, paused ${s.state.paused})`);
    await sleep(100);
  }
}

/** Samples for `ms`: POS lines (paired across windows by the lab) and popup / gate counts. */
async function watch(report: Report, tag: string, ms: number) {
  const w = { n: 0, popup: 0, gate: 0, advancing: 0, stuck: 0 };
  let last: number | null = null;
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const [s, r] = [await snap(), await current()];
    const p = posMs(s);
    report(`POS ${tag} ${Date.now()} ${p} ${s.state.paused ? 1 : 0}`);
    if (last != null && playing(s) && p > last) w.advancing++;
    if (!playing(s)) w.stuck++;
    last = p;
    w.n++;
    if (popupShown(s, r)) w.popup++;
    if (L(r).gate.active) w.gate++;
    await sleep(500);
  }
  return w;
}

const FAR_MS = 3_300_000; // 55:00: nobody has it yet
const LATE_MS = 2_400_000; // 40:00
const BACK_MS = 2_410_000; // 40:10, just played by both

export async function playHost(report: Report, magnet: string, fileName: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2, 60_000);
  await check(report, 'host sets the magnet and plays as soon as its own player is open (does not wait for the member)', async () => {
    const info = await torrents.metadata(magnet);
    const f = info.files.find((x) => x.path.endsWith(fileName));
    if (!f) throw new Error('lab video not in the torrent');
    await room.setTorrentMedia(magnet, f.index, f.path, f.size);
    await waitSnap('host loaded', (s) => s.state.fileLoaded, 120_000);
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 30_000);
    report(`TS host_playing ${Date.now()}`);
    await say('playing');
  });
  await check(report, '(a) host: the member plays within seconds; one minute in sync, no stuck wait, no popup', async () => {
    await waitFor('member playing', said('member-playing'), 60_000);
    const w = await watch(report, 'a60', 60_000);
    report(`INFO (a) host: ${JSON.stringify(w)}`);
    if (w.popup > 0 || w.gate > 1 || w.stuck > 0) throw new Error(JSON.stringify(w));
  });
  await waitFor('member a done', said('a-done'), 60_000);
  await check(report, '(b) host: seek to 55:00 (nobody has it): fetched from the new position first; resumes in sync', async () => {
    const r0 = await current();
    const target = byteOf(r0, FAR_MS);
    report(`INFO (b) host before: ${pct(r0)}%, target byte ~${target} there: ${rangeAt(r0, target) != null}; ranges MiB ${fmtRanges(r0)}`);
    await say('b-seek');
    await act({ type: 'seek', ms: FAR_MS });
    const t0 = Date.now();
    let first: [number, number] | null = null;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      const at = rangeAt(r, target);
      if (at && !first) {
        first = at;
        report(`INFO (b) host: target there after ${Date.now() - t0} ms as range MiB ${(at[0] / MIB).toFixed(1)}-${(at[1] / MIB).toFixed(1)}; all ${fmtRanges(r)}`);
      }
      if (playing(s) && Math.abs(posMs(s) - FAR_MS) < 20_000) break;
      if (Date.now() - t0 > 180_000) throw new Error('no resume');
      await sleep(100);
    }
    report(`INFO (b) host resumed after ${Date.now() - t0} ms`);
    await watch(report, 'b', 6000);
  });
  await waitFor('member b done', said('b-done'), 60_000);
  await check(report, '(c) host: plays at 40:00 while the member rejoins late', async () => {
    await say('c-leave');
    await waitFor('member left', (s) => s.members.length === 1, 60_000);
    await act({ type: 'seek', ms: LATE_MS });
    await waitSnap('host at 40:00', (s) => playing(s) && Math.abs(posMs(s) - LATE_MS) < 5000, 60_000);
    await say('c-rejoin');
    await waitFor('member c done', said('c-done'), 240_000);
  });
  await check(report, '(d) host: seek back to data both have (40:10): instant, no popup', async () => {
    await say('d-seek');
    const t0 = Date.now();
    await act({ type: 'seek', ms: BACK_MS });
    await waitSnap('host back', (s) => playing(s) && Math.abs(posMs(s) - BACK_MS) < 3000, 10_000);
    report(`INFO (d) host playing again after ${Date.now() - t0} ms`);
    const w = await watch(report, 'd', 5000);
    if (w.popup > 0) throw new Error('popup');
    if (Date.now() - t0 > 9000) throw new Error('slow');
  });
  await waitFor('member d done', said('d-done'), 60_000);
  await check(report, '(e) host: 100% and the handoff to the file: no popup', async () => {
    await waitFor('own complete', (s) => L(s).torrent?.phase === 'complete', 600_000);
    const w = await watch(report, 'e', 10_000);
    if (w.popup > 0) throw new Error('popup after completion');
  });
  await waitFor('member e done', said('e-done'), 600_000);
  report(`TS final ${Date.now()}`);
}

export async function playMember(report: Report) {
  await waitFor('host playing', said('playing'), 180_000);
  await check(report, '(a) member: joins a playing room and plays in sync within seconds (no stuck buffering)', async () => {
    await waitFor('confirm magnet', (x) => x.confirmMagnet != null, 60_000);
    const t0 = Date.now();
    await act({ type: 'acceptMagnet' });
    report(`TS member_accept ${t0}`);
    let firstLoad: number | null = null;
    let atLoad = '';
    const end = t0 + 60_000;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      if (s.state.fileLoaded && firstLoad == null) {
        firstLoad = Date.now() - t0;
        atLoad = `${pct(r)}% overall; ranges MiB ${fmtRanges(r)}`;
      }
      if (playing(s)) break;
      if (Date.now() > end) throw new Error(`member never played (loaded: ${firstLoad}; ${pct(r)}%; popup ${popupShown(s, r)}; ranges ${fmtRanges(r)})`);
      await sleep(100);
    }
    report(`INFO (a) member: player open after ${firstLoad} ms (${atLoad}); playing after ${Date.now() - t0} ms`);
    await say('member-playing');
    const w = await watch(report, 'a60', 60_000);
    report(`INFO (a) member: ${JSON.stringify(w)}`);
    if (w.popup > 0 || w.stuck > 2) throw new Error(JSON.stringify(w));
  });
  await say('a-done');
  await waitFor('b seek', said('b-seek'), 120_000);
  await check(report, '(b) member: the new position is fetched first; resumes in sync; popup only for the stall', async () => {
    const r0 = await current();
    const target = byteOf(r0, FAR_MS);
    const before = rangeAt(r0, target);
    report(`INFO (b) member before: ${pct(r0)}%, target there: ${before != null}; ranges MiB ${fmtRanges(r0)}`);
    const t0 = Date.now();
    let first: [number, number] | null = null;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      const at = rangeAt(r, target);
      if (at && !first) {
        first = at;
        report(`INFO (b) member: target there after ${Date.now() - t0} ms as range MiB ${(at[0] / MIB).toFixed(1)}-${(at[1] / MIB).toFixed(1)}; all ${fmtRanges(r)}`);
      }
      if (playing(s) && Math.abs(posMs(s) - FAR_MS) < 20_000) break;
      if (Date.now() - t0 > 180_000) throw new Error('no resume');
      await sleep(100);
    }
    if (!before && first && first[0] < target - 4 * MIB) throw new Error('the target came as part of a run from earlier data, not from the new position');
    await sleep(1000);
    const w = await watch(report, 'b', 5000);
    if (w.popup > 0) throw new Error('popup stayed after resume');
  });
  await say('b-done');
  await waitFor('c leave', said('c-leave'), 120_000);
  await check(report, '(c) member rejoins late at 40:00: starts there, without the file\'s start or end (only container ranges)', async () => {
    const code = (await current()).roomCode;
    if (!code) throw new Error('no room code');
    await room.leave();
    await waitFor('left', (s) => !s.active, 20_000);
    await sleep(6000);
    await room.join('Member B', code, null);
    await waitFor('confirm again', (x) => x.confirmMagnet != null, 60_000);
    const t0 = Date.now();
    await act({ type: 'acceptMagnet' });
    let atLoad: RoomState | null = null;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      if (s.state.fileLoaded && !atLoad) atLoad = r;
      if (playing(s)) break;
      if (Date.now() - t0 > 90_000) throw new Error(`never played; ranges ${fmtRanges(r)}`);
      await sleep(100);
    }
    const r = atLoad ?? (await current());
    const len = L(r).torrent?.size ?? 0;
    const rs = L(r).torrent?.ranges ?? [];
    report(`INFO (c) member: playing after ${Date.now() - t0} ms; when its player opened: ${pct(r)}% overall; ranges MiB ${fmtRanges(r)}`);
    const startBytes = rs.filter(([a]) => a < 4 * MIB).reduce((n, [a, e]) => n + Math.min(e, 4 * MIB) - a, 0);
    const endBytes = rs.filter(([, e]) => e > len - 8 * MIB).reduce((n, [a, e]) => n + e - Math.max(a, len - 8 * MIB), 0);
    report(`INFO (c) member: at open, ${(startBytes / MIB).toFixed(2)} MiB at the start and ${(endBytes / MIB).toFixed(2)} MiB at the end`);
    // librqbit queues the 32 MiB after the player's header read while it is open; a same-PC peer
    // can fill several MiB of that in ~100 ms. A warning with the numbers, not a policy
    // pre-fetch (ADR 0012, limits).
    if (startBytes > 3 * MIB || endBytes > 3 * MIB) {
      report(`WARN (c) member: beyond the container ranges at open: ${(startBytes / MIB).toFixed(2)} MiB at the start, ${(endBytes / MIB).toFixed(2)} MiB at the end`);
    }
    if (rs.length > 0 && !rs.some(([a, e]) => a <= byteOf(r, LATE_MS) && byteOf(r, LATE_MS) < e)) throw new Error('the room part was not there when the player opened');
    await watch(report, 'c', 8000);
  });
  await say('c-done');
  await waitFor('d seek', said('d-seek'), 120_000);
  await check(report, '(d) member: seek back to data it has: instant, no popup', async () => {
    const t0 = Date.now();
    await waitSnap('member back', (s) => playing(s) && Math.abs(posMs(s) - BACK_MS) < 3000, 10_000);
    report(`INFO (d) member playing again after ${Date.now() - t0} ms`);
    const w = await watch(report, 'd', 5000);
    if (w.popup > 0) throw new Error('popup');
  });
  await say('d-done');
  await check(report, '(e) member: 100% and the handoff to the file: no popup', async () => {
    await waitFor('own complete', (s) => L(s).torrent?.phase === 'complete', 600_000);
    const w = await watch(report, 'e', 10_000);
    if (w.popup > 0) throw new Error('popup after completion');
  });
  await say('e-done');
  await sleep(4000);
}

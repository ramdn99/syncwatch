// Debug builds only: Milestone 7 torrent scenario (scripts/dev/torrent-lab.ps1 runs it with
// qBittorrent as the only, throttled, local seeder). Same rules as scenario6.ts: the real UI
// code path, the real mpv player, the real engine; no mouse or keyboard.
//
// Report lines: PASS/FAIL, "TS tag <wallMs>", "POS tag <wallMs> <positionMs> <paused>",
// "INFO text". The lab script adds the outside evidence (qBittorrent's counters, our sockets).

import { invoke } from '@tauri-apps/api/core';
import { room, torrents, type RoomState } from '../lib/room';
import { act, check, current, has, sleep, waitFor, type Report } from './scenario';

interface Snap {
  state: { fileLoaded: boolean; paused: boolean; timePos: number | null; duration: number | null };
}
const snap = () => invoke<Snap>('snapshot');
let cid = 20_000;
const say = (text: string) => room.action({ type: 'chat', clientId: cid++, text: `zqx ${text}` });
const said = (text: string) => has(`zqx ${text}`);
const posMs = (s: Snap) => Math.round((s.state.timePos ?? -1) * 1000);
const playing = (s: Snap) => s.state.fileLoaded && !s.state.paused;

async function waitSnap(label: string, pred: (s: Snap) => boolean, timeoutMs = 15_000): Promise<Snap> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label} (pos ${s.state.timePos?.toFixed(2)}, paused ${s.state.paused})`);
    await sleep(100);
  }
}

async function sample(report: Report, tag: string, n = 8) {
  for (let i = 0; i < n; i++) {
    const s = await snap();
    report(`POS ${tag} ${Date.now()} ${posMs(s)} ${s.state.paused ? 1 : 0}`);
    await sleep(250);
  }
}

const SEEK_FAR_MS = 3_000_000; // 50:00 of the 63-minute lab video: past the download frontier
const SEEK_BACK_MS = 60_000;
const COMPLETE = 'Downloading and sharing have stopped. No further traffic.';

const pct = (s: RoomState) => (s.torrent && s.torrent.size > 0 ? Math.round((100 * s.torrent.haveBytes) / s.torrent.size) : -1);

/** Watches the stream -> file handoff: the player must never unload/reload, and the position
 * must not jump while playing. Returns once complete (+3 s). */
async function watchHandoff(report: Report, who: string, timeoutMs: number) {
  const end = Date.now() + timeoutMs;
  let unloaded = 0;
  let back = 0;
  let last: number | null = null;
  let doneAt: number | null = null;
  let posAtComplete = -1;
  for (;;) {
    const [s, rs] = [await snap(), await current()];
    if (!s.state.fileLoaded) unloaded++;
    const p = posMs(s);
    if (last != null && !s.state.paused && p + 1500 < last) back = Math.max(back, last - p);
    last = p;
    if (doneAt == null && rs.torrent?.completeMessage) {
      doneAt = Date.now();
      posAtComplete = p;
      report(`TS ${who}_complete ${doneAt}`);
    }
    if (doneAt != null && Date.now() - doneAt > 3000) break;
    if (Date.now() > end) throw new Error(`no completion (${pct(rs)}%)`);
    await sleep(200);
  }
  report(`INFO ${who}: position at completion ${posAtComplete} ms, unloaded samples ${unloaded}, largest backward jump ${back} ms`);
  if (unloaded > 0 || back > 0) throw new Error(`handoff reloaded or jumped (unloaded ${unloaded}, back ${back} ms)`);
}

export async function torrentHost(report: Report, magnet: string, fileName: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2, 60_000);
  let chosen: { index: number; path: string; size: number } | null = null;
  await check(report, 'host gets the multi-file torrent file list (metadata)', async () => {
    report(`TS meta_sent ${Date.now()}`);
    const info = await torrents.metadata(magnet);
    report(`TS meta_done ${Date.now()}`);
    report(`INFO files: ${info.files.map((f) => `${f.path}${f.video ? '' : ' (not video)'} ${f.size}`).join(' | ')}`);
    chosen = info.files.find((f) => f.path.endsWith(fileName)) ?? null;
    if (!chosen || info.files.length < 2) throw new Error('expected a multi-file torrent with the lab video');
  });
  if (!chosen) return;
  const file: { index: number; path: string; size: number } = chosen;
  await check(report, 'host sets the torrent as the room source', () => room.setTorrentMedia(magnet, file.index, file.path, file.size));
  await check(report, 'host plays while downloading', async () => {
    await waitSnap('host file loaded', (s) => s.state.fileLoaded, 120_000);
    const rs = await current();
    report(`INFO host loaded at ${pct(rs)}% downloaded`);
    if (pct(rs) >= 100) throw new Error('already complete: not streaming');
  });
  await check(report, 'member streams too', () => waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 180_000));
  await check(report, 'host play, both in sync while downloading', async () => {
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 30_000);
    await sleep(3000);
    await sample(report, 'tplay');
  });
  await check(report, 'seek past the download frontier: the room waits (buffering), then resumes in sync', async () => {
    const rs = await current();
    const b = rs.torrent ? Math.floor((SEEK_FAR_MS / 3_780_000) * rs.torrent.size) : 0;
    const there = rs.torrent?.ranges.some(([a, e]) => a <= b && b < e) ?? false;
    report(`INFO seek target byte ${b} already downloaded: ${there} (${pct(rs)}% done)`);
    report(`TS seek_sent ${Date.now()}`);
    await act({ type: 'seek', ms: SEEK_FAR_MS });
    let held = false;
    const end = Date.now() + 180_000;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      if (!held && (r.gate.active || r.stall.stalled)) {
        held = true;
        report(`TS hold_seen ${Date.now()}`);
        report(`INFO hold: gate ${r.gate.active} (${r.gate.waiting.map((w) => `${w.name}: ${w.reason}`).join(', ')}), own buffering ${r.stall.stalled ? 'yes' : 'no'}`);
      }
      if (playing(s) && Math.abs(posMs(s) - SEEK_FAR_MS) < 20_000 && (held || there)) break;
      if (Date.now() > end) throw new Error('did not resume after the far seek');
      await sleep(100);
    }
    report(`TS resumed ${Date.now()}`);
    if (!held && !there) throw new Error('the room never waited');
    await sleep(2000);
    await sample(report, 'tseek');
  });
  await check(report, 'seek back to downloaded data is instant', async () => {
    const t0 = Date.now();
    report(`TS back_sent ${t0}`);
    await act({ type: 'seek', ms: SEEK_BACK_MS });
    await waitSnap('host back', (s) => playing(s) && Math.abs(posMs(s) - SEEK_BACK_MS) < 3000, 10_000);
    report(`INFO seek back took ${Date.now() - t0} ms in the host window`);
    await sleep(1500);
    await sample(report, 'tback');
  });
  await check(report, 'host: 100% verified, handoff to the file keeps the position (no reload)', () => watchHandoff(report, 'host', 600_000));
  await check(report, 'host shows the completion message', async () => {
    const rs = await current();
    if (rs.torrent?.completeMessage !== COMPLETE) throw new Error(`message: ${rs.torrent?.completeMessage}`);
  });
  await check(report, 'member completed too', () => waitFor('member complete', said('member-complete'), 600_000));
  report(`TS idle_start ${Date.now()}`);
  await say('idle-start');
  await sleep(15_000);
  report(`TS idle_end ${Date.now()}`);
  await say('idle-end');
  await check(report, 'member rejoined and plays the finished file', () => waitFor('member rejoined', said('rejoined'), 120_000));
  await sleep(5000);
  report(`TS final ${Date.now()}`);
}

export async function torrentMember(report: Report, fileName: string) {
  await check(report, 'member is asked to confirm the magnet (name and size shown)', async () => {
    const s = await waitFor('confirm magnet', (x) => x.confirmMagnet != null, 120_000);
    report(`INFO confirm: ${s.confirmMagnet?.name} ${s.confirmMagnet?.size}`);
    if (!s.confirmMagnet?.name.endsWith(fileName) || !(s.confirmMagnet.size > 0)) throw new Error('wrong magnet details');
    await act({ type: 'acceptMagnet' });
  });
  await check(report, 'member plays while downloading', async () => {
    await waitSnap('member file loaded', (s) => s.state.fileLoaded, 180_000);
    report(`INFO member loaded at ${pct(await current())}% downloaded`);
  });
  await check(report, 'member follows play', async () => {
    await waitSnap('member playing', playing, 60_000);
    await sleep(3000);
    await sample(report, 'tplay');
  });
  await check(report, 'member resumes in sync after the far seek', async () => {
    let buf = false;
    const end = Date.now() + 200_000;
    for (;;) {
      const [s, r] = [await snap(), await current()];
      if (r.stall.stalled) buf = true;
      if (playing(s) && Math.abs(posMs(s) - SEEK_FAR_MS) < 20_000) break;
      if (Date.now() > end) throw new Error('member did not resume');
      await sleep(100);
    }
    report(`TS member_resumed ${Date.now()}`);
    report(`INFO member buffering seen: ${buf}`);
    await sleep(2000);
    await sample(report, 'tseek');
  });
  await check(report, 'member seek back', async () => {
    await waitSnap('member back', (s) => playing(s) && Math.abs(posMs(s) - SEEK_BACK_MS) < 3000, 15_000);
    report(`TS member_back ${Date.now()}`);
    await sleep(1500);
    await sample(report, 'tback');
  });
  await check(report, 'member: 100% verified, handoff keeps the position (no reload)', () => watchHandoff(report, 'member', 600_000));
  await say('member-complete');
  await waitFor('idle over', said('idle-end'), 120_000);
  await check(report, 'rejoin plays the finished file without re-joining the swarm', async () => {
    const code = (await current()).roomCode;
    if (!code) throw new Error('no room code');
    await room.leave();
    await waitFor('left', (s) => !s.active, 20_000);
    report(`TS rejoin_start ${Date.now()}`);
    await room.join('Member B', code, null);
    await waitFor('confirm again', (x) => x.confirmMagnet != null, 60_000);
    await act({ type: 'acceptMagnet' });
    await waitFor('complete at once', (s) => s.torrent?.phase === 'complete', 20_000);
    await waitSnap('file loaded again', (s) => s.state.fileLoaded, 30_000);
    report(`TS rejoined ${Date.now()}`);
    await say('rejoined');
  });
  await sleep(6000);
}

// ------------------------------------------------------- .torrent file, decline, late joiner --

/** Every member's status (host view) satisfies `ok`. */
const membersAll = (ok: (m: RoomState['members'][number]) => boolean) => (s: RoomState) => s.members.filter((m) => !m.isYou).every(ok);

export async function torrentFileHost(report: Report, torrentFile: string, magnet: string, local: string) {
  await waitFor('two members joined', (s) => s.phase === 'inRoom' && s.members.length === 3, 90_000);
  let ih = '';
  await check(report, '(b) the host opens a .torrent FILE: name, infohash, size and files at once (no network)', async () => {
    const t0 = Date.now();
    const info = await torrents.inspectFile(torrentFile);
    report(`INFO .torrent inspected in ${Date.now() - t0} ms: ${info.name}, ${info.files.length} file(s), ${info.totalSize} bytes, ${info.trackers} tracker(s)`);
    ih = info.infohash;
    await room.setTorrentFile(info.infohash, info.files[0]!.index);
    report(`TS t2_set ${Date.now()}`);
  });
  await check(report, '(g) a member who declines starts nothing; the host sees "declined"', () =>
    waitFor('a member declined', (s) => s.members.some((m) => !m.isYou && m.media === 'declined'), 60_000),
  );
  await check(report, '(b) host and member play while downloading, in sync', async () => {
    await waitSnap('host loaded', (s) => s.state.fileLoaded, 120_000);
    await waitFor('member B ready', (s) => s.members.some((m) => m.name === 'Member B' && m.media === 'ready'), 120_000);
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 30_000);
    await sleep(3000);
    await sample(report, 'fplay');
  });
  await check(report, "(d) the late joiner starts directly on the host's file", () => waitFor('c rejoined and ready', said('c-started'), 180_000));
  await check(report, '(f) everyone completes the file', async () => {
    await waitFor('own complete', (s) => s.torrent?.phase === 'complete', 400_000);
    await waitFor('members complete', membersAll((m) => m.torrent?.phase === 'complete'), 400_000);
    report(`TS t2_all_complete ${Date.now()}`);
  });
  await sleep(6000);
  await check(report, '(e) switch: .torrent file -> magnet', async () => {
    const info = await torrents.metadata(magnet);
    const f = info.files.find((x) => x.path.endsWith('long.mkv'))!;
    await room.setTorrentMedia(magnet, f.index, f.path, f.size);
    report(`TS t1_set ${Date.now()}`);
    await waitFor(
      'members downloading T1',
      membersAll((m) => m.torrent != null && (m.torrent.phase === 'downloading' || m.torrent.phase === 'streaming') && m.torrent.permille > 0),
      120_000,
    );
    await sleep(5000);
  });
  await check(report, '(e) switch: magnet -> local file', async () => {
    report(`TS t1_off ${Date.now()}`);
    await room.setLocalMedia(local);
    await waitSnap('local loaded', (s) => s.state.fileLoaded, 30_000);
    await sleep(15_000);
  });
  await check(report, '(e) switch: local file -> back to the .torrent file (finished: no swarm)', async () => {
    const info = await torrents.inspectFile(torrentFile);
    if (info.infohash !== ih) throw new Error('different torrent');
    report(`TS back_t2 ${Date.now()}`);
    await room.setTorrentFile(info.infohash, info.files[0]!.index);
    await waitFor('own complete again', (s) => s.torrent?.phase === 'complete', 30_000);
    await waitFor('members complete again', membersAll((m) => m.torrent?.phase === 'complete'), 60_000);
    await waitSnap('loaded again', (s) => s.state.fileLoaded, 30_000);
  });
  await sleep(10_000);
  report(`TS final ${Date.now()}`);
  await say('tf-done');
}

export async function torrentFileMember(report: Report) {
  await check(report, "(b) member confirms the .torrent room and starts the host's file", async () => {
    const s = await waitFor('confirm', (x) => x.confirmMagnet != null, 120_000);
    report(`INFO B confirm: torrent "${s.confirmMagnet?.torrentName}", file ${s.confirmMagnet?.name} ${s.confirmMagnet?.size}`);
    await act({ type: 'acceptMagnet' });
    await waitSnap('B loaded', (x) => x.state.fileLoaded, 180_000);
    report(`INFO B loaded at ${pct(await current())}% downloaded`);
  });
  await check(report, 'B follows play', async () => {
    await waitSnap('B playing', playing, 60_000);
    await sleep(3000);
    await sample(report, 'fplay');
  });
  await check(report, '(e) B accepts the magnet after the switch', async () => {
    await waitFor('confirm T1', (x) => x.confirmMagnet != null && x.confirmMagnet.name === 'long.mkv', 600_000);
    await act({ type: 'acceptMagnet' });
  });
  await check(report, '(e) B accepts the .torrent again (finished file, no swarm)', async () => {
    await waitFor('confirm T2 again', (x) => x.confirmMagnet != null && x.confirmMagnet.name !== 'long.mkv', 600_000);
    await act({ type: 'acceptMagnet' });
    await waitFor('B complete again', (x) => x.torrent?.phase === 'complete', 30_000);
  });
  await waitFor('done', said('tf-done'), 120_000);
}

export async function torrentFileLate(report: Report) {
  await check(report, '(g) member C declines: nothing starts', async () => {
    await waitFor('confirm', (x) => x.confirmMagnet != null, 120_000);
    await act({ type: 'declineMagnet' });
    await sleep(8000);
    const s = await current();
    if (s.torrent != null) throw new Error(`a download started: ${s.torrent.phase}`);
    report(`TS c_declined ${Date.now()}`);
  });
  await check(report, "(d) C rejoins late and starts directly on the host's file", async () => {
    const code = (await current()).roomCode;
    if (!code) throw new Error('no room code');
    await room.leave();
    await waitFor('left', (s) => !s.active, 20_000);
    await room.join('Member C', code, null);
    const s = await waitFor('confirm again', (x) => x.confirmMagnet != null, 60_000);
    report(`INFO C confirm after rejoin: ${s.confirmMagnet?.name} ${s.confirmMagnet?.size}`);
    await act({ type: 'acceptMagnet' });
    report(`TS c_accept ${Date.now()}`);
    let sawSwarmLookup = false;
    const end = Date.now() + 120_000;
    for (;;) {
      const r = await current();
      if (r.torrent?.phase === 'metadata') sawSwarmLookup = true;
      const sn = await snap();
      if (sn.state.fileLoaded) break;
      if (Date.now() > end) throw new Error(`C did not start (${r.torrent?.phase})`);
      await sleep(150);
    }
    report(`TS c_loaded ${Date.now()}`);
    if (sawSwarmLookup) throw new Error('C looked for the file list in the swarm');
    await say('c-started');
  });
  await check(report, 'C accepts the magnet after the switch, then the .torrent again', async () => {
    await waitFor('confirm T1', (x) => x.confirmMagnet != null && x.confirmMagnet.name === 'long.mkv', 600_000);
    await act({ type: 'acceptMagnet' });
    await waitFor('confirm T2 again', (x) => x.confirmMagnet != null && x.confirmMagnet.name !== 'long.mkv', 600_000);
    await act({ type: 'acceptMagnet' });
    await waitFor('C complete again', (x) => x.torrent?.phase === 'complete', 30_000);
  });
  await waitFor('done', said('tf-done'), 120_000);
}

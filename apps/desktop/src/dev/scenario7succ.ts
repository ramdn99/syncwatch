// Debug builds only: torrent succession window test ("torrentsucc"), run by
// scripts/dev/torrent-lab.ps1 -Scenario torrentsucc with three windows and qBittorrent as the
// only seeder. The host (a) shares the multi-file magnet; b and c accept; c leaves; the host is
// killed (a real crash); b takes over; c joins again and must start directly on the chosen file
// with the file list from the NEW host, never from the swarm (no "getting file list" phase).
// Real UI code path, real mpv, real engine; no mouse or keyboard.

import { invoke } from '@tauri-apps/api/core';
import { devJoin } from './devjoin';
import { room, torrents } from '../lib/room';
import { act, check, current, has, sleep, waitFor, type Report } from './scenario';

interface Snap {
  state: { fileLoaded: boolean; paused: boolean; timePos: number | null };
}
const snap = () => invoke<Snap>('snapshot');
let cid = 50_000;
const say = (text: string) => room.action({ type: 'chat', clientId: cid++, text: `zqs ${text}` });
const said = (text: string) => has(`zqs ${text}`);
const playing = (s: Snap) => s.state.fileLoaded && !s.state.paused;

async function waitSnap(label: string, pred: (s: Snap) => boolean, timeoutMs: number): Promise<Snap> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label}`);
    await sleep(200);
  }
}

export async function succHost(report: Report, magnet: string, fileName: string) {
  await waitFor('two members', (s) => s.phase === 'inRoom' && s.members.length === 3, 90_000);
  await check(report, 'host shares the multi-file magnet', async () => {
    const info = await torrents.metadata(magnet);
    const f = info.files.find((x) => x.path.endsWith(fileName));
    if (!f) throw new Error('lab video not in the torrent');
    await room.setTorrentMedia(magnet, f.index, f.path, f.size);
    await waitSnap('host loaded', (s) => s.state.fileLoaded, 120_000);
  });
  await waitFor('c left', said('c-out'), 120_000);
  await waitFor('c gone', (s) => s.members.length === 2, 60_000);
  await check(report, 'b streams; host plays', async () => {
    await waitFor('b ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 120_000);
    await act({ type: 'play' });
    await waitSnap('host playing', playing, 30_000);
  });
  await sleep(4000);
  await say('host-crash');
  await sleep(1000);
  // The lab kills this window now (no clean leave).
  report('KILL-HOST');
  await sleep(60_000);
}

export async function succMember(report: Report) {
  await check(report, 'b accepts and streams', async () => {
    await waitFor('confirm', (x) => x.confirmMagnet != null, 120_000);
    await act({ type: 'acceptMagnet' });
    await waitSnap('b playing', playing, 180_000);
  });
  await waitFor('crash marker', said('host-crash'), 120_000);
  await check(report, 'b takes over as host after the crash; its torrent keeps going', async () => {
    const s = await waitFor('b is host', (x) => x.isHost && x.phase === 'inRoom', 90_000);
    report(`INFO new host: ${s.members.find((m) => m.isHost)?.name}; torrent ${s.torrent?.phase}`);
    if (!s.torrent || s.torrent.phase === 'failed') throw new Error(`torrent ${s.torrent?.phase}`);
  });
  await say('b-is-host');
  await check(report, 'b (new host) sees c join and start on the file', async () => {
    await waitFor('c back and ready', (s) => s.members.some((m) => m.name === 'Late C' && m.media === 'ready'), 240_000);
  });
  await waitFor('c done', said('c-done'), 120_000);
  await sleep(2000);
}

export async function succLate(report: Report) {
  let code: string | null = null;
  await check(report, 'c accepts first, then leaves before the crash', async () => {
    await waitFor('confirm', (x) => x.confirmMagnet != null, 120_000);
    await act({ type: 'acceptMagnet' });
    code = (await current()).roomCode;
    await sleep(3000);
    await say('c-out');
    await sleep(500);
    await room.leave();
    await waitFor('left', (s) => !s.active, 20_000);
  });
  if (!code) return;
  // Wait for the crash and the takeover (c is outside the room: it cannot see the chat).
  await sleep(75_000);
  await check(report, 'c joins after the succession and starts directly on the chosen file (file list from the new host, no swarm lookup)', async () => {
    await devJoin('Late C', code!, null);
    const s = await waitFor('confirm again', (x) => x.confirmMagnet != null, 90_000);
    const host = s.members.find((m) => m.isHost)?.name;
    report(`INFO c sees host: ${host}`);
    if (host !== 'Member B') throw new Error(`host is ${host}, expected Member B`);
    const phases = new Set<string>();
    const t0 = Date.now();
    await act({ type: 'acceptMagnet' });
    for (;;) {
      const [x, r] = [await snap(), await current()];
      if (r.torrent) phases.add(r.torrent.phase);
      if (x.state.fileLoaded) break;
      if (Date.now() - t0 > 120_000) throw new Error(`never opened; phases ${[...phases].join(',')}`);
      await sleep(100);
    }
    report(`INFO c: player open after ${Date.now() - t0} ms; phases seen: ${[...phases].join(' > ')}`);
    if (phases.has('metadata')) throw new Error('c fetched the file list from the swarm');
    await say('c-done');
  });
  await sleep(3000);
}


// Debug builds only: scripted two-window checks (scripts/dev/two-windows.ps1 -Scenario chat).
// Each window runs its half through the SAME code path as the UI buttons (room.action ->
// invoke('room_action') -> Rust -> signaling/bridge -> the other window) and verifies the
// outcome in its own room state. No mouse or keyboard input is simulated.

import { invoke } from '@tauri-apps/api/core';
import { onRoomState, room, type RoomAction, type RoomState } from '../lib/room';

type Report = (line: string) => void;

let latest: RoomState | null = null;

async function current(): Promise<RoomState> {
  latest = await room.state();
  return latest;
}

async function waitFor(label: string, pred: (s: RoomState) => boolean, timeoutMs = 20_000): Promise<RoomState> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await current();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label}`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const texts = (s: RoomState) => s.chat.filter((c) => !c.divider).map((c) => c.text);
const has = (t: string) => (s: RoomState) => texts(s).includes(t);
const rejected = (what: string) => (s: RoomState) => s.diag.rejected.some((r) => r.what === `chat message refused: ${what}`);
let clientId = 1;
const say = (text: string) => room.action({ type: 'chat', clientId: clientId++, text });
const act = (a: RoomAction) => room.action(a);

async function check(report: Report, name: string, f: () => Promise<unknown>) {
  try {
    await f();
    report(`PASS ${name}`);
  } catch (e) {
    report(`FAIL ${name}: ${String(e)}`);
    throw e;
  }
}

async function host(report: Report) {
  const s0 = await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  const member = s0.members.find((m) => !m.isYou)!.id;
  const cfg = async (c: Partial<RoomState['chatConfig']>) => {
    const base = (await current()).chatConfig;
    await act({ type: 'chatConfig', ...base, ...c });
  };
  await check(report, 'chat member -> host', () => waitFor('m1', has('m1: hello host')));
  await check(report, 'chat host -> member (sent)', () => say('h1: hello member'));
  await check(report, 'chat enabled OFF is sent', async () => {
    await cfg({ enabled: false });
    await waitFor('own config off', (s) => !s.chatConfig.enabled);
  });
  await check(report, 'chat enabled OFF is enforced by the host', async () => {
    await waitFor('member refused (Disabled)', rejected('Disabled'));
    if (has('m2: while disabled')(await current())) throw new Error('a message got through while chat was off');
  });
  await check(report, 'chat enabled ON again', async () => {
    await cfg({ enabled: true });
    await waitFor('m3', has('m3: chat back on'));
  });
  await check(report, 'hide chat from members ON', async () => {
    await cfg({ hiddenFromMembers: true });
    await waitFor('own config hidden', (s) => s.chatConfig.hiddenFromMembers);
    await sleep(500);
    await say('h2: hidden from members');
    await waitFor('m4 (member confirms it did not see h2)', has('m4: hidden ack'));
  });
  await check(report, 'hide chat from members OFF', async () => {
    await cfg({ hiddenFromMembers: false });
    await waitFor('own config visible', (s) => !s.chatConfig.hiddenFromMembers);
  });
  await check(report, 'slow mode is enforced', async () => {
    await cfg({ slowModeSecs: 10 });
    await waitFor('member refused (SlowMode)', rejected('SlowMode'));
    await cfg({ slowModeSecs: 0 });
    await waitFor('own config slow off', (s) => s.chatConfig.slowModeSecs === 0);
  });
  await check(report, 'delete message', async () => {
    await sleep(1500);
    const s = await current();
    const msg = s.chat.find((c) => c.text === 'm3: chat back on');
    if (!msg) throw new Error('m3 not found');
    await act({ type: 'deleteChat', id: msg.id });
    await waitFor('m3 gone here', (x) => !has('m3: chat back on')(x));
    await waitFor('member confirms deletion', has('m5: deleted ok'));
  });
  await check(report, 'clear chat', async () => {
    await act({ type: 'clearChat' });
    await waitFor('own chat empty', (x) => texts(x).length === 0);
    await waitFor('member confirms clear', has('m6: cleared ok'));
  });
  await check(report, 'mute in chat', async () => {
    await act({ type: 'muteChat', member, muted: true });
    await waitFor('member refused (Muted)', rejected('Muted'));
    if (has('m7x: while muted')(await current())) throw new Error('a muted message got through');
  });
  await check(report, 'unmute in chat', async () => {
    await act({ type: 'muteChat', member, muted: false });
    await waitFor('m7', has('m7: unmuted'));
  });
}

async function member(report: Report) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'chat member -> host (sent)', () => say('m1: hello host'));
  await check(report, 'chat host -> member', () => waitFor('h1', has('h1: hello member')));
  await check(report, 'sees chat turned off, and its message is refused', async () => {
    await waitFor('config off', (s) => !s.chatConfig.enabled);
    await say('m2: while disabled');
    await waitFor('own refusal (Disabled)', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('Disabled')));
  });
  await check(report, 'sees chat turned on again and can send', async () => {
    await waitFor('config on', (s) => s.chatConfig.enabled);
    await say('m3: chat back on');
    await waitFor('own echo m3', has('m3: chat back on'));
  });
  await check(report, 'hidden chat: does not receive the host message', async () => {
    await waitFor('config hidden', (s) => s.chatConfig.hiddenFromMembers);
    await sleep(2500);
    if (has('h2: hidden from members')(await current())) throw new Error('received a hidden message');
    await say('m4: hidden ack');
    await waitFor('own echo m4', has('m4: hidden ack'));
  });
  await check(report, 'slow mode refuses a quick second message', async () => {
    await waitFor('config visible', (s) => !s.chatConfig.hiddenFromMembers);
    await waitFor('slow mode on', (s) => s.chatConfig.slowModeSecs > 0);
    await say('m5a: slow one');
    await sleep(300);
    await say('m5b: slow two');
    await waitFor('own refusal (SlowMode)', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('SlowMode')));
    await waitFor('slow mode off', (s) => s.chatConfig.slowModeSecs === 0);
  });
  await check(report, 'sees the deleted message disappear', async () => {
    await waitFor('m3 deleted', (s) => !has('m3: chat back on')(s));
    await sleep(1200);
    await say('m5: deleted ok');
  });
  await check(report, 'sees the chat cleared', async () => {
    await waitFor('chat cleared', (s) => texts(s).length === 0);
    await sleep(1200);
    await say('m6: cleared ok');
  });
  await check(report, 'muted: own message is refused', async () => {
    await waitFor('muted', (s) => s.members.some((m) => m.isYou && m.chatMuted));
    await say('m7x: while muted');
    await waitFor('own refusal (Muted)', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('Muted')));
  });
  await check(report, 'unmuted: can send again', async () => {
    await waitFor('unmuted', (s) => s.members.some((m) => m.isYou && !m.chatMuted));
    await sleep(1200);
    await say('m7: unmuted');
    await waitFor('own echo m7', has('m7: unmuted'));
  });
}

// ------------------------------------------------------------------ 'rooms' scenario ---

interface Snap {
  state: { fileLoaded: boolean };
  tracks: unknown[];
}
const snap = () => invoke<Snap>('snapshot');
async function waitPlayer(label: string, loaded: boolean, timeoutMs = 20_000) {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (s.state.fileLoaded === loaded && (loaded || s.tracks.length === 0)) return;
    if (Date.now() > end) throw new Error(`player: ${label}`);
    await sleep(200);
  }
}
async function assertCleanRoom(who: string) {
  await sleep(1500);
  const p = await snap();
  if (p.state.fileLoaded) throw new Error(`${who}: a video is loaded in the new room`);
  if (p.tracks.length) throw new Error(`${who}: ${p.tracks.length} old tracks are still listed`);
  const s = await current();
  if (s.missingFile) throw new Error(`${who}: the choose-my-file panel came back (${s.missingFile.name})`);
  if (s.mediaTitle) throw new Error(`${who}: media title ${s.mediaTitle} survived`);
  if (texts(s).length) throw new Error(`${who}: chat from the old room survived`);
  const withMedia = s.members.filter((m) => m.media !== 'none');
  if (withMedia.length) throw new Error(`${who}: members still report media (${withMedia.map((m) => m.media).join(',')})`);
}

async function roomsHost(report: Report, media: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  await check(report, 'room 1: host loads the video', async () => {
    await room.setLocalMedia(media);
    await waitPlayer('host file loaded', true);
  });
  await check(report, 'room 1: member reports ready', () =>
    waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 30_000),
  );
  await check(report, 'closing the room unloads the host player', async () => {
    await sleep(1000);
    await room.leave();
    await waitFor('ended', (s) => !s.active && s.ended != null);
    await waitPlayer('host unloaded', false);
  });
  await check(report, 'room 2: created clean', async () => {
    await sleep(1000);
    const out = await room.create('Host A', null);
    if (out.status !== 'inRoom') throw new Error(`create: ${out.status}`);
    const s = await current();
    await invoke('dev_scenario_share', { code: s.roomCode });
    await waitFor('member joined room 2', (x) => x.members.length === 2, 30_000);
    await assertCleanRoom('host');
  });
}

async function roomsMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'room 1: member chooses the same file', async () => {
    await waitFor('choose-my-file panel', (s) => s.missingFile?.panelOpen === true);
    await room.chooseFile(media);
    await waitFor('verified', (s) => s.fileStatus === 'verified', 30_000);
    await waitPlayer('member file loaded', true);
    const s = await current();
    if (s.missingFile?.panelOpen) throw new Error('the panel is still open after choosing');
  });
  await check(report, 'room closed: the member player is unloaded', async () => {
    await waitFor('room closed', (s) => !s.active && s.ended?.kind === 'roomClosed', 60_000);
    await waitPlayer('member unloaded', false);
  });
  await check(report, 'room 2: joined clean, no video loaded', async () => {
    let code: string | null = null;
    const end = Date.now() + 30_000;
    while (!code && Date.now() < end) {
      code = await invoke<string | null>('dev_scenario_shared');
      if (!code) await sleep(300);
    }
    if (!code) throw new Error('no room 2 code');
    const out = await room.join('Member B', code, null);
    if (out.status !== 'inRoom') throw new Error(`join: ${out.status}`);
    await assertCleanRoom('member');
  });
}

let started = false;

/** Starts the scenario if the debug build was launched with one (once per window). */
export async function maybeRunScenario() {
  // React StrictMode runs effects twice in development: run the scenario only once.
  if (started) return;
  started = true;
  const cfg = await invoke<{ name: string; role: string; media: string | null } | null>('dev_scenario').catch(() => null);
  if (!cfg || (cfg.name !== 'chat' && cfg.name !== 'rooms')) return;
  const report: Report = (line) => void invoke('dev_scenario_report', { line });
  onRoomState((s) => (latest = s));
  report(`START ${cfg.role}`);
  try {
    if (cfg.name === 'rooms') {
      if (!cfg.media) throw new Error('no media for the rooms scenario');
      if (cfg.role === 'host') await roomsHost(report, cfg.media);
      else await roomsMember(report, cfg.media);
    } else if (cfg.role === 'host') await host(report);
    else await member(report);
    report('DONE');
  } catch {
    report('DONE');
  }
}

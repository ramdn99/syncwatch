// Debug builds only: scripted two-window checks (scripts/dev/two-windows.ps1 -Scenario chat).
// Each window runs its half through the SAME code path as the UI buttons (room.action ->
// invoke('room_action') -> Rust -> signaling/bridge -> the other window) and verifies the
// outcome in its own room state. No mouse or keyboard input is simulated.

import { invoke } from '@tauri-apps/api/core';
import { onRoomState, room, type RoomAction, type RoomState } from '../lib/room';

export type Report = (line: string) => void;

let latest: RoomState | null = null;

export async function current(): Promise<RoomState> {
  latest = await room.state();
  return latest;
}

export async function waitFor(label: string, pred: (s: RoomState) => boolean, timeoutMs = 20_000): Promise<RoomState> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await current();
    if (pred(s)) return s;
    if (Date.now() > end) throw new Error(`timed out waiting for: ${label}`);
    await new Promise((r) => setTimeout(r, 150));
  }
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export const texts = (s: RoomState) => s.chat.filter((c) => !c.divider).map((c) => c.text);
export const has = (t: string) => (s: RoomState) => texts(s).includes(t);
const rejected = (what: string) => (s: RoomState) => s.diag.rejected.some((r) => r.what === `chat message refused: ${what}`);
let clientId = 1;
const say = (text: string) => room.action({ type: 'chat', clientId: clientId++, text });
export const act = (a: RoomAction) => room.action(a);

export async function check(report: Report, name: string, f: () => Promise<unknown>) {
  try {
    await f();
    report(`PASS ${name}`);
  } catch (e) {
    report(`FAIL ${name}: ${String(e)}`);
    throw e;
  }
}

// ------------------------------------------------------------------ 'chat' scenario ----
// Host a, member b, late joiner c. Chat state is checked in the member's own DOM: the input
// must be DISABLED with the right text whenever the host's state says it cannot send.

/** The member's chat input as rendered (disabled flag, placeholder, status line). */
function chatInput() {
  const ta = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Chat message"]');
  const status = document.querySelector('[data-testid="chat-input-status"]')?.textContent ?? null;
  const mine = document.querySelector('[data-testid="my-chat-status"]')?.textContent ?? null;
  return { found: ta != null, disabled: ta?.disabled ?? false, placeholder: ta?.placeholder ?? '', status, mine };
}

async function expectInput(label: string, want: 'enabled' | 'muted' | 'off') {
  const end = Date.now() + 10_000;
  for (;;) {
    const i = chatInput();
    const text = want === 'muted' ? 'You are muted by the host' : 'Chat is turned off by the host';
    const ok =
      i.found &&
      (want === 'enabled'
        ? !i.disabled && i.status == null && (i.mine == null || i.mine.endsWith('on'))
        : i.disabled && i.placeholder === text && i.status === text && (i.mine == null || i.mine.endsWith(want)));
    if (ok) return;
    if (Date.now() > end) throw new Error(`${label}: input ${JSON.stringify(i)}`);
    await sleep(150);
  }
}

async function host(report: Report) {
  const s0 = await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  const member = s0.members.find((m) => !m.isYou)!.id;
  const cfg = async (c: Partial<{ enabled: boolean; slowModeSecs: number }>) => {
    const base = (await current()).chatState;
    await act({ type: 'chatConfig', enabled: base.enabled, slowModeSecs: base.slowModeSecs, ...c });
  };
  await check(report, 'chat member -> host', () => waitFor('m1', has('m1: hello host')));
  await check(report, 'chat host -> member (sent)', () => say('h1: hello member'));
  await check(report, 'mute: the muted member is refused, the host does not receive it', async () => {
    await act({ type: 'muteChat', member, muted: true });
    await waitFor('member refused (Muted)', rejected('Muted'), 20_000);
    if (has('m2: while muted')(await current())) throw new Error('a muted message got through');
  });
  await say('h2: to everyone while muted');
  await check(report, 'still muted after the member reconnects', () =>
    // m2, m3 before the reconnect and one more after it: three refusals.
    waitFor('third refusal', (s) => s.diag.rejected.some((r) => r.what === 'chat message refused: Muted' && r.count >= 3), 60_000),
  );
  await check(report, 'unmute takes effect at once (and the muted member had seen h2)', async () => {
    await act({ type: 'muteChat', member, muted: false });
    await waitFor('m3 and m4', (s) => has('m3: saw h2')(s) && has('m4: unmuted')(s), 20_000);
  });
  await check(report, 'chat off: members refused, the host still reaches everyone', async () => {
    await cfg({ enabled: false });
    await waitFor('member refused (ChatDisabled)', rejected('ChatDisabled'), 20_000);
    if (has('m5: while off')(await current())) throw new Error('got through while off');
    await say('h3: host while off');
  });
  await check(report, 'a third window joins late while chat is off', async () => {
    await invoke('dev_scenario_share', { code: (await current()).roomCode });
    await waitFor('third member', (s) => s.members.length === 3, 90_000);
    await sleep(8000); // the late joiner checks its disabled input meanwhile
  });
  await check(report, 'chat on again: members can send (they saw h3)', async () => {
    await cfg({ enabled: true });
    await waitFor('m6, m7 and c1', (s) => has('m6: saw h3')(s) && has('m7: on again')(s) && has('c1: late checked')(s), 30_000);
  });
  await check(report, 'slow mode is enforced', async () => {
    await cfg({ slowModeSecs: 10 });
    await waitFor('member refused (SlowMode)', rejected('SlowMode'), 20_000);
    await cfg({ slowModeSecs: 0 });
  });
  await check(report, 'delete and clear', async () => {
    await sleep(1500);
    const msg = (await current()).chat.find((c) => c.text === 'm7: on again');
    if (!msg) throw new Error('m7 not found');
    await act({ type: 'deleteChat', id: msg.id });
    await waitFor('member saw delete', has('m8: deleted ok'), 20_000);
    await act({ type: 'clearChat' });
    await waitFor('member saw clear', has('m9: cleared ok'), 20_000);
  });
}

async function member(report: Report) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'input enabled at start', () => expectInput('start', 'enabled'));
  await check(report, 'chat member -> host (sent)', () => say('m1: hello host'));
  await check(report, 'chat host -> member', () => waitFor('h1', has('h1: hello member')));
  await check(report, 'after mute: input disabled with "You are muted by the host"', async () => {
    await waitFor('muted state', (s) => s.chatState.muted, 20_000);
    await expectInput('muted', 'muted');
  });
  await check(report, 'a send while muted is refused without an error', async () => {
    await say('m2: while muted');
    await waitFor('own refusal', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('Muted')), 15_000);
    await expectInput('muted after refusal', 'muted');
  });
  await check(report, 'a muted member still sees chat', async () => {
    await waitFor('h2', has('h2: to everyone while muted'), 20_000);
    await say('m3: saw h2'); // refused while muted (the host counts it); resent after unmute
  });
  await check(report, 'after a reconnect while muted: still disabled', async () => {
    await invoke('dev_reconnect');
    await sleep(1500);
    await waitFor('back in the room', (s) => s.phase === 'inRoom', 30_000);
    await expectInput('muted after reconnect', 'muted');
    await say('m3: saw h2 (still muted)');
  });
  await check(report, 'after unmute: input enabled again', async () => {
    await waitFor('unmuted', (s) => !s.chatState.muted, 30_000);
    await expectInput('unmuted', 'enabled');
    await say('m3: saw h2');
    await say('m4: unmuted');
  });
  await check(report, 'after chat off: input disabled with "Chat is turned off by the host"', async () => {
    await waitFor('chat off', (s) => !s.chatState.enabled, 20_000);
    await expectInput('chat off', 'off');
    await say('m5: while off');
    await waitFor('own refusal', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('ChatDisabled')), 15_000);
    await waitFor('h3', has('h3: host while off'), 20_000);
  });
  await check(report, 'after chat on: input enabled again', async () => {
    await waitFor('chat on', (s) => s.chatState.enabled, 180_000);
    await expectInput('chat on', 'enabled');
    await say('m6: saw h3');
    await sleep(1300);
    await say('m7: on again');
    await sleep(1300);
  });
  await check(report, 'slow mode refuses a quick second message', async () => {
    await waitFor('slow mode on', (s) => s.chatState.slowModeSecs > 0, 20_000);
    await say('m7a: slow one');
    await sleep(300);
    await say('m7b: slow two');
    await waitFor('own refusal (SlowMode)', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.endsWith('SlowMode')), 15_000);
    await waitFor('slow mode off', (s) => s.chatState.slowModeSecs === 0, 20_000);
  });
  await check(report, 'sees delete and clear', async () => {
    await waitFor('m7 deleted', (s) => !has('m7: on again')(s), 20_000);
    await sleep(1200);
    await say('m8: deleted ok');
    await waitFor('cleared', (s) => texts(s).length === 0, 20_000);
    await sleep(1200);
    await say('m9: cleared ok');
  });
}

/** Window c joins while chat is off: its snapshot must already disable the input. */
async function lateChat(report: Report) {
  // Join only once the host has turned chat off (it publishes the code then).
  let code: string | null = null;
  const end = Date.now() + 240_000;
  while (!code && Date.now() < end) {
    code = await invoke<string | null>('dev_scenario_shared');
    if (!code) await sleep(300);
  }
  if (!code) throw new Error('no room code from the host');
  await room.join('Late C', code, null);
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost), 60_000);
  await check(report, 'joined late while chat is off: input disabled with the right text', async () => {
    await waitFor('chat off in snapshot', (s) => !s.chatState.enabled, 10_000);
    await expectInput('late join while off', 'off');
  });
  await check(report, 'the late joiner sees the host message sent while chat was off', () => waitFor('h3', has('h3: host while off'), 20_000));
  // Report to the host: members cannot send while off, so wait for chat on.
  await waitFor('chat on', (s) => s.chatState.enabled, 180_000);
  await check(report, 'chat on again: the late joiner input is enabled', () => expectInput('late chat on', 'enabled'));
  await say('c1: late checked');
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
  const cfg = await invoke<{ name: string; role: string; media: string | null; extra: string[] } | null>('dev_scenario').catch(() => null);
  if (!cfg || !['chat', 'rooms', 'sync', 'chattime', 'late', 'popup', 'succession', 'torrent', 'skiprelay', 'torrentfile'].includes(cfg.name)) return;
  const report: Report = (line) => void invoke('dev_scenario_report', { line });
  onRoomState((s) => (latest = s));
  report(`START ${cfg.role}`);
  try {
    const m6 = await import('./scenario6');
    const media = cfg.media ?? '';
    if (cfg.name === 'torrentfile') {
      const m7 = await import('./scenario7');
      if (cfg.role === 'host') await m7.torrentFileHost(report, cfg.extra[0] ?? '', media, cfg.extra[1] ?? '');
      else if (cfg.role === 'late') await m7.torrentFileLate(report);
      else await m7.torrentFileMember(report);
    } else if (cfg.name === 'torrent') {
      const m7 = await import('./scenario7');
      if (cfg.role === 'host') await m7.torrentHost(report, media, cfg.extra[0] ?? '');
      else await m7.torrentMember(report, cfg.extra[0] ?? '');
    } else if (cfg.name === 'skiprelay') {
      if (cfg.role === 'host') await m6.skipRelayedHost(report, media, cfg.extra);
      else await m6.skipRelayedMember(report, media);
    } else if (cfg.name === 'sync') {
      if (cfg.role === 'host') await m6.syncHost(report, media, cfg.extra);
      else await m6.syncMember(report, media);
    } else if (cfg.name === 'succession') {
      if (cfg.role === 'host') await m6.successionHost(report, media);
      else await m6.successionMember(report, media);
    } else if (cfg.name === 'popup') {
      if (cfg.role === 'host') await m6.popupHost(report, media);
      else await m6.popupMember(report, media);
    } else if (cfg.name === 'chattime') {
      if (cfg.role === 'host') await m6.chatTimeHost(report, media);
      else await m6.chatTimeMember(report, media);
    } else if (cfg.name === 'late') {
      if (cfg.role === 'host') await m6.lateHost(report, media);
      else if (cfg.role === 'late') await m6.lateJoiner(report, cfg.extra[0] ?? '', media);
      else await m6.lateMember(report, media);
    } else if (cfg.name === 'rooms') {
      if (!cfg.media) throw new Error('no media for the rooms scenario');
      if (cfg.role === 'host') await roomsHost(report, cfg.media);
      else await roomsMember(report, cfg.media);
    } else if (cfg.role === 'host') await host(report);
    else if (cfg.role === 'late') await lateChat(report);
    else await member(report);
    report('DONE');
  } catch {
    report('DONE');
  }
}
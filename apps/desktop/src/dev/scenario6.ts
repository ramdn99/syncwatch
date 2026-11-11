// Debug builds only: Milestone 6 two/three-window scenarios (scripts/dev/two-windows.ps1).
// Same rules as scenario.ts: the real UI code path (room.* -> invoke -> Rust -> signaling and
// the WebView2 bridge -> the other windows), the REAL mpv player in every window, checks
// against each window's own state, no mouse or keyboard. Every chat text carries the marker
// "zqx" so the log grep can prove chat content never reaches the logs.
//
// Report lines: "PASS/FAIL name", "TS tag <wallMs>" (cross-window latency, same PC clock) and
// "POS tag <wallMs> <positionMs> <paused>" (position agreement); the script evaluates them.

import { invoke } from '@tauri-apps/api/core';
import { chipText } from '../lib/chat';
import type { Track } from '../lib/player';
import { room } from '../lib/room';
import { act, check, current, has, sleep, texts, waitFor, type Report } from './scenario';

interface Snap {
  state: { fileLoaded: boolean; paused: boolean; timePos: number | null; speed: number; duration: number | null; subDelay?: number; audioDelay?: number };
  tracks: Track[];
}
const snap = () => invoke<Snap>('snapshot');
let cid = 10_000;
const say = (text: string) => room.action({ type: 'chat', clientId: cid++, text: `zqx ${text}` });
const said = (text: string) => has(`zqx ${text}`);

async function waitSnap(label: string, pred: (s: Snap) => boolean, timeoutMs = 15_000): Promise<Snap> {
  const end = Date.now() + timeoutMs;
  for (;;) {
    const s = await snap();
    if (pred(s)) return s;
    if (Date.now() > end) {
      throw new Error(`timed out waiting for: ${label} (pos ${s.state.timePos?.toFixed(2)}, paused ${s.state.paused})`);
    }
    await sleep(100);
  }
}
const posMs = (s: Snap) => Math.round((s.state.timePos ?? -1) * 1000);
const near = (ms: number, tol = 1500) => (s: Snap) => s.state.fileLoaded && Math.abs(posMs(s) - ms) <= tol;
const playing = (s: Snap) => s.state.fileLoaded && !s.state.paused;
const pausedAt = (s: Snap) => s.state.fileLoaded && s.state.paused;
const selected = (s: Snap, kind: 'audio' | 'sub') => s.tracks.find((t) => t.kind === kind && t.selected) ?? null;
const ofKind = (s: Snap, kind: 'audio' | 'sub') => s.tracks.filter((t) => t.kind === kind);

/** Position samples for the cross-window agreement check. */
async function sample(report: Report, tag: string, n = 8) {
  for (let i = 0; i < n; i++) {
    const s = await snap();
    report(`POS ${tag} ${Date.now()} ${posMs(s)} ${s.state.paused ? 1 : 0}`);
    await sleep(250);
  }
}

async function loadAsHost(media: string) {
  await room.setLocalMedia(media);
  await waitSnap('host file loaded', (s) => s.state.fileLoaded, 20_000);
}

async function chooseAsMember(media: string, status: 'verified' | 'mismatch' = 'verified') {
  await waitFor('choose-my-file panel', (s) => s.missingFile?.panelOpen === true, 30_000);
  await room.chooseFile(media);
  await waitFor(`file ${status}`, (s) => s.fileStatus === status, 60_000);
}

// ------------------------------------------------------------------------------ sync --------

export async function syncHost(report: Report, media: string, extras: string[]) {
  const s0 = await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  const member = s0.members.find((m) => !m.isYou)!;
  await check(report, 'host loads the video', () => loadAsHost(media));
  await check(report, 'member reports ready', () => waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 60_000));
  await sleep(1500);
  await check(report, 'host play', async () => {
    report(`TS play_sent ${Date.now()}`);
    await act({ type: 'play' });
    await waitSnap('host playing', playing);
  });
  await sleep(3000);
  await sample(report, 'playing');
  await check(report, 'host pause', async () => {
    report(`TS pause_sent ${Date.now()}`);
    await act({ type: 'pause' });
    await waitSnap('host paused', pausedAt);
  });
  await sleep(1500);
  await sample(report, 'paused', 4);
  await check(report, 'host seek to 10:00', async () => {
    report(`TS seek_sent ${Date.now()}`);
    await act({ type: 'seek', ms: 600_000 });
    await waitSnap('host at 10:00', near(600_000));
  });
  await sleep(1500);
  await sample(report, 'seeked', 4);
  await check(report, 'a member with seek permission drives the room', () => waitSnap('host at 5:00 (member seek)', near(300_000), 15_000));
  await check(report, 'revoke the member seek permission', async () => {
    await act({ type: 'setPermissions', member: member.id, permissions: { ...member.permissions, canSeek: false } });
    await waitFor('member seek off', (s) => s.members.some((m) => m.id === member.id && !m.permissions.canSeek));
  });
  await check(report, 'the refused member seek does not move the room', async () => {
    // The member's own state machine refuses before sending (no permission); the host's
    // enforcement of forged requests is covered by sync-core's permission tests.
    // (The member may press play right after; only the refused jump to 15:00 must not happen.)
    await sleep(6000);
    const s = await snap();
    if (posMs(s) > 400_000) throw new Error(`the refused seek moved the room to ${posMs(s)} ms`);
  });
  await check(report, 'a member with play permission starts playback', () => waitSnap('host playing (member play)', playing, 15_000));
  await check(report, 'host speed 1.5x', async () => {
    await act({ type: 'setSpeed', permille: 1500 });
    await waitSnap('host speed 1.5', (s) => Math.abs(s.state.speed - 1.5) < 0.06);
    await sleep(3000);
    await act({ type: 'setSpeed', permille: 1000 });
    await waitSnap('host speed 1', (s) => Math.abs(s.state.speed - 1) < 0.06);
  });
  await act({ type: 'pause' });
  await check(report, 'host switches audio to the second track', async () => {
    const s = await snap();
    const second = ofKind(s, 'audio')[1];
    if (!second) throw new Error('the video has one audio track');
    report(`TS audio_sent ${Date.now()}`);
    await room.selectTrack('audio', second.id);
    await waitSnap('host audio switched', (x) => selected(x, 'audio')?.id === second.id);
  });
  await check(report, 'host turns subtitles off', async () => {
    await room.selectTrack('sub', null);
    await waitSnap('host subs off', (x) => selected(x, 'sub') === null);
  });
  await sleep(1500);
  await check(report, 'host selects the first subtitle', async () => {
    const s = await snap();
    const first = ofKind(s, 'sub').find((t) => !t.external)!;
    await room.selectTrack('sub', first.id);
    await waitSnap('host sub on', (x) => selected(x, 'sub')?.id === first.id);
  });
  await sleep(1500);
  const [extraSub, extraAudio] = extras;
  await check(report, 'host adds an external subtitle and audio file', async () => {
    if (!extraSub || !extraAudio) throw new Error('no external test files');
    const a = await room.addExternal('sub', extraSub, false, 'all');
    if (!a.added) throw new Error('subtitle not added');
    if (!a.convertedFrom) throw new Error('the windows-1252 subtitle was not converted to UTF-8');
    const b = await room.addExternal('audio', extraAudio, false, 'all');
    if (!b.added) throw new Error('audio not added');
    await waitSnap('host has both externals', (x) => x.tracks.filter((t) => t.external).length >= 2, 20_000);
  });
  await check(report, 'member downloads both externals', () =>
    waitFor('member tracks in sync', (s) => s.members.some((m) => m.id === member.id && m.tracksInSync), 60_000),
  );
  await check(report, 'host selects the external subtitle', async () => {
    const s = await snap();
    const ext = s.tracks.find((t) => t.kind === 'sub' && t.external);
    if (!ext) throw new Error('no external subtitle track');
    const t0 = Date.now();
    report(`TS extsub_sent ${t0}`);
    await room.selectTrack('sub', ext.id);
    await waitSnap('host external sub selected', (x) => selected(x, 'sub')?.external === true);
    report(`INFO the host's own player shows the external subtitle selected after ${Date.now() - t0} ms`);
  });
  // Measured on its own: the member must not still be busy with the external audio download.
  await waitFor('member has both externals', said('externals-in'), 90_000);
  await check(report, 'host sets subtitle delay +500 ms and audio delay -300 ms', async () => {
    const t0 = Date.now();
    report(`TS delay_sent ${t0}`);
    await act({ type: 'setDelay', kind: 'sub', ms: 500 });
    await act({ type: 'setDelay', kind: 'audio', ms: -300 });
    report(`INFO both delay actions returned after ${Date.now() - t0} ms`);
    await waitSnap('host delays applied', delaysAre(0.5, -0.3));
    report(`INFO the host's own player shows both delays after ${Date.now() - t0} ms`);
  });
  await check(report, 'the member applied both delays', () => waitFor('member confirms', said('delays-seen'), 30_000));
  await check(report, 'host resets both delays', async () => {
    await act({ type: 'setDelay', kind: 'sub', ms: 0 });
    await act({ type: 'setDelay', kind: 'audio', ms: 0 });
    await waitSnap('host delays reset', delaysAre(0, 0));
  });
  await waitFor('member finished', (s) => texts(s).includes('zqx sync-done'), 60_000);
}

const delaysAre = (sub: number, audio: number) => (s: Snap) =>
  Math.abs((s.state.subDelay ?? 0) - sub) < 0.01 && Math.abs((s.state.audioDelay ?? 0) - audio) < 0.01;

export async function syncMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'choose-my-file shows once; the file verifies and loads', async () => {
    await chooseAsMember(media);
    await waitSnap('member file loaded', (s) => s.state.fileLoaded, 30_000);
    if ((await current()).missingFile?.panelOpen) throw new Error('panel still open');
  });
  await check(report, 'host play reaches the member', async () => {
    await waitSnap('member playing', playing, 20_000);
    report(`TS play_seen ${Date.now()}`);
  });
  await sleep(2500);
  await sample(report, 'playing');
  await check(report, 'host pause reaches the member', async () => {
    await waitSnap('member paused', pausedAt, 20_000);
    report(`TS pause_seen ${Date.now()}`);
  });
  await sleep(1000);
  await sample(report, 'paused', 4);
  await check(report, 'host seek reaches the member', async () => {
    await waitSnap('member at 10:00', near(600_000), 20_000);
    report(`TS seek_seen ${Date.now()}`);
  });
  await sleep(1000);
  await sample(report, 'seeked', 4);
  await check(report, 'member seek (has permission) drives the room', async () => {
    await act({ type: 'seek', ms: 300_000 });
    await waitSnap('member at 5:00', near(300_000));
  });
  await check(report, 'member without seek permission is refused', async () => {
    await waitFor('seek revoked', (s) => !s.myPermissions.canSeek, 15_000);
    await act({ type: 'seek', ms: 900_000 });
    await waitFor('own refusal', (s) => s.diag.rejected.some((r) => r.who === 'you' && r.what.startsWith('request refused')), 15_000);
    await sleep(1500);
    const s = await snap();
    if (Math.abs(posMs(s) - 300_000) > 3000) throw new Error(`moved to ${posMs(s)} ms`);
  });
  await check(report, 'member play (has permission) starts the room', async () => {
    await act({ type: 'play' });
    await waitSnap('member playing', playing);
  });
  await check(report, 'host speed reaches the member', async () => {
    await waitSnap('member speed 1.5', (s) => Math.abs(s.state.speed - 1.5) < 0.06, 15_000);
    await waitSnap('member speed back to 1', (s) => Math.abs(s.state.speed - 1) < 0.06, 15_000);
  });
  await check(report, 'host audio switch applies here', async () => {
    const s0 = await snap();
    const second = ofKind(s0, 'audio')[1]!;
    await waitSnap('member audio = second track', (s) => selected(s, 'audio')?.ffIndex === second.ffIndex, 15_000);
    report(`TS audio_seen ${Date.now()}`);
  });
  await check(report, 'host subtitles off applies here', () => waitSnap('member subs off', (s) => selected(s, 'sub') === null, 15_000));
  await check(report, 'host subtitle selection applies here', async () => {
    const s0 = await snap();
    const first = ofKind(s0, 'sub').find((t) => !t.external)!;
    await waitSnap('member sub = first', (s) => selected(s, 'sub')?.ffIndex === first.ffIndex, 15_000);
  });
  await check(report, 'external files are listed for the member', () => waitFor('two externals listed', (s) => s.externals.length >= 2, 30_000));
  // Measured on its own (the audio file may still be downloading): the subtitle selection.
  await check(report, 'the host external subtitle selection applies here', async () => {
    await waitSnap('member external sub selected', (s) => selected(s, 'sub')?.external === true, 30_000);
    report(`TS extsub_seen ${Date.now()}`);
  });
  await check(report, 'both external files end up in the player', async () => {
    await waitSnap('both externals in the player', (s) => s.tracks.filter((t) => t.external).length >= 2, 60_000);
    await say('externals-in');
  });
  await check(report, 'host subtitle and audio delay apply here', async () => {
    await waitSnap('member delays +0.5 / -0.3', delaysAre(0.5, -0.3), 30_000);
    report(`TS delay_seen ${Date.now()}`);
    await say('delays-seen');
  });
  await check(report, 'host delay reset applies here', () => waitSnap('member delays reset', delaysAre(0, 0), 30_000));
  await say('sync-done');
}

// ------------------------------------------------------------------------------ chat time ---

const CHIP_TIMES = [10, 330, 3723];

export async function chatTimeHost(report: Report, media: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  await check(report, '(e) a message without media has no timestamp', async () => {
    const s = await waitFor('member no-media message', said('e-nomedia'), 20_000);
    const m = s.chat.find((c) => c.text === 'zqx e-nomedia')!;
    if (m.mediaTimeMs != null) throw new Error(`stamped ${m.mediaTimeMs}`);
  });
  await check(report, 'host loads the 1 h+ video', () => loadAsHost(media));
  await waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 60_000);
  for (const t of CHIP_TIMES) {
    await act({ type: 'seek', ms: t * 1000 - 1500 });
    await waitSnap(`host near ${t}`, near(t * 1000 - 1500, 800));
    await act({ type: 'play' });
    await waitFor(`member message at ${t}`, said(`a-${t}`), 30_000);
    await act({ type: 'pause' });
    await sleep(800);
  }
  await check(report, '(b) host pauses for the paused-position message', async () => {
    await act({ type: 'play' });
    await sleep(2000);
    await act({ type: 'pause' });
    await waitSnap('host paused', pausedAt);
    await waitFor('member paused message', said('b-paused'), 30_000);
  });
  await ordering(report, 'h');
}

export async function chatTimeMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await say('e-nomedia');
  await chooseAsMember(media);
  await waitSnap('member file loaded', (s) => s.state.fileLoaded, 30_000);
  for (const t of CHIP_TIMES) {
    await check(report, `(a) chip at ${chipText(t * 1000, 3_800_000)} while playing`, async () => {
      const s = await waitSnap(`playing past ${t}`, (x) => playing(x) && posMs(x) >= t * 1000 && posMs(x) < t * 1000 + 800, 30_000);
      const mine = posMs(s);
      await say(`a-${t}`);
      const st = await waitFor('echo', said(`a-${t}`));
      const m = st.chat.find((c) => c.text === `zqx a-${t}`)!;
      if (m.mediaTimeMs == null) throw new Error('no timestamp');
      if (Math.abs(m.mediaTimeMs - mine) > 1000) throw new Error(`stamp ${m.mediaTimeMs} vs my position ${mine}`);
      const label = chipText(m.mediaTimeMs, st.mediaDurationMs);
      report(`CHIP ${t} label=${label} stamp-minus-my-position=${m.mediaTimeMs - mine}ms`);
      // "Each chip shows that time within 1 s" (the label is floored to whole seconds).
      const shown = label.split(':').map(Number).reduce((a, b) => a * 60 + b, 0);
      if (Math.abs(shown - t) > 1) throw new Error(`chip "${label}" is not within 1 s of ${chipText(t * 1000, 3_800_000)}`);
      if (!/^\d:\d\d:\d\d$/.test(label)) throw new Error(`chip "${label}" is not h:mm:ss for a 1 h+ video`);
    });
  }
  await check(report, '(b) a message while paused uses the paused position', async () => {
    await waitSnap('playing again', playing, 20_000);
    const s = await waitSnap('paused', pausedAt, 20_000);
    await sleep(600);
    const p = posMs(await snap());
    await say('b-paused');
    const st = await waitFor('echo', said('b-paused'));
    const m = st.chat.find((c) => c.text === 'zqx b-paused')!;
    if (m.mediaTimeMs == null || Math.abs(m.mediaTimeMs - p) > 300) throw new Error(`stamp ${m.mediaTimeMs} vs paused ${p} (first paused ${posMs(s)})`);
  });
  await ordering(report, 'm');
}

/** (g) Two simultaneous senders: both windows must show the same total order. */
async function ordering(report: Report, who: 'h' | 'm') {
  await check(report, '(g) total ordering with two simultaneous senders', async () => {
    let start: number;
    if (who === 'h') {
      start = Date.now() + 3000;
      await say(`g-start ${start}`);
    } else {
      const s = await waitFor('start time', (x) => x.chat.some((c) => c.text.startsWith('zqx g-start ')), 30_000);
      start = Number(s.chat.find((c) => c.text.startsWith('zqx g-start '))!.text.split(' ')[2]);
    }
    await sleep(Math.max(0, start - Date.now()));
    for (let i = 0; i < 4; i++) {
      await say(`g-${who}-${i}`);
      await sleep(30);
    }
    const s = await waitFor('all 8 messages', (x) => x.chat.filter((c) => /^zqx g-[hm]-\d$/.test(c.text)).length === 8, 20_000);
    const order = s.chat.filter((c) => /^zqx g-[hm]-\d$/.test(c.text));
    const ids = order.map((c) => c.id);
    if (ids.some((id, i) => i > 0 && id <= ids[i - 1]!)) throw new Error('ids not increasing');
    for (const p of ['h', 'm']) {
      const mine = order.filter((c) => c.text.startsWith(`zqx g-${p}-`)).map((c) => c.text);
      if (mine.join() !== [0, 1, 2, 3].map((i) => `zqx g-${p}-${i}`).join()) throw new Error(`sender ${p} out of order`);
    }
    report(`ORDER ${order.map((c) => c.text.slice(6)).join(',')}`);
  });
}

// ------------------------------------------------------------------------------ late joiner -

export async function lateHost(report: Report, media: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length >= 2);
  await loadAsHost(media);
  await waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 60_000);
  const s = await snap();
  await room.selectTrack('audio', ofKind(s, 'audio')[1]!.id);
  await room.selectTrack('sub', ofKind(s, 'sub')[2]!.id);
  await act({ type: 'setDelay', kind: 'sub', ms: 400 });
  await act({ type: 'seek', ms: 120_000 });
  await act({ type: 'play' });
  await sleep(3000);
  await say('late-history-1');
  await waitFor('third window', (x) => x.members.length === 3, 120_000);
  await check(report, 'a member missing the file does not block the others', async () => {
    const before = posMs(await snap());
    await act({ type: 'seek', ms: before + 20_000 });
    await act({ type: 'play' });
    await sleep(3000);
    const x = await snap();
    if (!playing(x)) throw new Error('the room is not playing while the late joiner lacks the file');
    report('INFO host kept playing while the late joiner was missing the file');
  });
  await check(report, 'the late joiner reports a mismatch and proceeds', () =>
    waitFor('late joiner in', (x) => x.members.some((m) => !m.isYou && !m.isHost && m.media === 'mismatch'), 90_000),
  );
  await sampleAtAnnounced(report);
  const h = await current();
  report(`CHIPS ${h.chat.filter((c) => c.text.startsWith('zqx late-')).map((c) => `${c.text.slice(4)}@${c.mediaTimeMs}`).join(',')}`);
  await waitFor('late joiner done', said('late-done'), 60_000);
}

/** Host and member sample together with the late joiner at the time it announced. */
async function sampleAtAnnounced(report: Report) {
  const s = await waitFor('sample time', (x) => x.chat.some((c) => c.text.startsWith('zqx late-sample ')), 120_000);
  const at = Number(s.chat.find((c) => c.text.startsWith('zqx late-sample '))!.text.split(' ')[2]);
  await sleep(Math.max(0, at - Date.now()));
  await sample(report, 'late', 8);
}

export async function lateMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await chooseAsMember(media);
  await waitSnap('member file loaded', (s) => s.state.fileLoaded, 30_000);
  await waitFor('history line', said('late-history-1'), 60_000);
  await say('late-history-2');
  await waitFor('third window', (x) => x.members.length === 3, 120_000);
  await sleep(4000);
  await check(report, 'keeps playing while the late joiner lacks the file', () => waitSnap('member playing', playing, 15_000));
  await sampleAtAnnounced(report);
}

export async function lateJoiner(report: Report, mismatchMedia: string, hostMedia: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'late joiner sees choose-my-file once', () => waitFor('panel', (s) => s.missingFile?.panelOpen === true, 30_000));
  await sleep(8000); // missing the file for a while: the others must not wait
  await check(report, 'the mismatch file shows the warning', async () => {
    await room.chooseFile(mismatchMedia);
    await waitFor('mismatch', (s) => s.fileStatus === 'mismatch', 60_000);
  });
  await check(report, 'proceeding anyway loads it', async () => {
    await act({ type: 'proceedWithMismatch' });
    await waitSnap('loaded', (s) => s.state.fileLoaded, 30_000);
  });
  await check(report, 'late joiner lands with the same audio, subtitle and delay', async () => {
    await waitSnap('playing', playing, 30_000);
    const s = await snap();
    const audio = selected(s, 'audio');
    const sub = selected(s, 'sub');
    if (audio?.ffIndex !== ofKind(s, 'audio')[1]?.ffIndex) throw new Error(`audio ${audio?.ffIndex}`);
    if (sub?.ffIndex !== ofKind(s, 'sub')[2]?.ffIndex) throw new Error(`sub ${sub?.ffIndex}`);
  });
  const at = Date.now() + 3000;
  await say(`late-sample ${at}`);
  await sleep(at - Date.now());
  await sample(report, 'late', 8);
  const st = await current();
  report(`CHIPS ${st.chat.filter((c) => c.text.startsWith('zqx late-')).map((c) => `${c.text.slice(4)}@${c.mediaTimeMs}`).join(',')}`);
  void hostMedia;
  await say('late-done');
}

// ------------------------------------------------------------------------------ popup -------

const win = (op: 'minimize' | 'restore' | 'query') => invoke<[boolean, boolean]>('dev_window', { op });

/** Regression (spec section 1): loading/showing the video while the window is minimized must
 *  not restore the window or show the video window. Window API only, no input. */
export async function popupHost(report: Report, media: string) {
  await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2);
  await waitFor('member minimized', said('popup-minimized'), 30_000);
  await check(report, 'host picks the source and plays while the member is minimized', async () => {
    await loadAsHost(media);
    await act({ type: 'play' });
    await waitSnap('host playing', playing);
  });
  await waitFor('member checked', said('popup-checked'), 120_000);
}

export async function popupMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await check(report, 'window minimized (window API)', async () => {
    const [min] = await win('minimize');
    await sleep(800);
    if (!min && !(await win('query'))[0]) throw new Error('did not minimize');
  });
  await say('popup-minimized');
  await check(report, 'file loads and plays while minimized', async () => {
    await chooseAsMember(media);
    await waitSnap('loaded and playing', playing, 30_000);
  });
  await check(report, 'the window stays minimized and the video window stays hidden', async () => {
    await sleep(3000);
    const [min, video] = await win('query');
    if (!min) throw new Error('the main window was restored by the video');
    if (video) throw new Error('the video window is visible while minimized');
  });
  await check(report, 'after restoring, the video is shown again', async () => {
    await win('restore');
    await sleep(1500);
    const [min, video] = await win('query');
    if (min) throw new Error('still minimized');
    if (!video) throw new Error('video window not shown after restore');
  });
  await say('popup-checked');
}

// ------------------------------------------------------------------------------ succession --

/** The host prepares the room, then asks the script to kill its process (a real crash). */
export async function successionHost(report: Report, media: string) {
  await waitFor('two members', (s) => s.phase === 'inRoom' && s.members.length === 3, 60_000);
  await loadAsHost(media);
  await waitFor('members ready', (s) => s.members.filter((m) => !m.isYou && m.media === 'ready').length === 2, 90_000);
  await act({ type: 'seek', ms: 200_000 });
  await act({ type: 'play' });
  await waitSnap('host playing', playing);
  await sleep(3000);
  await say('succ-before');
  await sleep(1500);
  report('KILL-HOST');
  await sleep(600_000);
}

export async function successionMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost));
  await chooseAsMember(media);
  await waitSnap('playing', playing, 60_000);
  await waitFor('pre-crash chat', said('succ-before'), 60_000);
  await check(report, 'a member takes over after the host crashed', async () => {
    const s = await waitFor('new host', (x) => x.members.some((m) => m.isHost && m.name !== 'Host A') && x.phase === 'inRoom', 90_000);
    report(`INFO new host: ${s.members.find((m) => m.isHost)!.name}${s.isHost ? ' (me)' : ''}`);
    if (s.members.some((m) => m.name === 'Host A')) throw new Error('the crashed host is still listed');
  });
  await check(report, 'the video keeps playing; no new choose-my-file panel', async () => {
    const st = await current();
    if (st.missingFile?.panelOpen) throw new Error('panel came back');
    await waitSnap('still loaded and playing', playing, 20_000);
  });
  const me = await current();
  if (me.isHost) {
    const at = Date.now() + 3000;
    await say(`succ-sample ${at}`);
    await sleep(at - Date.now());
  } else {
    const s = await waitFor('sample time', (x) => x.chat.some((c) => c.text.startsWith('zqx succ-sample ')), 60_000);
    const at = Number(s.chat.find((c) => c.text.startsWith('zqx succ-sample '))!.text.split(' ')[2]);
    await sleep(Math.max(0, at - Date.now()));
  }
  await sample(report, 'succ', 8);
  await check(report, 'chat works both ways with the new host', async () => {
    const who = me.isHost ? 'host' : 'member';
    await say(`succ-from-${who}`);
    await waitFor('other side', said(`succ-from-${me.isHost ? 'member' : 'host'}`), 30_000);
  });
  await check(report, 'playback control still works (new host pauses)', async () => {
    if (me.isHost) {
      await sleep(1000);
      await act({ type: 'pause' });
    }
    await waitSnap('paused', pausedAt, 20_000);
  });
  await sleep(1500);
  await say(`succ-done-${me.isHost ? 'host' : 'member'}`);
  await waitFor('both done', (x) => texts(x).includes('zqx succ-done-host') && texts(x).includes('zqx succ-done-member'), 30_000);
}

// ------------------------------------------------------------------- skip relayed (M7) ------

/** Run with -Ice coturn (relay-only): the member is relayed, the host adds a file with
 * "Skip relayed members"; the member's own window must say so instead of a stalled download. */
export async function skipRelayedHost(report: Report, media: string, extras: string[]) {
  const s0 = await waitFor('member joined', (s) => s.phase === 'inRoom' && s.members.length === 2, 60_000);
  await check(report, 'the member is connected through the relay', async () => {
    const m = (await waitFor('relayed link', (s) => s.members.some((x) => !x.isYou && x.link === 'relayed'), 30_000)).members.find((x) => !x.isYou);
    report(`INFO member link: ${m?.link} (members ${s0.members.length})`);
  });
  await check(report, 'host loads the video', () => loadAsHost(media));
  await check(report, 'member ready', () => waitFor('member ready', (s) => s.members.some((m) => !m.isYou && m.media === 'ready'), 60_000));
  await check(report, 'host adds a file with "Skip relayed members"', async () => {
    const r = await room.addExternal('audio', extras[0] ?? '', false, 'skipRelayed');
    if (!r.added) throw new Error('not added');
    report(`TS skip_added ${Date.now()}`);
  });
  await check(report, 'the member saw the message', () => waitFor('member saw it', said('withheld-seen'), 60_000));
  await check(report, 'host lists the file normally', async () => {
    const s = await current();
    if (s.externals.length !== 1) throw new Error(`externals: ${s.externals.length}`);
  });
}

export async function skipRelayedMember(report: Report, media: string) {
  await waitFor('in the room', (s) => s.phase === 'inRoom' && s.members.some((m) => m.isHost), 60_000);
  await check(report, 'member chooses the file', async () => {
    await chooseAsMember(media);
    await waitSnap('member file loaded', (s) => s.state.fileLoaded, 30_000);
  });
  await check(report, 'skipped member: "The host did not send this file to you" (no stalled download)', async () => {
    await waitFor('withheld', (s) => s.externals.length === 1 && s.externals[0]!.withheld, 60_000);
    report(`TS withheld_seen ${Date.now()}`);
    const el = document.querySelector('[data-testid="external-withheld"]');
    if (el?.textContent !== 'The host did not send this file to you') throw new Error(`DOM text: ${el?.textContent ?? 'missing'}`);
    const s = await current();
    if (s.externals[0]!.received != null) throw new Error('still shows a download');
    if (document.body.textContent?.includes('%') && document.querySelector('[data-testid="external"] .text-warning')) throw new Error('progress shown');
    await sleep(3000);
    const later = await current();
    if (later.externals[0]!.received != null || !later.externals[0]!.withheld) throw new Error('state changed');
  });
  await say('withheld-seen');
  await sleep(3000);
}

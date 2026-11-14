import { formatTime } from './time';
// Bridge to the Rust room driver (src-tauri/src/room.rs). The Rust side owns all room state;
// the UI renders the `room-state` snapshots it pushes and sends actions back.

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { inTauri } from './player';

export type LinkState = 'connecting' | 'authenticating' | 'direct' | 'relayed' | 'reconnecting';
export type Phase = 'connecting' | 'authenticating' | 'inRoom' | 'reconnecting';
export type MediaText = 'none' | 'missing' | 'verifying' | 'ready' | 'mismatch' | 'downloading' | 'declined' | 'unavailable';

export interface Perms {
  canPlayPause: boolean;
  canSeek: boolean;
  canChangeMedia: boolean;
  canChangeSpeed: boolean;
}

export interface RoomMember {
  id: number;
  name: string;
  isHost: boolean;
  isYou: boolean;
  connected: boolean;
  link: LinkState | null;
  media: MediaText;
  buffering: boolean;
  tracksInSync: boolean;
  chatMuted: boolean;
  permissions: Perms;
  words: [string, string, string, string] | null;
  /** Torrent rooms: this member's download phase and whole-percent progress. */
  torrent: { phase: TorrentPhase; permille: number } | null;
  /** Torrent rooms: what a buffering member waits for. */
  wait: BufferWait | null;
}

/** A buffering participant's missing window at the room position (ADR 0012). */
export interface BufferWait {
  atMs: number;
  /** Bytes of the window still missing. */
  missing: number;
  /** How much of the window is there (0..1000). */
  permille: number;
}

export type TorrentPhase = 'metadata' | 'waitingForHost' | 'downloading' | 'streaming' | 'noPeers' | 'complete' | 'failed';

/** This window's own torrent download. */
export interface TorrentState {
  phase: TorrentPhase;
  haveBytes: number;
  size: number;
  downBps: number;
  upBps: number;
  peers: number;
  etaSecs: number | null;
  /** Downloaded byte ranges [start, end). */
  ranges: [number, number][];
  /** Readiness at the room position (display only; the popup is RoomState.stall). */
  window: { ready: boolean; atMs: number; missing: number; permille: number } | null;
  /** Only the playback stream runs (window short or just seeked). */
  exclusive: boolean;
  /** What is used to find peers. */
  trackers: number;
  dht: boolean;
  /** Web seeds listed in the torrent (not supported by the engine). */
  webSeeds: number;
  /** "Downloading and sharing have stopped. No further traffic." once complete. */
  completeMessage: string | null;
  error: string | null;
}

export interface RoomChat {
  id: number;
  sender: number;
  name: string;
  isHost: boolean;
  text: string;
  wallMs: number;
  mediaTimeMs: number | null;
  /** The media this message was stamped for (chips of older media are not seekable). */
  mediaEpoch: number;
  divider: string | null;
}

export type EndReason =
  | { kind: 'left' }
  | { kind: 'roomClosed' }
  | { kind: 'kicked'; banned: boolean }
  | { kind: 'authRejected'; reason: string }
  | { kind: 'refused'; reason: string }
  | { kind: 'noSuchRoom' }
  | { kind: 'roomFull' }
  | { kind: 'hostNotVerified' }
  | { kind: 'error'; message: string };

export interface RoomState {
  active: boolean;
  /** Increments for every room this app process enters. */
  sessionId: number;
  roomCode: string | null;
  phase: Phase | null;
  isHost: boolean;
  me: number | null;
  hasPassword: boolean;
  locked: boolean;
  members: RoomMember[];
  myPermissions: Perms;
  chat: RoomChat[];
  /** My chat state as the host last told me (the only source for whether I can send). */
  chatState: { enabled: boolean; slowModeSecs: number; muted: boolean };
  hostWords: [string, string, string, string] | null;
  hostLink: LinkState | null;
  needPassword: { wrong: boolean; retryAfterMs: number } | null;
  missingFile: { name: string; size: number; durationMs: number | null; seq: number; panelOpen: boolean } | null;
  fileStatus: 'verifying' | 'verified' | 'mismatch' | null;
  mediaTitle: string | null;
  toasts: string[];
  ended: EndReason | null;
  passwordIgnored: boolean;
  diag: RoomDiag;
  mediaEpoch: number;
  mediaDurationMs: number | null;
  gate: { active: boolean; waiting: GateWait[] };
  externals: { id: number; kind: 'audio' | 'sub'; name: string; size: number; received: number | null; failed: boolean; withheld: boolean }[];
  declined: string[];
  joining: boolean;
  torrent: TorrentState | null;
  /** Member: the host's magnet waits for our confirmation. */
  confirmMagnet: { name: string; size: number; infohash: string; torrentName: string | null } | null;
  /** The buffering popup of THIS window's own player (never about another member). */
  stall: { stalled: boolean; wait: BufferWait | null };
}

export interface GateWait {
  member: number;
  name: string;
  reason: string;
  wait: BufferWait | null;
}

export interface AddExternalOutcome {
  added: boolean;
  relayedMembers: string[];
  size: number;
  convertedFrom: string | null;
}

export interface RoomDiag {
  signalingHost: string;
  signalingConnected: boolean;
  signalingReconnects: number;
  events: { t: number; text: string }[];
  rejected: { member: number | null; who: string; what: string; count: number }[];
}

export interface EntryOutcome {
  status: 'inRoom' | 'needPassword' | 'pending' | 'ended';
  passwordIgnored: boolean;
  ended: EndReason | null;
}

export const PASSWORD_IGNORED_TEXT = 'This room has no password, so the one you typed was ignored.';

const NO_PERMS: Perms = { canPlayPause: false, canSeek: false, canChangeMedia: false, canChangeSpeed: false };

export const EMPTY_ROOM: RoomState = {
  active: false,
  sessionId: 0,
  roomCode: null,
  phase: null,
  isHost: false,
  me: null,
  hasPassword: false,
  locked: false,
  members: [],
  myPermissions: NO_PERMS,
  chat: [],
  chatState: { enabled: true, slowModeSecs: 0, muted: false },
  hostWords: null,
  hostLink: null,
  needPassword: null,
  missingFile: null,
  fileStatus: null,
  mediaTitle: null,
  toasts: [],
  ended: null,
  passwordIgnored: false,
  diag: { signalingHost: '', signalingConnected: false, signalingReconnects: 0, events: [], rejected: [] },
  mediaEpoch: 0,
  mediaDurationMs: null,
  gate: { active: false, waiting: [] },
  externals: [],
  declined: [],
  joining: false,
  torrent: null,
  confirmMagnet: null,
  stall: { stalled: false, wait: null },
};

export type RoomAction =
  | { type: 'chat'; clientId: number; text: string }
  | { type: 'setPermissions'; member: number; permissions: Perms }
  | { type: 'preset'; preset: 'everyone' | 'onlyMe' }
  | { type: 'kick'; member: number }
  | { type: 'ban'; member: number }
  | { type: 'lock'; locked: boolean }
  | { type: 'makeHost'; member: number }
  | { type: 'muteChat'; member: number; muted: boolean }
  | { type: 'deleteChat'; id: number }
  | { type: 'clearChat' }
  | { type: 'chatConfig'; enabled: boolean; slowModeSecs: number }
  | { type: 'localMute'; member: number; muted: boolean }
  | { type: 'waitForEveryone'; on: boolean }
  | { type: 'proceedWithMismatch' }
  | { type: 'play' }
  | { type: 'pause' }
  | { type: 'seek'; ms: number }
  | { type: 'setSpeed'; permille: number }
  | { type: 'resumeWithoutWaiting' }
  | { type: 'cancelDownload'; externalId: number }
  | { type: 'setDelay'; kind: 'audio' | 'sub'; ms: number }
  | { type: 'acceptMagnet' }
  | { type: 'declineMagnet' };

/**
 * One example of EVERY room action, exactly as the UI sends it. The Record type makes the
 * compiler insist on a complete list. room.contract.test.ts writes these to
 * room-actions.fixture.json, and the Rust test oom_actions_contract deserializes that file
 * with the real command argument type, so a field-name mismatch fails both test suites.
 */
export const ROOM_ACTION_EXAMPLES: { [K in RoomAction['type']]: Extract<RoomAction, { type: K }> } = {
  chat: { type: 'chat', clientId: 7, text: 'hello' },
  setPermissions: {
    type: 'setPermissions',
    member: 2,
    permissions: { canPlayPause: true, canSeek: false, canChangeMedia: false, canChangeSpeed: true },
  },
  preset: { type: 'preset', preset: 'onlyMe' },
  kick: { type: 'kick', member: 2 },
  ban: { type: 'ban', member: 2 },
  lock: { type: 'lock', locked: true },
  makeHost: { type: 'makeHost', member: 2 },
  muteChat: { type: 'muteChat', member: 2, muted: true },
  deleteChat: { type: 'deleteChat', id: 41 },
  clearChat: { type: 'clearChat' },
  chatConfig: { type: 'chatConfig', enabled: false, slowModeSecs: 10 },
  localMute: { type: 'localMute', member: 2, muted: true },
  waitForEveryone: { type: 'waitForEveryone', on: true },
  proceedWithMismatch: { type: 'proceedWithMismatch' },
  play: { type: 'play' },
  pause: { type: 'pause' },
  seek: { type: 'seek', ms: 330_000 },
  setSpeed: { type: 'setSpeed', permille: 1250 },
  resumeWithoutWaiting: { type: 'resumeWithoutWaiting' },
  cancelDownload: { type: 'cancelDownload', externalId: 3 },
  setDelay: { type: 'setDelay', kind: 'sub', ms: -250 },
  acceptMagnet: { type: 'acceptMagnet' },
  declineMagnet: { type: 'declineMagnet' },
};

/** The arguments object passed to invoke('room_action', ...). */
export const roomActionArgs = (action: RoomAction) => ({ action });

export const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  inTauri() ? invoke<T>(cmd, args) : Promise.reject(new Error('Rooms need the desktop app.'));

export const room = {
  create: (name: string, password: string | null) => call<EntryOutcome>('room_create', { name, password }),
  join: (name: string, code: string, password: string | null) => call<EntryOutcome>('room_join', { name, code, password }),
  state: () => (inTauri() ? invoke<RoomState>('room_state') : Promise.resolve(EMPTY_ROOM)),
  leave: () => call<void>('room_leave'),
  password: (password: string) => call<void>('room_password', { password }),
  action: (action: RoomAction) => call<void>('room_action', roomActionArgs(action)),
  chooseFile: (path: string) => call<void>('room_choose_file', { path }),
  setLocalMedia: (path: string) => call<void>('room_set_local_media', { path }),
  /** Host: select a track by its local player id (null = off). */
  selectTrack: (kind: 'audio' | 'sub', trackId: number | null) => call<void>('room_select_track', { kind, trackId }),
  /** Host: add an external file. policy: 'ask' warns before large files go to relayed members. */
  addExternal: (kind: 'audio' | 'sub', path: string, noLimit: boolean, policy: 'ask' | 'all' | 'skipRelayed') =>
    call<AddExternalOutcome>('room_add_external', { kind, path, noLimit, policy }),
  /** Host: the room plays file `fileIndex` of this magnet. */
  setTorrentMedia: (magnet: string, fileIndex: number, name: string, size: number) =>
    call<void>('room_set_torrent_media', { magnet, fileIndex, name, size }),
  /** Host: the room plays file `fileIndex` of the inspected .torrent file. */
  setTorrentFile: (infohash: string, fileIndex: number) => call<void>('room_set_torrent_file', { infohash, fileIndex }),
};

export interface TorrentFile {
  index: number;
  path: string;
  size: number;
  video: boolean;
}

export interface TorrentInfo {
  infohash: string;
  name: string;
  files: TorrentFile[];
  totalSize: number;
  trackers: number;
  /** Web seeds listed in the torrent (not supported: the torrent needs peers). */
  webSeeds: number;
}

export interface TorrentSettings {
  cacheDir: string | null;
  capGb: number;
  keepFinished: boolean;
  port: number;
  bind: string;
  dht: boolean;
  noticeAccepted: boolean;
  metaCapMb: number;
  windowBeforeSecs: number;
  windowAfterSecs: number;
  startupKb: number;
}

export const torrents = {
  /** Host: the magnet's file list (metadata only). */
  metadata: (magnet: string) => call<TorrentInfo>('torrent_metadata', { magnet }),
  /** Host: a .torrent file, read and validated locally (no network). */
  inspectFile: (path: string) => call<TorrentInfo>('torrent_inspect_file', { path }),
  settings: () => call<TorrentSettings>('torrent_settings_get'),
  saveSettings: (settings: TorrentSettings) => call<void>('torrent_settings_set', { settings }),
};

/** The notice every user acknowledges once before sharing or downloading a torrent. */
export const TORRENT_NOTICE =
  'You are responsible for what you stream. Only use torrents you have the right to download and share. While a torrent downloads, other people in its swarm can see your IP address.';

const PHASE_TEXT: Record<TorrentPhase, string> = {
  metadata: 'getting file list',
  waitingForHost: "waiting for the host's file list",
  downloading: 'downloading',
  streaming: 'downloading',
  noPeers: 'waiting for peers',
  complete: 'complete',
  failed: 'failed',
};

/** "buffering at 12:30, 60% of the window" (the room position and the window's progress). */
export function bufferingText(w: BufferWait | null): string {
  if (!w) return 'buffering';
  return `buffering at ${formatTime(w.atMs / 1000)}, ${Math.floor(w.permille / 10)}% of the window`;
}

/**
 * Short status for a member's torrent ("downloading 42%", "buffering at 12:30, 60% of the
 * window", "complete"...). `buffering` is the member's real need for data at the room position,
 * never its overall progress.
 */
export function torrentStatusText(t: { phase: TorrentPhase; permille: number } | null, buffering = false, wait: BufferWait | null = null): string | null {
  if (!t) return null;
  if (buffering && t.phase !== 'complete') return bufferingText(wait);
  const pct = Math.floor(t.permille / 10);
  return t.phase === 'downloading' || t.phase === 'streaming' || t.phase === 'noPeers' ? `${PHASE_TEXT[t.phase]} ${pct}%` : PHASE_TEXT[t.phase];
}

/**
 * The ready-gate banner (ADR 0012). The host sees why and what each member waits for ("Waiting
 * for Bo: buffering at 12:30 (needs ~3 MB more)"); a member sees "Waiting for Bo to catch up"
 * for another member's buffering. A member's own buffering is its popup, not this banner:
 * `null` when nothing is left to show.
 */
export function gateBannerText(gate: { active: boolean; waiting: GateWait[] }, me: number | null, isHost: boolean): string | null {
  if (!gate.active) return null;
  if (gate.waiting.length === 0) return 'Waiting for everyone...';
  const others = gate.waiting.filter((w) => isHost || w.member !== me);
  if (others.length === 0) return null;
  const part = (w: GateWait) => {
    if (w.reason !== 'buffering') return `${w.name} (${w.reason})`;
    if (!isHost) return `${w.name} to catch up`;
    if (!w.wait) return `${w.name}: buffering`;
    return `${w.name}: buffering at ${formatTime(w.wait.atMs / 1000)} (needs ~${fmtBytes(w.wait.missing)} more)`;
  };
  return `Waiting for ${others.map(part).join(', ')}...`;
}

/** What this window is doing while it has no data yet, and why it may be stuck. */
export function torrentLookText(t: TorrentState): string | null {
  if (t.phase === 'waitingForHost') return 'Waiting for the host to get the file list';
  const how = `DHT ${t.dht ? 'on' : 'off'}, ${t.trackers} tracker${t.trackers === 1 ? '' : 's'}`;
  if (t.phase === 'metadata') return `Looking for peers (${how})`;
  if (t.phase === 'noPeers') {
    return t.webSeeds > 0
      ? 'The torrent file was loaded, but no peers are sharing it yet. Its web seeds (HTTP downloads) are not supported.'
      : `The torrent file was loaded, but no peers are sharing it yet (${how}). Retrying automatically; a firewall or DHT being off can also cause this.`;
  }
  if ((t.phase === 'downloading' || t.phase === 'streaming') && t.peers > 0) return `${t.peers} peer${t.peers === 1 ? '' : 's'} found (${how})`;
  return null;
}

export const fmtBytes = (n: number) =>
  n >= 1e9 ? `${(n / 1e9).toFixed(2)} GB` : n >= 1e6 ? `${(n / 1e6).toFixed(1)} MB` : `${Math.max(0, Math.round(n / 1e3))} KB`;

export const fmtRate = (bps: number) => `${fmtBytes(bps)}/s`;

export function fmtEta(secs: number | null): string {
  if (secs == null) return '-';
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min`;
  return `${Math.floor(secs / 3600)} h ${Math.floor((secs % 3600) / 60)} min`;
}

export function onRoomState(cb: (s: RoomState) => void): () => void {
  if (!inTauri()) return () => {};
  let un: (() => void) | undefined;
  let gone = false;
  void listen<RoomState>('room-state', (e) => cb(e.payload)).then((u) => {
    if (gone) u();
    else un = u;
  });
  return () => {
    gone = true;
    un?.();
  };
}

/** Friendly text for why a room ended (null = nothing to explain). */
export function endText(r: EndReason | null): string | null {
  if (!r) return null;
  switch (r.kind) {
    case 'left':
      return null;
    case 'roomClosed':
      return 'The host closed the room.';
    case 'kicked':
      return r.banned ? 'The host removed you from the room and banned you for this session.' : 'The host removed you from the room.';
    case 'authRejected':
      return r.reason === 'TooManyAttempts'
        ? 'Too many wrong passwords. Try again later.'
        : 'The room did not accept you (the connection could not be verified).';
    case 'refused':
      return r.reason === 'locked'
        ? 'This room is locked: the host is not letting new people in.'
        : r.reason === 'full'
          ? 'This room is full.'
          : r.reason === 'banned'
            ? 'You are banned from this room for this session.'
            : 'The room did not accept you.';
    case 'noSuchRoom':
      return 'There is no room with that code. Check the code with the host.';
    case 'roomFull':
      return 'This room is full.';
    case 'hostNotVerified':
      return 'The host could not be verified, so the connection was closed for your safety.';
    case 'error':
      return r.message;
  }
}

/** Room codes are shown as XXXX-XXXX. */
export function formatCode(code: string | null): string {
  if (!code) return '';
  return code.length === 8 ? `${code.slice(0, 4)}-${code.slice(4)}` : code;
}

const NAME_KEY = 'syncwatch.displayName';
export function loadName(): string {
  try {
    return localStorage.getItem(NAME_KEY) ?? '';
  } catch {
    return '';
  }
}
export function saveName(n: string) {
  try {
    localStorage.setItem(NAME_KEY, n);
  } catch {
    /* storage unavailable */
  }
}

const PERM_NAMES: [keyof Perms, string][] = [
  ['canPlayPause', 'play and pause'],
  ['canSeek', 'seek'],
  ['canChangeSpeed', 'change speed'],
  ['canChangeMedia', 'change the source'],
];

/** My chat status for the status line: the host always 'on'; muted wins over chat off. */
export function chatStatus(rs: Pick<RoomState, 'isHost' | 'chatState'>): 'on' | 'muted' | 'off' {
  if (rs.isHost) return 'on';
  if (rs.chatState.muted) return 'muted';
  return rs.chatState.enabled ? 'on' : 'off';
}

/** Why I cannot send right now (null = I can). These are normal states, not errors. */
export function chatBlockedReason(rs: Pick<RoomState, 'isHost' | 'chatState'>): string | null {
  switch (chatStatus(rs)) {
    case 'muted':
      return 'You are muted by the host';
    case 'off':
      return 'Chat is turned off by the host';
    default:
      return null;
  }
}

/** "You can: seek, change speed" summary of a member's own permissions. */
export function permsSummary(p: Perms): string[] {
  return PERM_NAMES.filter(([k]) => p[k]).map(([, name]) => name);
}

/**
 * Turns a command error into a friendly message. Argument mismatches between the UI and the
 * app ("invalid args ...") are internal bugs: the details go to the diagnostics view.
 */
export function friendlyError(e: unknown): { text: string; detail: string } {
  const detail = String(e).replace(/^Error: /, '');
  if (/invalid args|missing field|unknown field|unknown variant/i.test(detail)) {
    return { text: 'SyncWatch could not do that because of an internal error. Details are in Diagnostics.', detail };
  }
  return { text: detail, detail };
}
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
  gate: { active: boolean; waiting: { member: number; name: string; reason: string }[] };
  externals: { id: number; kind: 'audio' | 'sub'; name: string; size: number; received: number | null; failed: boolean }[];
  declined: string[];
  joining: boolean;
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
  | { type: 'setDelay'; kind: 'audio' | 'sub'; ms: number };

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
};

/** The arguments object passed to invoke('room_action', ...). */
export const roomActionArgs = (action: RoomAction) => ({ action });

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
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
};

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
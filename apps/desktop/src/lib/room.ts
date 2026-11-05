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
  canChat: boolean;
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
  roomCode: string | null;
  phase: Phase | null;
  isHost: boolean;
  me: number | null;
  hasPassword: boolean;
  locked: boolean;
  members: RoomMember[];
  myPermissions: Perms;
  chat: RoomChat[];
  chatConfig: { enabled: boolean; hiddenFromMembers: boolean; slowModeSecs: number };
  hostWords: [string, string, string, string] | null;
  hostLink: LinkState | null;
  needPassword: { wrong: boolean; retryAfterMs: number } | null;
  missingFile: { name: string; size: number; durationMs: number | null; seq: number } | null;
  fileStatus: 'verified' | 'mismatch' | null;
  mediaTitle: string | null;
  toasts: string[];
  ended: EndReason | null;
}

const NO_PERMS: Perms = { canPlayPause: false, canSeek: false, canChangeMedia: false, canChangeSpeed: false, canChat: false };

export const EMPTY_ROOM: RoomState = {
  active: false,
  roomCode: null,
  phase: null,
  isHost: false,
  me: null,
  hasPassword: false,
  locked: false,
  members: [],
  myPermissions: NO_PERMS,
  chat: [],
  chatConfig: { enabled: true, hiddenFromMembers: false, slowModeSecs: 0 },
  hostWords: null,
  hostLink: null,
  needPassword: null,
  missingFile: null,
  fileStatus: null,
  mediaTitle: null,
  toasts: [],
  ended: null,
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
  | { type: 'chatConfig'; enabled: boolean; hiddenFromMembers: boolean; slowModeSecs: number }
  | { type: 'localMute'; member: number; muted: boolean }
  | { type: 'waitForEveryone'; on: boolean };

const call = <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
  inTauri() ? invoke<T>(cmd, args) : Promise.reject(new Error('Rooms need the desktop app.'));

export const room = {
  create: (name: string, password: string | null) => call<void>('room_create', { name, password }),
  join: (name: string, code: string, password: string | null) => call<void>('room_join', { name, code, password }),
  state: () => (inTauri() ? invoke<RoomState>('room_state') : Promise.resolve(EMPTY_ROOM)),
  leave: () => call<void>('room_leave'),
  password: (password: string) => call<void>('room_password', { password }),
  action: (action: RoomAction) => call<void>('room_action', { action }),
  chooseFile: (path: string) => call<void>('room_choose_file', { path }),
  setLocalMedia: (path: string) => call<void>('room_set_local_media', { path }),
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

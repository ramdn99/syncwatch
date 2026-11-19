// Thin bridge to the Rust player commands. Outside Tauri (vitest, plain browser) every call is
// a no-op so the UI can be developed and tested without the native side.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type TrackKind = 'video' | 'audio' | 'sub';

export interface Track {
  id: number;
  kind: TrackKind;
  ffIndex: number | null;
  lang: string | null;
  title: string | null;
  codec: string | null;
  selected: boolean;
  external: boolean;
  default: boolean;
  forced: boolean;
  channels: number | null;
}

export interface PlaybackState {
  paused: boolean;
  timePos: number | null;
  duration: number | null;
  speed: number;
  pausedForCache: boolean;
  eofReached: boolean;
  volume: number;
  mute: boolean;
  fileLoaded: boolean;
  cacheAhead: number | null;
  subDelay?: number;
  audioDelay?: number;
}

export type PlayerEvent =
  | { type: 'fileLoaded'; tracks: Track[] }
  | { type: 'tracksChanged'; tracks: Track[] }
  | ({ type: 'state' } & PlaybackState)
  | { type: 'endFile'; reason: string }
  | { type: 'shutdown' };

export type TrackSelection = { mode: 'id'; id: number } | { mode: 'off' };

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export const inTauri = (): boolean => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  if (!inTauri()) return undefined;
  return invoke<T>(cmd, args);
}

export const player = {
  open: (path: string) => call('player_open', { path }),
  setPaused: (paused: boolean) => call('player_set_paused', { paused }),
  seek: (seconds: number) => call('player_seek', { seconds: Math.max(0, seconds) }),
  /** Relative to the player's current position (rapid key repeats accumulate correctly). */
  seekBy: (delta: number) => call('player_seek_relative', { delta }),
  setVolume: (volume: number) => call('player_set_volume', { volume }),
  setMute: (mute: boolean) => call('player_set_mute', { mute }),
  setSubMargin: (scaledPx: number) => call('player_set_sub_margin', { scaledPx }),
  selectTrack: (kind: TrackKind, selection: TrackSelection) => call('player_select_track', { kind, selection }),
  addExternal: (kind: TrackKind, path: string) => call('player_add_external', { kind, path }),
};

export const app = {
  ready: () => call('app_ready'),
  setLayout: (center: Rect, columns: Rect[], fullscreen: boolean) =>
    call('set_layout', { rects: { center, columns, fullscreen } }),
  setFullscreen: (on: boolean) => call('set_fullscreen', { on }),
  snapshot: () =>
    call<{ state: PlaybackState; tracks: Track[]; vulkan: string; dev: boolean }>('snapshot'),
};

export function onPlayerEvent(cb: (e: PlayerEvent) => void): () => void {
  if (!inTauri()) return () => {};
  let un: UnlistenFn | undefined;
  let cancelled = false;
  void listen<PlayerEvent>('player-event', (e) => cb(e.payload)).then((u) => {
    if (cancelled) u();
    else un = u;
  });
  return () => {
    cancelled = true;
    un?.();
  };
}

export function trackLabel(t: Track): string {
  const parts = [t.lang, t.title].filter((x): x is string => !!x);
  if (parts.length === 0) parts.push(`Track ${t.id}`);
  if (t.codec) parts.push(t.codec);
  return parts.join(' · ');
}

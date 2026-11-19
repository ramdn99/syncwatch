// In-app updates (ADR 0016): the UI side of src-tauri/src/updater.rs.
import { listen } from '@tauri-apps/api/event';
import { call } from './room';
import { inTauri } from './player';

export interface UpdateState {
  phase: 'idle' | 'checking' | 'upToDate' | 'available' | 'downloading' | 'ready' | 'failed';
  version: string | null;
  notes: string | null;
  received: number;
  total: number | null;
  error: string | null;
}

export const IDLE_UPDATE: UpdateState = { phase: 'idle', version: null, notes: null, received: 0, total: null, error: null };

export const updates = {
  state: () => (inTauri() ? call<UpdateState>('update_state') : Promise.resolve(IDLE_UPDATE)),
  check: () => call<UpdateState>('update_check'),
  download: () => call<UpdateState>('update_download'),
  cancel: () => call<void>('update_cancel'),
  install: () => call<void>('update_install'),
};

export function onUpdateState(cb: (s: UpdateState) => void): () => void {
  if (!inTauri()) return () => {};
  let off: (() => void) | null = null;
  let dead = false;
  void listen<UpdateState>('update-state', (e) => cb(e.payload)).then((u) => (dead ? u() : (off = u)));
  return () => {
    dead = true;
    off?.();
  };
}

/** "12.3 of 58.0 MB" (or "12.3 MB" while the size is unknown). */
export function progressText(received: number, total: number | null): string {
  const mb = (n: number) => (n / (1024 * 1024)).toFixed(1);
  return total ? `${mb(received)} of ${mb(total)} MB` : `${mb(received)} MB`;
}

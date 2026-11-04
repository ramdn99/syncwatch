// Global keyboard shortcuts (spec section 7). Typing in chat or any input NEVER triggers them.

export type ShortcutAction =
  | 'togglePause'
  | 'seekBack'
  | 'seekForward'
  | 'volumeUp'
  | 'volumeDown'
  | 'toggleFullscreen'
  | 'toggleMute'
  | 'toggleLeft'
  | 'toggleRight'
  | 'focusChat'
  | 'exitFullscreen'
  | 'cycleAudio'
  | 'cycleSub';

/** Host-only actions (track control is host-only, spec section 6). */
export const HOST_ONLY: ReadonlySet<ShortcutAction> = new Set(['cycleAudio', 'cycleSub']);

export function isTypingTarget(t: EventTarget | null): boolean {
  if (!t || typeof (t as HTMLElement).tagName !== 'string') return false;
  const el = t as HTMLElement;
  const tag = el.tagName.toLowerCase();
  if (tag === 'textarea' || tag === 'select') return true;
  if (tag === 'input') {
    const type = (el as HTMLInputElement).type;
    return !['button', 'checkbox', 'radio', 'range', 'submit', 'reset'].includes(type);
  }
  return el.isContentEditable;
}

export function mapKey(
  e: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'metaKey' | 'target'>,
  isHost: boolean,
): ShortcutAction | null {
  if (e.ctrlKey || e.altKey || e.metaKey) return null;
  if (isTypingTarget(e.target)) return null;
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const action: ShortcutAction | null =
    k === ' ' ? 'togglePause'
    : k === 'ArrowLeft' ? 'seekBack'
    : k === 'ArrowRight' ? 'seekForward'
    : k === 'ArrowUp' ? 'volumeUp'
    : k === 'ArrowDown' ? 'volumeDown'
    : k === 'f' ? 'toggleFullscreen'
    : k === 'm' ? 'toggleMute'
    : k === '[' ? 'toggleLeft'
    : k === ']' ? 'toggleRight'
    : k === 'c' ? 'focusChat'
    : k === 'Escape' ? 'exitFullscreen'
    : k === 'a' ? 'cycleAudio'
    : k === 's' ? 'cycleSub'
    : null;
  if (action && HOST_ONLY.has(action) && !isHost) return null;
  return action;
}

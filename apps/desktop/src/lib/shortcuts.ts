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
  | 'cycleSub'
  | 'showHelp';

/** Host-only actions (track control is host-only, spec section 6). */
export const HOST_ONLY: ReadonlySet<ShortcutAction> = new Set(['cycleAudio', 'cycleSub']);

/** Every shortcut, as shown in the help overlay (one list, used by the overlay and its test). */
export const SHORTCUTS: { keys: string[]; label: string; action: ShortcutAction }[] = [
  { keys: ['Space'], label: 'Play or pause', action: 'togglePause' },
  { keys: ['Left', 'Right'], label: 'Seek 5 seconds back or forward', action: 'seekBack' },
  { keys: ['Up', 'Down'], label: 'Volume up or down', action: 'volumeUp' },
  { keys: ['M'], label: 'Mute or unmute', action: 'toggleMute' },
  { keys: ['F'], label: 'Fullscreen', action: 'toggleFullscreen' },
  { keys: ['Esc'], label: 'Leave fullscreen', action: 'exitFullscreen' },
  { keys: ['['], label: 'Show or hide the chat', action: 'toggleLeft' },
  { keys: [']'], label: 'Show or hide the members panel', action: 'toggleRight' },
  { keys: ['C'], label: 'Write in the chat', action: 'focusChat' },
  { keys: ['A'], label: 'Next audio track', action: 'cycleAudio' },
  { keys: ['S'], label: 'Next subtitle track (or off)', action: 'cycleSub' },
  { keys: ['?'], label: 'Show these shortcuts', action: 'showHelp' },
];

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
    : k === '?' ? 'showHelp'
    : null;
  if (action && HOST_ONLY.has(action) && !isHost) return null;
  return action;
}

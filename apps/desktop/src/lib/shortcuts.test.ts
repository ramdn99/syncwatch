import { describe, expect, it } from 'vitest';
import { formatTime } from './time';
import { mapKey } from './shortcuts';

const key = (k: string, target: EventTarget | null = document.body) => ({ key: k, ctrlKey: false, altKey: false, metaKey: false, target });

describe('mapKey', () => {
  it('maps the documented shortcuts', () => {
    expect(mapKey(key(' '), false)).toBe('togglePause');
    expect(mapKey(key('ArrowLeft'), false)).toBe('seekBack');
    expect(mapKey(key('F'), false)).toBe('toggleFullscreen');
    expect(mapKey(key('m'), false)).toBe('toggleMute');
    expect(mapKey(key('['), false)).toBe('toggleLeft');
    expect(mapKey(key(']'), false)).toBe('toggleRight');
    expect(mapKey(key('c'), false)).toBe('focusChat');
  });

  it('never fires while typing in chat or inputs', () => {
    const ta = document.createElement('textarea');
    const input = document.createElement('input');
    expect(mapKey(key(' ', ta), true)).toBeNull();
    expect(mapKey(key('f', input), true)).toBeNull();
    const range = document.createElement('input');
    range.type = 'range';
    expect(mapKey(key(' ', range), true)).toBe('togglePause');
  });

  it('track shortcuts are host only', () => {
    expect(mapKey(key('a'), true)).toBe('cycleAudio');
    expect(mapKey(key('a'), false)).toBeNull();
    expect(mapKey(key('s'), false)).toBeNull();
  });

  it('ignores modified keys', () => {
    expect(mapKey({ ...key('f'), ctrlKey: true }, true)).toBeNull();
  });
});

describe('formatTime', () => {
  it('uses mm:ss below an hour and h:mm:ss above', () => {
    expect(formatTime(0)).toBe('00:00');
    expect(formatTime(95.9)).toBe('01:35');
    expect(formatTime(4350)).toBe('1:12:30');
    expect(formatTime(null)).toBe('--:--');
    expect(formatTime(-1)).toBe('--:--');
  });
});

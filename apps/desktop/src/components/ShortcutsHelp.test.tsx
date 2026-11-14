import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { mapKey, SHORTCUTS } from '../lib/shortcuts';
import { ShortcutsHelp } from './ShortcutsHelp';

afterEach(cleanup);
const key = (k: string, target: EventTarget | null = document.body) => ({ key: k, ctrlKey: false, altKey: false, metaKey: false, target });

describe('shortcuts help', () => {
  it('"?" opens it; the list covers every spec shortcut; track keys are host-only', () => {
    expect(mapKey(key('?'), false)).toBe('showHelp');
    const listed = SHORTCUTS.flatMap((s) => s.keys);
    for (const k of ['Space', 'Left', 'Right', 'Up', 'Down', 'F', 'M', '[', ']', 'C']) expect(listed, k).toContain(k);
    expect(mapKey(key('a'), false)).toBeNull();
    expect(mapKey(key('a'), true)).toBe('cycleAudio');
    const input = document.createElement('input');
    expect(mapKey(key('?', input), true)).toBeNull();
  });

  it('shows every shortcut, marks host-only ones for members, closes with Esc', () => {
    const onClose = vi.fn();
    render(<ShortcutsHelp isHost={false} onClose={onClose} />);
    expect(screen.getAllByTestId('shortcut-row')).toHaveLength(SHORTCUTS.length);
    expect(screen.getAllByText('(host only)')).toHaveLength(2);
    expect(document.activeElement?.getAttribute('aria-label')).toBe('Close');
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalled();
  });
});

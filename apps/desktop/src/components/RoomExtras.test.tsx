import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { EMPTY_ROOM, friendlyError, permsSummary, type RoomState } from '../lib/room';
import { diagnosticsText } from './Diagnostics';
import { MyPermissions } from './HostControls';

const ALL = { canPlayPause: true, canSeek: true, canChangeMedia: true, canChangeSpeed: true, canChat: true };

describe('own permissions badge', () => {
  it('lists exactly what the member may do and updates with new props', () => {
    const { rerender } = render(<MyPermissions perms={{ ...ALL, canChangeMedia: false, canPlayPause: false }} />);
    const items = () => screen.getAllByRole('listitem').map((li) => li.textContent);
    expect(items()).toEqual(['seek', 'change speed', 'chat']);
    // The host revokes seek and chat: the badge follows.
    rerender(<MyPermissions perms={{ ...ALL, canSeek: false, canChat: false }} />);
    expect(items()).toEqual(['play and pause', 'change speed', 'change the source']);
    rerender(<MyPermissions perms={{ canPlayPause: false, canSeek: false, canChangeMedia: false, canChangeSpeed: false, canChat: false }} />);
    expect(screen.queryAllByRole('listitem')).toHaveLength(0);
    expect(screen.getByText(/only watch/)).toBeTruthy();
  });

  it('summarizes permissions', () => {
    expect(permsSummary(ALL)).toHaveLength(5);
  });
});

describe('errors', () => {
  it('argument mismatches become a friendly message, with the details kept', () => {
    const f = friendlyError(new Error("invalid args `action` for command `room_action`: missing field `client_id`"));
    expect(f.text).toMatch(/internal error/);
    expect(f.detail).toMatch(/client_id/);
    expect(friendlyError('You are not in a room.').text).toBe('You are not in a room.');
  });
});

describe('diagnostics text', () => {
  it('has connection, rejections and events but never chat content', () => {
    const rs: RoomState = {
      ...EMPTY_ROOM,
      active: true,
      isHost: true,
      phase: 'inRoom',
      chat: [{ id: 1, sender: 2, name: 'Mo', isHost: false, text: 'my secret chat text', wallMs: 0, mediaTimeMs: null, divider: null }],
      diag: {
        signalingHost: '127.0.0.1:8787',
        signalingConnected: true,
        signalingReconnects: 1,
        events: [{ t: 1.5, text: 'Mo joined' }],
        rejected: [{ member: 2, who: 'Mo', what: 'chat message refused: SlowMode', count: 2 }],
      },
    };
    const text = diagnosticsText(rs, [{ at: 0, text: 'missing field `client_id`' }]);
    expect(text).toContain('127.0.0.1:8787');
    expect(text).toContain('Mo: chat message refused: SlowMode x2');
    expect(text).toContain('Mo joined');
    expect(text).toContain('client_id');
    expect(text).not.toContain('my secret chat text');
  });
});

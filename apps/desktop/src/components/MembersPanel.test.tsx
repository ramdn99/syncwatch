// Spec section 7 members list rows: link with latency, and the muted icon (also for a local mute).
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import type { RoomMember } from '../lib/room';
import { MembersPanel } from './MembersPanel';

afterEach(cleanup);

const member = (id: number, name: string, extra: Partial<RoomMember> = {}): RoomMember => ({
  id,
  name,
  isHost: id === 0,
  isYou: false,
  connected: true,
  link: 'direct',
  media: 'ready',
  buffering: false,
  tracksInSync: true,
  chatMuted: false,
  permissions: { canPlayPause: true, canSeek: true, canChangeMedia: false, canChangeSpeed: false },
  words: null,
  torrent: null,
  wait: null,
  ...extra,
});

describe('members list rows', () => {
  it('shows the latency next to direct and relayed links only', () => {
    render(
      <MembersPanel
        viewerIsHost
        onAction={() => {}}
        members={[
          member(0, 'Host', { isYou: true, link: null }),
          member(1, 'Ana', { rttMs: 42 }),
          member(2, 'Ben', { link: 'relayed', rttMs: 118 }),
          member(3, 'Cy', { link: 'reconnecting', connected: false, rttMs: 70 }),
        ]}
      />,
    );
    const links = screen.getAllByTestId('member-link').map((e) => e.textContent);
    expect(links).toEqual(['Direct · 42 ms', 'Relayed · 118 ms', 'Reconnecting...']);
  });

  it('a member muted locally gets the muted icon with a name', () => {
    render(<MembersPanel viewerIsHost={false} onAction={() => {}} members={[member(0, 'Host'), member(1, 'Ana')]} />);
    expect(screen.queryByLabelText('Muted by you')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Actions for Ana' }));
    fireEvent.click(screen.getByRole('menuitem', { name: 'Mute locally' }));
    expect(screen.getByLabelText('Muted by you')).toBeTruthy();
  });
});

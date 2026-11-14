import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ErrorBanner } from '../components/ErrorBanner';
import { roomErrors } from '../lib/errors';
import { EMPTY_ROOM, type RoomState } from '../lib/room';
import { ERRORS, isNoServer, type ErrorId } from './errors';

afterEach(cleanup);

describe('error states (Milestone 8)', () => {
  it('every state has a title and a friendly text; the actionable ones have an action', () => {
    const actionable: ErrorId[] = ['noServer', 'firewall', 'unsupportedFile', 'trackUnresolved', 'noPeers', 'diskFull'];
    for (const [id, e] of Object.entries(ERRORS)) {
      expect(e.title.length, id).toBeGreaterThan(5);
      expect(e.text.length, id).toBeGreaterThan(20);
      if (actionable.includes(id as ErrorId)) expect(e.action, id).not.toBeNull();
    }
  });

  it('recognizes every "server unreachable" text of the signaling client', () => {
    for (const s of [
      'Could not reach the signaling server sw.example: the name could not be resolved (DNS)',
      'Could not reach the signaling server sw.example: connection refused',
      'Could not reach the signaling server sw.example: timed out',
      'Could not reach the signaling server sw.example: secure connection (TLS) failed',
      'The connection to the signaling server sw.example was lost',
    ])
      expect(isNoServer(s), s).toBe(true);
    expect(isNoServer('The room is full.')).toBe(false);
  });

  it('maps the room state to its banners', () => {
    const me = { id: 1, name: 'B', isHost: false, isYou: true, connected: true, link: null, media: 'ready' as const, buffering: false, tracksInSync: false, chatMuted: false, permissions: EMPTY_ROOM.myPermissions, words: null, torrent: null, wait: null };
    const rs: RoomState = {
      ...EMPTY_ROOM,
      active: true,
      loadError: true,
      members: [me],
      torrent: { phase: 'noPeers', haveBytes: 0, size: 1, downBps: 0, upBps: 0, peers: 0, etaSecs: null, ranges: [], window: null, exclusive: true, trackers: 1, dht: false, webSeeds: 0, completeMessage: null, error: 'There is not enough free disk space for this file.' },
    };
    expect(roomErrors(rs, true)).toEqual(['unsupportedFile', 'trackUnresolved', 'noPeers', 'diskFull', 'firewall']);
    expect(roomErrors({ ...EMPTY_ROOM, active: true }, false)).toEqual([]);
  });

  it('the banner runs its one-click action', () => {
    const onAction = vi.fn();
    render(<ErrorBanner id="firewall" onAction={onAction} />);
    fireEvent.click(screen.getByRole('button', { name: 'Always use relay' }));
    expect(onAction).toHaveBeenCalledWith('alwaysRelay');
  });
});

vi.mock('../lib/room', async (orig) => {
  const m = await orig<typeof import('../lib/room')>();
  return { ...m, room: { ...m.room, create: () => Promise.reject(new Error('Could not reach the signaling server sw.example: connection refused')) } };
});

it('Create/Join: no server reachable offers Try again and Connection settings', async () => {
  const { RoomEntryDialog } = await import('../components/RoomDialogs');
  const onSettings = vi.fn();
  render(<RoomEntryDialog mode="create" onClose={() => {}} onEntered={() => {}} onConnectionSettings={onSettings} />);
  fireEvent.change(screen.getByLabelText(/name/i), { target: { value: 'Ana' } });
  fireEvent.submit(screen.getByRole('dialog').querySelector('form')!);
  await waitFor(() => expect(screen.getByTestId('error-noServer')).toBeTruthy());
  expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Connection settings...' }));
  expect(onSettings).toHaveBeenCalled();
});

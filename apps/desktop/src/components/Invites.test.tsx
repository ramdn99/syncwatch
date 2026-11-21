// Manual invites UI (ADR 0014): the host's Invites panel and the member's Join steps.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { InviteView } from '../lib/room';

const decide = vi.fn().mockResolvedValue(undefined);
const create = vi.fn().mockResolvedValue(undefined);
const reply = vi.fn().mockResolvedValue(undefined);
const invitePreview = vi.fn();
const joinInvite = vi.fn();
const roomStateNow = vi.fn();
const leave = vi.fn().mockResolvedValue(undefined);
const joinByCode = vi.fn();
// The room-state listener the Join dialog registers while it waits (the test drives it).
let pushState: ((s: import('../lib/room').RoomState) => void) | null = null;
vi.mock('../lib/room', async (orig) => {
  const real = await orig<typeof import('../lib/room')>();
  return {
    ...real,
    onRoomState: (cb: (s: import('../lib/room').RoomState) => void) => {
      pushState = cb;
      return () => {
        pushState = null;
      };
    },
    room: {
      ...real.room,
      invitePreview: (t: string) => invitePreview(t),
      joinInvite: (...a: unknown[]) => joinInvite(...a),
      state: () => roomStateNow(),
      leave: () => leave(),
      join: (...a: unknown[]) => joinByCode(...a),
      handshake: () => Promise.resolve('manual'),
      invite: { create, reply, decide },
    },
  };
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const inv = (over: Partial<InviteView>): InviteView => ({
  id: 'a'.repeat(32),
  status: 'waiting',
  text: 'SWINV1-abc',
  expiresUnix: Math.floor(Date.now() / 1000) + 600,
  memberName: null,
  words: null,
  forMember: null,
  reason: null,
  ...over,
});

describe('host: Invites panel', () => {
  it('makes invites, shows the text to copy and takes a pasted reply', async () => {
    const { InvitesPanel } = await import('./InvitesPanel');
    render(<InvitesPanel rs={{ invites: [inv({})], inviteError: null, members: [] }} />);
    fireEvent.click(screen.getByRole('button', { name: /Invite a member/ }));
    expect(create).toHaveBeenCalled();
    expect((screen.getByTestId(`invite-text-${'a'.repeat(32)}`) as HTMLTextAreaElement).value).toBe('SWINV1-abc');
    expect(screen.getByText('10 min left')).toBeTruthy();
    fireEvent.change(screen.getByTestId('reply-input'), { target: { value: 'SWREP1-xyz' } });
    fireEvent.click(screen.getByRole('button', { name: 'Add reply' }));
    expect(reply).toHaveBeenCalledWith('SWREP1-xyz');
  });

  it('shows who wants to join with the words, and admits or refuses', async () => {
    const { InvitesPanel } = await import('./InvitesPanel');
    const id = 'b'.repeat(32);
    render(
      <InvitesPanel
        rs={{ invites: [inv({ id, status: 'replied', memberName: 'Mo', words: ['cider', 'helmet', 'snow', 'carrot'] })], inviteError: null, members: [] }}
      />,
    );
    expect(screen.getByText('Mo')).toBeTruthy();
    expect(screen.getByTestId('invite-words').textContent).toContain('cider helmet snow carrot');
    fireEvent.click(screen.getByRole('button', { name: 'Admit' }));
    expect(decide).toHaveBeenCalledWith(id, 'admit');
    fireEvent.click(screen.getByRole('button', { name: 'Refuse' }));
    expect(decide).toHaveBeenCalledWith(id, 'refuse');
  });

  it('shows a refused reply as a friendly error, and re-issues an expired invite', async () => {
    const { InvitesPanel } = await import('./InvitesPanel');
    const id = 'c'.repeat(32);
    render(<InvitesPanel rs={{ invites: [inv({ id, status: 'expired', text: null })], inviteError: { seq: 1, message: 'This invite has expired.' }, members: [] }} />);
    expect(screen.getByTestId('invite-error').textContent).toContain('expired');
    fireEvent.click(screen.getByRole('button', { name: 'Re-issue' }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(null));
    expect(decide).toHaveBeenCalledWith(id, 'remove');
    // The invite text is never shown once it can no longer be used.
    expect(screen.queryByText('SWINV1-abc')).toBeNull();
  });
});

describe('member: joining with an invite', () => {
  it('previews a pasted invite and asks for the password only when the room has one', async () => {
    invitePreview.mockResolvedValueOnce({ roomName: 'Movie night', hostId: 'ab12cd', password: true, ttlMins: 5, hostVersion: '0.1.1', relayOnly: false, reinvite: false });
    const { InviteJoin } = await import('./InviteJoin');
    render(<InviteJoin name="Mo" onEntered={() => {}} onCancel={() => {}} onNameRequired={() => true} />);
    fireEvent.change(screen.getByTestId('invite-input'), { target: { value: 'SWINV1-abc' } });
    await waitFor(() => expect(screen.getByTestId('invite-preview').textContent).toContain('Movie night'));
    expect(screen.getByTestId('invite-preview').textContent).toContain('open for 5 minutes');
    expect(screen.getByTestId('invite-preview').textContent).toContain('SyncWatch 0.1.1');
    expect(screen.getByLabelText('Room password')).toBeTruthy();
  });

  it('a bad paste shows the reason and keeps the button off', async () => {
    invitePreview.mockRejectedValueOnce('This is a reply, not an invite. Paste it in the host’s window (Invites).');
    const { InviteJoin } = await import('./InviteJoin');
    render(<InviteJoin name="Mo" onEntered={() => {}} onCancel={() => {}} onNameRequired={() => true} />);
    fireEvent.change(screen.getByTestId('invite-input'), { target: { value: 'SWREP1-xyz' } });
    await waitFor(() => expect(screen.getByTestId('invite-preview-error').textContent).toContain('reply, not an invite'));
    expect((screen.getByRole('button', { name: 'Create my reply' }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe('member: waiting to be admitted (Milestone 9 fix)', () => {
  const words: [string, string, string, string] = ['badge', 'pillow', 'orchid', 'wagon'];
  async function toReplyStep() {
    const real = await vi.importActual<typeof import('../lib/room')>('../lib/room');
    const base = { ...real.EMPTY_ROOM } as import('../lib/room').RoomState;
    const waiting = { ...base, active: true, reply: { text: 'SWREP1-abc', words } };
    invitePreview.mockResolvedValueOnce({ roomName: 'Movie night', hostId: 'ab12cd', password: false, ttlMins: 10, hostVersion: '0.1.1', relayOnly: false, reinvite: false });
    joinInvite.mockResolvedValueOnce({ status: 'reply', passwordIgnored: false, ended: null });
    roomStateNow.mockResolvedValueOnce(waiting);
    const onCancel = vi.fn();
    const { InviteJoin } = await import('./InviteJoin');
    render(<InviteJoin name="Mo" onEntered={() => {}} onCancel={onCancel} onNameRequired={() => true} />);
    fireEvent.change(screen.getByTestId('invite-input'), { target: { value: 'SWINV2-xyz' } });
    await waitFor(() => expect((screen.getByRole('button', { name: 'Create my reply' }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('button', { name: 'Create my reply' }));
    await waitFor(() => expect(screen.getByTestId('invite-reply-step')).toBeTruthy());
    return { base, waiting, onCancel };
  }

  it('shows the reply, the four words and "waiting" with a Cancel button', async () => {
    await toReplyStep();
    expect(screen.getByTestId('invite-reply-step').textContent).toContain('badge');
    expect(screen.getByRole('status').textContent).toMatch(/waiting/i);
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeTruthy();
  });

  it('a room state without the reply (the attempt ended) keeps the step and shows why, never the paste step', async () => {
    const { base } = await toReplyStep();
    await waitFor(() => expect(pushState).not.toBeNull());
    pushState?.({ ...base, active: false, reply: null, ended: { kind: 'connectFailed', reason: 'every network check between you failed.' } });
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('The host could not connect: every network check'));
    expect(screen.getByTestId('invite-reply-step')).toBeTruthy();
    expect(screen.queryByTestId('invite-input')).toBeNull();
    expect(screen.getByRole('button', { name: 'Close' })).toBeTruthy();
  });

  it('an ended state without a reason still says the join failed', async () => {
    const { base } = await toReplyStep();
    await waitFor(() => expect(pushState).not.toBeNull());
    pushState?.({ ...base, active: false, reply: null, ended: null });
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(screen.queryByTestId('invite-input')).toBeNull();
  });
});

describe('the invite Join dialog never falls back to room codes', () => {
  it('Enter in "Your name" does not try a room-code join', async () => {
    const { RoomEntryDialog } = await import('./RoomDialogs');
    render(<RoomEntryDialog mode="join" onClose={() => {}} onEntered={() => {}} />);
    const nameBox = screen.getByLabelText('Your name');
    fireEvent.change(nameBox, { target: { value: 'Mo' } });
    fireEvent.submit(nameBox.closest('form') as HTMLFormElement);
    await new Promise((r) => setTimeout(r, 20));
    expect(joinByCode).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toMatch(/room codes? (have|has) 8 characters/i);
    expect(screen.queryByText('Room code')).toBeNull();
  });
});

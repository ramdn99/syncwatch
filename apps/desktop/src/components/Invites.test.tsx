// Manual invites UI (ADR 0014): the host's Invites panel and the member's Join steps.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { InviteView } from '../lib/room';

const decide = vi.fn().mockResolvedValue(undefined);
const create = vi.fn().mockResolvedValue(undefined);
const reply = vi.fn().mockResolvedValue(undefined);
const invitePreview = vi.fn();
vi.mock('../lib/room', async (orig) => {
  const real = await orig<typeof import('../lib/room')>();
  return {
    ...real,
    room: { ...real.room, invitePreview: (t: string) => invitePreview(t), invite: { create, reply, decide } },
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
    invitePreview.mockResolvedValueOnce({ roomName: 'Movie night', hostId: 'ab12cd', password: true, expiresUnix: Math.floor(Date.now() / 1000) + 300, reinvite: false });
    const { InviteJoin } = await import('./InviteJoin');
    render(<InviteJoin name="Mo" onEntered={() => {}} onCancel={() => {}} onNameRequired={() => true} />);
    fireEvent.change(screen.getByTestId('invite-input'), { target: { value: 'SWINV1-abc' } });
    await waitFor(() => expect(screen.getByTestId('invite-preview').textContent).toContain('Movie night'));
    expect(screen.getByTestId('invite-preview').textContent).toContain('5 more minutes');
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

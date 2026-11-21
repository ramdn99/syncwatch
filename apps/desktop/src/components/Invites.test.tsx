// Manual invites UI (ADR 0014): the host's Invites panel and the member's Join steps.
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
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
const submitPassword = vi.fn().mockResolvedValue(undefined);
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
      password: (p: string) => submitPassword(p),
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
  wrongAttempts: 0,
  maxAttempts: 6,
  lastWrongUnix: null,
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
    // No <form> wraps the invite Join dialog (a nested form swallowed the password retry), so
    // Enter in the name field cannot submit anything.
    expect(nameBox.closest('form')).toBeNull();
    fireEvent.keyDown(nameBox, { key: 'Enter' });
    await new Promise((r) => setTimeout(r, 20));
    expect(joinByCode).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toMatch(/room codes? (have|has) 8 characters/i);
    expect(screen.queryByText('Room code')).toBeNull();
  });
});

describe('wrong password with an invite (manual-test fix)', () => {
  const words: [string, string, string, string] = ['badge', 'pillow', 'orchid', 'wagon'];
  const np = (over: Partial<NonNullable<import('../lib/room').RoomState['needPassword']>> = {}) => ({
    wrong: true,
    retryAfterMs: 2000,
    attempts: 1,
    throttled: false,
    seq: 1,
    ...over,
  });

  it('countdown, the button re-enables by itself, Enter submits, never stuck busy', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: false });
    try {
      const onSubmit = vi.fn().mockResolvedValue(undefined);
      const { PasswordRetry } = await import('./PasswordRetry');
      const { rerender } = render(<PasswordRetry needPassword={np()} onSubmit={onSubmit} />);
      expect(screen.getByTestId('password-status').textContent).toBe('Wrong password. Try again in 2 seconds.');
      expect(screen.getByTestId('password-attempts').textContent).toContain('1 of 6');
      const field = screen.getByLabelText('Room password') as HTMLInputElement;
      expect(document.activeElement).toBe(field);
      fireEvent.change(field, { target: { value: 'again' } });
      const button = () => screen.getByRole('button') as HTMLButtonElement;
      expect(button().disabled).toBe(true);
      expect(button().textContent).toBe('Try again in 2 s');
      // Enter while throttled does nothing.
      fireEvent.submit(field.closest('form') as HTMLFormElement);
      expect(onSubmit).not.toHaveBeenCalled();
      await act(async () => {
        vi.advanceTimersByTime(2100);
      });
      expect(button().disabled).toBe(false);
      expect(button().textContent).toBe('Enter');
      expect(screen.getByTestId('password-status').textContent).toBe('Wrong password. Try again.');
      // Enter now works.
      fireEvent.submit(field.closest('form') as HTMLFormElement);
      expect(onSubmit).toHaveBeenCalledWith('again');
      expect(button().textContent).toBe('Checking...');
      // No answer yet: the button frees itself.
      await act(async () => {
        vi.advanceTimersByTime(1600);
      });
      expect(button().textContent).toBe('Enter');
      // The host's next prompt (seq 2, 4 s) restarts the countdown and clears the field.
      rerender(<PasswordRetry needPassword={np({ seq: 2, retryAfterMs: 4000, attempts: 2 })} onSubmit={onSubmit} />);
      expect(screen.getByTestId('password-status').textContent).toBe('Wrong password. Try again in 4 seconds.');
      expect(screen.getByTestId('password-attempts').textContent).toContain('2 of 6');
      expect((screen.getByLabelText('Room password') as HTMLInputElement).value).toBe('');
      // Too early: a throttle prompt says so.
      rerender(<PasswordRetry needPassword={np({ seq: 3, retryAfterMs: 3000, attempts: 2, wrong: false, throttled: true })} onSubmit={onSubmit} />);
      expect(screen.getByTestId('password-status').textContent).toBe('Too many tries in a short time. Try again in 3 seconds.');
    } finally {
      vi.useRealTimers();
    }
  });

  async function replyStep() {
    const real = await vi.importActual<typeof import('../lib/room')>('../lib/room');
    const base = { ...real.EMPTY_ROOM } as import('../lib/room').RoomState;
    invitePreview.mockResolvedValueOnce({ roomName: 'Movie night', hostId: 'ab12cd', password: true, ttlMins: 10, hostVersion: '0.1.0', relayOnly: false, reinvite: false });
    joinInvite.mockResolvedValueOnce({ status: 'reply', passwordIgnored: false, ended: null });
    roomStateNow.mockResolvedValueOnce({ ...base, active: true, reply: { text: 'SWREP2-abc', words } });
    const onCancel = vi.fn();
    const { InviteJoin } = await import('./InviteJoin');
    render(<InviteJoin name="Mo" onEntered={() => {}} onCancel={onCancel} onNameRequired={() => true} />);
    fireEvent.change(screen.getByTestId('invite-input'), { target: { value: 'SWINV2-xyz' } });
    await waitFor(() => expect(screen.getByLabelText('Room password')).toBeTruthy());
    fireEvent.change(screen.getByLabelText('Room password'), { target: { value: 'wrong' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create my reply' }));
    await waitFor(() => expect(screen.getByTestId('invite-reply-step')).toBeTruthy());
    await waitFor(() => expect(pushState).not.toBeNull());
    return { base, onCancel };
  }

  it('the Join dialog: wrong password, retry with Enter, then the room; Cancel works', async () => {
    const { base, onCancel } = await replyStep();
    pushState?.({ ...base, active: true, reply: { text: 'SWREP2-abc', words }, needPassword: np({ retryAfterMs: 0 }) });
    await waitFor(() => expect(screen.getByTestId('password-retry')).toBeTruthy());
    expect(screen.getByTestId('invite-reply-step').textContent).toContain('badge');
    const field = screen.getByLabelText('Room password');
    // The prompt clears and focuses the field when it appears: type after that, as a user would.
    await waitFor(() => expect(document.activeElement).toBe(field));
    fireEvent.change(field, { target: { value: 's3cret' } });
    fireEvent.submit(field.closest('form') as HTMLFormElement);
    await waitFor(() => expect(submitPassword).toHaveBeenCalledWith('s3cret'));
    expect(joinByCode).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(leave).toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalled();
  });

  it('after the ban the member reads what happened and what to do', async () => {
    const { base } = await replyStep();
    pushState?.({ ...base, active: false, reply: null, ended: { kind: 'authRejected', reason: 'TooManyAttempts' } });
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Too many wrong attempts'));
    expect(screen.getByRole('alert').textContent).toContain('10 minutes');
    expect(screen.getByRole('alert').textContent).toContain('fresh invite');
  });

  it('host: the admitted member shows the real state, then the ban with Re-issue', async () => {
    const real = await vi.importActual<typeof import('../lib/room')>('../lib/room');
    const { InvitesPanel } = await import('./InvitesPanel');
    const base = { ...real.EMPTY_ROOM, active: true, isHost: true } as import('../lib/room').RoomState;
    const { rerender } = render(<InvitesPanel rs={{ ...base, invites: [inv({ status: 'admitted', memberName: 'Mo', wrongAttempts: 2, lastWrongUnix: 1_800_000_000 })] }} />);
    expect(screen.getByTestId('invite-wrong-password').textContent).toContain('wrong password (attempt 2 of 6)');
    expect(screen.queryByText('Connecting...')).toBeNull();
    rerender(<InvitesPanel rs={{ ...base, invites: [inv({ status: 'failed', memberName: 'Mo', wrongAttempts: 6, reason: 'Blocked after too many wrong passwords (6). They can try again in 10 minutes, with a new invite.' })] }} />);
    expect(screen.getByTestId('invite-fail-reason').textContent).toContain('Blocked after too many wrong passwords');
    expect(screen.getByRole('button', { name: 'Re-issue' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Remove this invite' })).toBeTruthy();
  });
});

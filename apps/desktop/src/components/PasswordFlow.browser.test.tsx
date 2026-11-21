// Real-DOM test of the wrong-password retry (Milestone 9, two-machine finding): the actual Join
// dialog and the in-room prompt in a real browser engine, driven with real typing, the real
// Enter key and real clicks. Only the Tauri IPC boundary is mocked (Tauri's own mockIPC), so
// the app's room calls and room-state events run unchanged.
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { userEvent } from 'vitest/browser';
import { EMPTY_ROOM, type RoomState } from '../lib/room';
import { PasswordPrompt, RoomEntryDialog } from './RoomDialogs';

const words: [string, string, string, string] = ['tower', 'jaguar', 'fountain', 'ginger'];

/** A fake backend: answers the room commands and pushes room states like the real one. */
function fakeBackend(right: string) {
  let state: RoomState = { ...EMPTY_ROOM };
  let seq = 0;
  const tries: string[] = [];
  const push = (s: RoomState) => {
    state = s;
    void emit('room-state', s);
  };
  const wrong = (retryAfterMs: number) => {
    seq += 1;
    push({ ...state, active: true, reply: { text: 'SWREP1-abc', words }, needPassword: { wrong: true, retryAfterMs, attempts: seq, throttled: false, seq } });
  };
  mockIPC(
    (cmd, args) => {
      const a = (args ?? {}) as Record<string, unknown>;
      switch (cmd) {
        case 'room_handshake':
          return 'manual';
        case 'invite_preview':
          return { roomName: "Hana's room", hostId: 'ab12cd', password: true, ttlMins: 10, hostVersion: '0.1.0', relayOnly: false, reinvite: false };
        case 'room_join_invite':
          state = { ...state, active: true, reply: { text: 'SWREP1-abc', words } };
          // The host admits; the first password was wrong (1 s delay).
          setTimeout(() => wrong(1000), 50);
          return { status: 'reply', passwordIgnored: false, ended: null };
        case 'room_state':
          return state;
        case 'room_password':
          tries.push(String(a.password));
          if (a.password === right) setTimeout(() => push({ ...state, phase: 'inRoom', needPassword: null }), 50);
          else setTimeout(() => wrong(2000), 50);
          return null;
        case 'room_leave':
          push({ ...EMPTY_ROOM });
          return null;
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  return { tries };
}

afterEach(async () => {
  cleanup();
  // Unmounting unsubscribes from events through IPC: let that finish before removing the mock.
  await new Promise((r) => setTimeout(r, 50));
  clearMocks();
});

describe('wrong password retry, real browser events', () => {
  it('Join dialog: wrong, countdown, Enter key retry (wrong again), then the right one with a click', async () => {
    const be = fakeBackend('s3cret');
    const entered = vi.fn();
    render(<RoomEntryDialog mode="join" onClose={() => {}} onEntered={entered} />);
    await userEvent.fill(screen.getByLabelText('Your name'), 'Mo');
    await userEvent.fill(screen.getByTestId('invite-input'), 'SWINV1-xyz');
    const pw0 = await screen.findByLabelText('Room password');
    await userEvent.fill(pw0, 'nope');
    await userEvent.click(screen.getByRole('button', { name: 'Create my reply' }));
    // First refusal (1 s).
    await waitFor(() => expect(screen.getByTestId('password-status').textContent).toMatch(/^Wrong password\./));
    expect(screen.getByTestId('password-attempts').textContent).toContain('1 of 6');
    const field = screen.getByLabelText('Room password') as HTMLInputElement;
    await waitFor(() => expect(document.activeElement).toBe(field));
    await userEvent.type(field, 'nope2');
    expect(field.value).toBe('nope2');
    // The countdown ends and the button enables by itself.
    const button = () => screen.getByTestId('password-submit') as HTMLButtonElement;
    await waitFor(() => expect(button().disabled).toBe(false), { timeout: 3000 });
    // The real Enter key submits.
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(be.tries).toEqual(['nope2']));
    // Second refusal (2 s): countdown, attempts 2, the field is cleared and focused again.
    await waitFor(() => expect(screen.getByTestId('password-attempts').textContent).toContain('2 of 6'));
    const field2 = screen.getByLabelText('Room password') as HTMLInputElement;
    await waitFor(() => expect(field2.value).toBe(''));
    await waitFor(() => expect(document.activeElement).toBe(field2));
    await userEvent.type(field2, 's3cret');
    expect(button().disabled).toBe(true);
    await waitFor(() => expect(button().disabled).toBe(false), { timeout: 4000 });
    // A real click on the button.
    await userEvent.click(button());
    await waitFor(() => expect(be.tries).toEqual(['nope2', 's3cret']));
    await waitFor(() => expect(entered).toHaveBeenCalled());
  });

  it('in-room prompt: Enter key and click both submit, many times', async () => {
    const be = fakeBackend('s3cret');
    const { rerender } = render(
      <PasswordPrompt needPassword={{ wrong: true, retryAfterMs: 0, attempts: 1, throttled: false, seq: 1 }} onLeave={() => {}} />,
    );
    const field = screen.getByLabelText('Room password') as HTMLInputElement;
    await userEvent.type(field, 'a1');
    await userEvent.keyboard('{Enter}');
    await waitFor(() => expect(be.tries).toEqual(['a1']));
    rerender(<PasswordPrompt needPassword={{ wrong: true, retryAfterMs: 0, attempts: 2, throttled: false, seq: 2 }} onLeave={() => {}} />);
    await userEvent.type(screen.getByLabelText('Room password'), 'a2');
    await userEvent.click(screen.getByTestId('password-submit'));
    await waitFor(() => expect(be.tries).toEqual(['a1', 'a2']));
  });
});

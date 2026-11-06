import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { chatBlockedReason, chatStatus, EMPTY_ROOM, type RoomState } from '../lib/room';
import { ChatPanel } from './ChatPanel';
import { MyPermissions } from './HostControls';

afterEach(cleanup);

const state = (enabled: boolean, muted: boolean, isHost = false): RoomState => ({
  ...EMPTY_ROOM,
  active: true,
  isHost,
  chatState: { enabled, slowModeSecs: 0, muted },
});

const panel = (rs: RoomState) =>
  render(<ChatPanel messages={[]} onCollapse={() => {}} onChipClick={() => {}} onSend={() => {}} disabledReason={chatBlockedReason(rs)} />);

describe('chat input follows the host state', () => {
  it('on: enabled, no status line', () => {
    panel(state(true, false));
    const ta = screen.getByLabelText('Chat message') as HTMLTextAreaElement;
    expect(ta.disabled).toBe(false);
    expect(screen.queryByTestId('chat-input-status')).toBeNull();
  });

  it('muted: disabled with "You are muted by the host"', () => {
    panel(state(true, true));
    const ta = screen.getByLabelText('Chat message') as HTMLTextAreaElement;
    expect(ta.disabled).toBe(true);
    expect(ta.placeholder).toBe('You are muted by the host');
    expect(screen.getByTestId('chat-input-status').textContent).toBe('You are muted by the host');
    expect((screen.getByRole('button', { name: 'Send message' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('chat off: disabled with "Chat is turned off by the host"', () => {
    panel(state(false, false));
    const ta = screen.getByLabelText('Chat message') as HTMLTextAreaElement;
    expect(ta.disabled).toBe(true);
    expect(ta.placeholder).toBe('Chat is turned off by the host');
  });

  it('both: the muted message wins', () => {
    expect(chatBlockedReason(state(false, true))).toBe('You are muted by the host');
    expect(chatStatus(state(false, true))).toBe('muted');
  });

  it('the host can always send', () => {
    expect(chatBlockedReason(state(false, true, true))).toBeNull();
    expect(chatStatus(state(false, false, true))).toBe('on');
  });

  it('the status summary shows the chat state separately from permissions', () => {
    const perms = { canPlayPause: true, canSeek: true, canChangeMedia: false, canChangeSpeed: false };
    const { rerender } = render(<MyPermissions perms={perms} chat="muted" />);
    expect(screen.getByTestId('my-chat-status').textContent).toBe('Chat: muted');
    expect(screen.getAllByRole('listitem').map((l) => l.textContent)).toEqual(['play and pause', 'seek']);
    rerender(<MyPermissions perms={perms} chat="off" />);
    expect(screen.getByTestId('my-chat-status').textContent).toBe('Chat: off');
    rerender(<MyPermissions perms={perms} chat="on" />);
    expect(screen.getByTestId('my-chat-status').textContent).toBe('Chat: on');
  });
});

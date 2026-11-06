import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ChatPanel } from '../components/ChatPanel';
import { chatLines, chipText, markerSeconds, nearCurrent, spoilerUntil } from './chat';
import { EMPTY_ROOM, type RoomChat } from './room';

afterEach(cleanup);

const msg = (id: number, ms: number | null, epoch = 1): RoomChat => ({
  id,
  sender: 2,
  name: 'Mo',
  isHost: false,
  text: `m${id}`,
  wallMs: 1_700_000_000_000,
  mediaTimeMs: ms,
  mediaEpoch: epoch,
  divider: null,
});
const room = { ...EMPTY_ROOM, mediaEpoch: 1, isHost: false, myPermissions: { ...EMPTY_ROOM.myPermissions, canSeek: true } };

describe('chip text', () => {
  it('(f) mm:ss for short videos, h:mm:ss for videos of an hour or more', () => {
    expect(chipText(10_000, 1_700_000)).toBe('00:10');
    expect(chipText(330_000, 1_700_000)).toBe('05:30');
    expect(chipText(10_000, 3_800_000)).toBe('0:00:10');
    expect(chipText(3_723_000, 3_800_000)).toBe('1:02:03');
    expect(chipText(3_723_000, null)).toBe('1:02:03');
  });
});

describe('chat lines', () => {
  it('no media: no timestamp at all (never 00:00)', () => {
    const [l] = chatLines([msg(1, null)], room, null);
    expect(l!.chip).toBeNull();
    expect(l!.seekMs).toBeNull();
  });

  it('current media: seekable with a 2 s lead-in; older media: kept but not seekable', () => {
    const [cur, old] = chatLines([msg(1, 90_000, 1), msg(2, 50_000, 0)], room, 1_700_000);
    expect(cur!.chip).toBe('01:30');
    expect(cur!.seekMs).toBe(88_000);
    expect(old!.chip).toBe('00:50');
    expect(old!.stale).toBe(true);
    expect(old!.seekMs).toBeNull();
  });

  it('without seek permission chips are not seekable; the host always may', () => {
    const noSeek = { ...room, myPermissions: { ...room.myPermissions, canSeek: false } };
    expect(chatLines([msg(1, 5_000)], noSeek, null)[0]!.seekMs).toBeNull();
    expect(chatLines([msg(1, 5_000)], { ...noSeek, isHost: true }, null)[0]!.seekMs).toBe(3_000);
  });

  it('markers, near-current filter and spoiler guard', () => {
    const lines = chatLines([msg(1, 10_000), msg(2, 600_000), msg(3, null), msg(4, 20_000, 0)], room, 1_700_000);
    expect(markerSeconds(lines)).toEqual([10, 600]);
    expect(nearCurrent(lines, 60_000).map((l) => l.text)).toEqual(['m1', 'm3', 'm4']);
    expect(spoilerUntil(lines[1]!, 60_000)).toBe(600_000);
    expect(spoilerUntil(lines[0]!, 60_000)).toBeNull();
  });
});

describe('chat panel', () => {
  it('renders "no timestamp", seeks with the lead-in, and hides spoilers on request', () => {
    const onChip = vi.fn();
    const lines = chatLines([msg(1, null), msg(2, 90_000), msg(3, 900_000)], room, 1_700_000);
    render(<ChatPanel messages={lines} positionMs={120_000} onCollapse={() => {}} onChipClick={onChip} onSend={() => {}} />);
    expect(screen.getAllByTestId('chip-none')).toHaveLength(1);
    expect(screen.queryByText('00:00')).toBeNull();
    fireEvent.click(screen.getByText('01:30'));
    expect(onChip).toHaveBeenCalledWith(88);
    expect(screen.getByText('m3')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Spoiler guard' }));
    expect(screen.queryByText('m3')).toBeNull();
    expect(screen.getByText('Hidden until 15:00')).toBeTruthy();
  });
});

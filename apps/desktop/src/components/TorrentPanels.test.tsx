import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fmtEta, torrentStatusText, type RoomMember, type TorrentState } from '../lib/room';
import { BufferingOverlay, ConfirmMagnetDialog, TorrentBlock } from './TorrentPanels';
import { rangeSpans } from './VideoArea';

afterEach(cleanup);

const own = (over: Partial<TorrentState> = {}): TorrentState => ({
  phase: 'streaming',
  haveBytes: 420_000_000,
  size: 1_000_000_000,
  downBps: 2_500_000,
  upBps: 300_000,
  peers: 7,
  etaSecs: 232,
  ranges: [[0, 420_000_000]],
  buffering: null,
  exclusive: false,
  completeMessage: null,
  error: null,
  ...over,
});

const member = (id: number, name: string, torrent: RoomMember['torrent'], extra: Partial<RoomMember> = {}): RoomMember => ({
  id,
  name,
  isHost: id === 0,
  isYou: id === 0,
  connected: true,
  link: null,
  media: 'downloading',
  buffering: false,
  tracksInSync: true,
  chatMuted: false,
  permissions: { canPlayPause: true, canSeek: true, canChangeMedia: false, canChangeSpeed: false },
  words: null,
  torrent,
  ...extra,
});

describe('torrent status texts', () => {
  it('phases, percent and buffering', () => {
    expect(torrentStatusText(null)).toBeNull();
    expect(torrentStatusText({ phase: 'metadata', permille: 0 })).toBe('getting file list');
    expect(torrentStatusText({ phase: 'downloading', permille: 420 })).toBe('downloading 42%');
    expect(torrentStatusText({ phase: 'streaming', permille: 990 })).toBe('downloading 99%');
    expect(torrentStatusText({ phase: 'noPeers', permille: 100 })).toBe('waiting for peers 10%');
    expect(torrentStatusText({ phase: 'streaming', permille: 500 }, true)).toBe('buffering');
    expect(torrentStatusText({ phase: 'complete', permille: 1000 }, true)).toBe('complete');
    expect(fmtEta(232)).toBe('3 min');
  });
});

describe('TorrentBlock', () => {
  it('shows my speeds, peers and every member for the host', () => {
    render(
      <TorrentBlock
        torrent={own()}
        isHost
        members={[
          member(0, 'Hana', { phase: 'streaming', permille: 420 }),
          member(1, 'Ben', { phase: 'downloading', permille: 130 }),
          member(2, 'Cy', { phase: 'streaming', permille: 300 }, { buffering: true }),
          member(3, 'Di', { phase: 'complete', permille: 1000 }),
          member(4, 'Ed', { phase: 'noPeers', permille: 50 }),
          member(5, 'Flo', null, { media: 'declined' }),
        ]}
      />,
    );
    expect(screen.getByTestId('torrent-phase').textContent).toBe('downloading 42%');
    expect(screen.getByTestId('torrent-down').textContent).toBe('2.5 MB/s');
    expect(screen.getByTestId('torrent-up').textContent).toBe('300 KB/s');
    expect(screen.getByTestId('torrent-peers').textContent).toBe('7');
    expect(screen.getAllByTestId('torrent-member-status').map((e) => e.textContent)).toEqual([
      'downloading 13%',
      'buffering',
      'complete',
      'waiting for peers 5%',
      'declined',
    ]);
  });

  it('shows the exact completion message and no speeds when complete', () => {
    render(<TorrentBlock torrent={own({ phase: 'complete', haveBytes: 1e9, completeMessage: 'Downloading and sharing have stopped. No further traffic.' })} isHost={false} members={[]} />);
    expect(screen.getByTestId('torrent-complete').textContent).toBe('Downloading and sharing have stopped. No further traffic.');
    expect(screen.queryByTestId('torrent-down')).toBeNull();
    expect(screen.queryByTestId('torrent-members')).toBeNull();
  });
});

describe('member magnet confirmation', () => {
  it('shows name and size and needs a choice', () => {
    const accept = vi.fn();
    const decline = vi.fn();
    render(<ConfirmMagnetDialog name="Sintel.mkv" size={1_200_000_000} onAccept={accept} onDecline={decline} />);
    expect(screen.getByTestId('magnet-name').textContent).toBe('Sintel.mkv');
    expect(screen.getByText('(1.20 GB)')).toBeTruthy();
    screen.getByRole('button', { name: 'Decline' }).click();
    expect(decline).toHaveBeenCalledOnce();
    screen.getByRole('button', { name: 'Download and play' }).click();
    expect(accept).toHaveBeenCalledOnce();
  });
});

describe('seek bar and buffering', () => {
  it('maps downloaded byte ranges to the bar', () => {
    expect(rangeSpans(null)).toEqual([]);
    expect(rangeSpans({ ranges: [[0, 250], [500, 1000]], size: 1000 })).toEqual([
      { left: 0, width: 25 },
      { left: 50, width: 50 },
    ]);
  });

  it('shows the buffering target and progress', () => {
    render(<BufferingOverlay buffering={{ have: 1_000_000, total: 4_000_000 }} />);
    expect(screen.getByTestId('torrent-buffering').textContent).toContain('Buffering 25%');
    expect(screen.getByTestId('torrent-buffering').textContent).toContain('1.0 MB of 4.0 MB');
  });
});

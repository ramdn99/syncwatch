import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { fmtEta, gateBannerText, torrentStatusText, type GateWait, type RoomMember, type TorrentState } from '../lib/room';
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
  window: null,
  exclusive: false,
  completeMessage: null,
  error: null,
  trackers: 2,
  dht: true,
  webSeeds: 0,
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
  wait: null,
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
    // Overall progress never reads as buffering: only the member's real need at the room position.
    expect(torrentStatusText({ phase: 'streaming', permille: 400 }, false)).toBe('downloading 40%');
    expect(torrentStatusText({ phase: 'streaming', permille: 400 }, true, { atMs: 750_000, missing: 3e6, permille: 600 })).toBe(
      'buffering at 12:30, 60% of the window',
    );
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
          member(2, 'Cy', { phase: 'streaming', permille: 300 }, { buffering: true, wait: { atMs: 750_000, missing: 3e6, permille: 600 } }),
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
      'buffering at 12:30, 60% of the window',
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
    render(<BufferingOverlay wait={{ atMs: 750_000, missing: 3_000_000, permille: 250 }} />);
    expect(screen.getByTestId('torrent-buffering').textContent).toContain('Buffering 25%');
    expect(screen.getByTestId('torrent-buffering').textContent).toContain('At 12:30: needs ~3.0 MB more');
  });

  it('a stall without a torrent window just says buffering', () => {
    render(<BufferingOverlay wait={null} />);
    expect(screen.getByTestId('torrent-buffering').textContent).toBe('Buffering...');
  });

  it("my own status shows 'buffering' only for my real stall, never for overall progress", () => {
    const { rerender } = render(<TorrentBlock torrent={own()} isHost={false} members={[]} stall={{ stalled: false, wait: null }} />);
    expect(screen.getByTestId('torrent-phase').textContent).toBe('downloading 42%');
    rerender(<TorrentBlock torrent={own()} isHost={false} members={[]} stall={{ stalled: true, wait: { atMs: 60_000, missing: 1e6, permille: 500 } }} />);
    expect(screen.getByTestId('torrent-phase').textContent).toBe('buffering at 01:00, 50% of the window');
  });
});

describe('ready-gate banner (ADR 0012)', () => {
  const bo: GateWait = { member: 2, name: 'Bo', reason: 'buffering', wait: { atMs: 750_000, missing: 3_000_000, permille: 600 } };
  const cy: GateWait = { member: 3, name: 'Cy', reason: 'loading', wait: null };
  const gate = (waiting: GateWait[], active = true) => ({ active, waiting });

  it('the host sees the reason and the data', () => {
    expect(gateBannerText(gate([bo, cy]), 0, true)).toBe('Waiting for Bo: buffering at 12:30 (needs ~3.0 MB more), Cy (loading)...');
    expect(gateBannerText(gate([]), 0, true)).toBe('Waiting for everyone...');
    expect(gateBannerText(gate([bo], false), 0, true)).toBeNull();
  });

  it("another member's buffering is a small 'catch up' message, never the popup text", () => {
    expect(gateBannerText(gate([bo]), 1, false)).toBe('Waiting for Bo to catch up...');
    expect(gateBannerText(gate([bo, cy]), 1, false)).toBe('Waiting for Bo to catch up, Cy (loading)...');
  });

  it('my own buffering is my popup, not the banner', () => {
    expect(gateBannerText(gate([bo]), 2, false)).toBeNull();
    expect(gateBannerText(gate([bo, cy]), 2, false)).toBe('Waiting for Cy (loading)...');
  });
});

describe('clear status instead of a silent stall', () => {
  it('waiting for the host, looking for peers, no peers (with and without web seeds)', () => {
    const { rerender } = render(<TorrentBlock torrent={own({ phase: 'waitingForHost', haveBytes: 0 })} isHost={false} members={[]} />);
    expect(screen.getByTestId('torrent-look').textContent).toBe('Waiting for the host to get the file list');
    rerender(<TorrentBlock torrent={own({ phase: 'metadata', haveBytes: 0, dht: false, trackers: 1 })} isHost={false} members={[]} />);
    expect(screen.getByTestId('torrent-look').textContent).toBe('Looking for peers (DHT off, 1 tracker)');
    rerender(<TorrentBlock torrent={own({ phase: 'noPeers', peers: 0 })} isHost={false} members={[]} />);
    expect(screen.getByTestId('torrent-look').textContent).toContain('no peers are sharing it yet');
    rerender(<TorrentBlock torrent={own({ phase: 'noPeers', peers: 0, webSeeds: 2 })} isHost={false} members={[]} />);
    expect(screen.getByTestId('torrent-look').textContent).toContain('web seeds');
  });

  it('the host sees a member waiting for its file list', () => {
    render(<TorrentBlock torrent={null} isHost members={[member(1, 'Ben', { phase: 'waitingForHost', permille: 0 })]} />);
    expect(screen.getByTestId('torrent-member-status').textContent).toBe("waiting for the host's file list");
  });

  it('the confirmation names the torrent and the chosen file', () => {
    render(<ConfirmMagnetDialog name="long.mkv" size={77_000_000} torrentName="SyncWatch Lab Pack" onAccept={() => {}} onDecline={() => {}} />);
    expect(screen.getByTestId('magnet-torrent-name').textContent).toBe('SyncWatch Lab Pack');
    expect(screen.getByTestId('magnet-name').textContent).toBe('long.mkv');
  });
});

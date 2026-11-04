import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Track } from '../lib/player';
import { TrackList } from './TrackList';

afterEach(cleanup);

const t = (id: number, kind: Track['kind'], extra: Partial<Track> = {}): Track => ({
  id, kind, ffIndex: id, lang: null, title: null, codec: null, selected: false, external: false, default: false, forced: false, channels: null, ...extra,
});

const tracks: Track[] = [
  t(1, 'audio', { lang: 'jpn', codec: 'eac3', selected: true }),
  t(2, 'audio', { lang: 'eng', codec: 'eac3' }),
  t(1, 'sub', { lang: 'eng', codec: 'subrip' }),
  t(2, 'sub', { lang: 'eng', title: 'SDH', codec: 'subrip' }),
];

describe('TrackList', () => {
  it('is collapsed by default and the header shows title, selection and count', () => {
    render(<TrackList kind="audio" tracks={tracks} isHost mediaKey="a" onSelect={() => {}} onAdd={() => {}} />);
    const header = screen.getByRole('button', { expanded: false });
    expect(header).toHaveProperty('textContent');
    expect(header.textContent).toContain('Audio');
    expect(header.textContent).toContain('jpn · eac3');
    expect(header.textContent).toContain('2');
    expect(screen.queryByRole('listbox')).toBeNull();
  });

  it('host: first item is "Add ... file..." and "+" works while collapsed', () => {
    const onAdd = vi.fn();
    render(<TrackList kind="sub" tracks={tracks} isHost mediaKey="a" onSelect={() => {}} onAdd={onAdd} />);
    fireEvent.click(screen.getByRole('button', { name: 'Add subtitle file...' }));
    expect(onAdd).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole('button', { expanded: false }));
    const list = screen.getByRole('listbox');
    const first = within(list).getAllByRole('button')[0];
    expect(first?.textContent).toBe('Add subtitle file...');
    expect(within(list).getByText('Off')).toBeTruthy();
  });

  it('collapses again when a new video loads', () => {
    const { rerender } = render(<TrackList kind="audio" tracks={tracks} isHost mediaKey="a" onSelect={() => {}} onAdd={() => {}} />);
    fireEvent.click(screen.getByRole('button', { expanded: false }));
    expect(screen.getByRole('listbox')).toBeTruthy();
    rerender(<TrackList kind="audio" tracks={tracks} isHost mediaKey="b" onSelect={() => {}} onAdd={() => {}} />);
    expect(screen.queryByRole('listbox')).toBeNull();
  });

  it('member: read-only, no add actions, lock tooltip, selection disabled', () => {
    const onSelect = vi.fn();
    render(<TrackList kind="sub" tracks={tracks} isHost={false} mediaKey="a" onSelect={onSelect} onAdd={() => {}} />);
    expect(screen.queryByRole('button', { name: 'Add subtitle file...' })).toBeNull();
    const header = screen.getByRole('button', { expanded: false });
    expect(header.getAttribute('title')).toBe('Only the host can change tracks');
    fireEvent.click(header);
    const rows = within(screen.getByRole('listbox')).getAllByRole('button');
    expect(rows.every((r) => (r as HTMLButtonElement).disabled)).toBe(true);
    expect(rows[0]?.textContent).not.toContain('Add');
    fireEvent.click(rows[1]!);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('host selecting a track reports the mpv id', () => {
    const onSelect = vi.fn();
    render(<TrackList kind="sub" tracks={tracks} isHost mediaKey="a" onSelect={onSelect} onAdd={() => {}} />);
    fireEvent.click(screen.getByRole('button', { expanded: false }));
    fireEvent.click(screen.getByText('eng · SDH · subrip'));
    expect(onSelect).toHaveBeenCalledWith({ mode: 'id', id: 2 });
    fireEvent.click(within(screen.getByRole('listbox')).getByText('Off'));
    expect(onSelect).toHaveBeenLastCalledWith({ mode: 'off' });
  });
});

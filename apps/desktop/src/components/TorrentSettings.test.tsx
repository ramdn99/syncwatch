import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TorrentSettings } from '../lib/room';
import { NumberSetting, rangeError, TORRENT_DEFAULTS, TORRENT_RANGES } from './TorrentSettings';

afterEach(cleanup);

describe('number settings (Files / Network)', () => {
  it('shows the default, refuses out-of-range values and resets', () => {
    const onSave = vi.fn();
    render(<NumberSetting label="After the position" unit="seconds" value={9} def={5} range={[2, 60]} testId="w" onSave={onSave} />);
    const input = screen.getByLabelText('After the position') as HTMLInputElement;
    expect(screen.getByTestId('w').textContent).toContain('Default: 5 seconds.');
    fireEvent.change(input, { target: { value: '61' } });
    fireEvent.blur(input);
    expect(onSave).not.toHaveBeenCalled();
    expect(screen.getByRole('alert').textContent).toContain('Allowed: 2 to 60 seconds.');
    expect(input.getAttribute('aria-invalid')).toBe('true');
    fireEvent.change(input, { target: { value: '12' } });
    fireEvent.blur(input);
    expect(onSave).toHaveBeenLastCalledWith(12);
    fireEvent.click(screen.getByRole('button', { name: 'Reset to default' }));
    expect(onSave).toHaveBeenLastCalledWith(5);
    expect(input.value).toBe('5');
  });

  it('ranges and defaults match the app side', () => {
    expect(rangeError(5, TORRENT_RANGES.metaCapMb, 'MB')).toBeNull();
    expect(rangeError(0, TORRENT_RANGES.metaCapMb, 'MB')).not.toBeNull();
    expect(rangeError(1.5, TORRENT_RANGES.windowAfterSecs, 's')).toBe('Enter a whole number.');
    const d: Pick<TorrentSettings, 'metaCapMb' | 'windowBeforeSecs' | 'windowAfterSecs' | 'startupKb'> = TORRENT_DEFAULTS;
    expect(d).toEqual({ metaCapMb: 5, windowBeforeSecs: 1, windowAfterSecs: 5, startupKb: 1024 });
  });
});

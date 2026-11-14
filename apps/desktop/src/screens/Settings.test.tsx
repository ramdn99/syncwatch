import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { EMPTY_STATUS } from '../lib/connection';
import { SECTIONS, Settings } from './Settings';

afterEach(cleanup);

const status = { ...EMPTY_STATUS, configured: true, verified: true, hostnames: ['<app-name>.metered.live'] };

describe('Settings screen (Milestone 8)', () => {
  it('has every section, each with its controls', async () => {
    render(<Settings status={status} onReplace={() => {}} onBack={() => {}} />);
    expect(SECTIONS).toEqual(['Connection', 'Playback', 'Room defaults', 'Chat', 'Files', 'Network', 'About']);
    const go = (s: string) => fireEvent.click(screen.getByRole('button', { name: s }));
    go('Playback');
    await waitFor(() => expect(screen.getByLabelText('Subtitle size')).toBeTruthy());
    expect(screen.getByText('Hardware decoding')).toBeTruthy();
    expect(screen.getByText(/resumes once 2 seconds are buffered/)).toBeTruthy();
    go('Room defaults');
    expect(screen.getByText('Only me')).toBeTruthy();
    expect(screen.getByLabelText('Wait for members')).toBeTruthy();
    go('Chat');
    expect(screen.getByText('up to 500 characters (4 KB)')).toBeTruthy();
    expect((screen.getByRole('button', { name: "Export this room's chat..." }) as HTMLButtonElement).disabled).toBe(true);
    go('Files');
    expect(screen.getByText('No limit')).toBeTruthy();
    expect(screen.getByText('up to 1 GB')).toBeTruthy();
    go('Network');
    expect(screen.getByRole('checkbox')).toBeTruthy();
    go('About');
    expect(screen.getByText(/GNU General Public License/)).toBeTruthy();
    expect(screen.getByText('Lucide icons')).toBeTruthy();
    expect(screen.getByAltText('SyncWatch')).toBeTruthy();
  });

  it('Copy diagnostics puts only redacted text on the clipboard', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.assign(navigator, { clipboard: { writeText } });
    render(<Settings status={status} onReplace={() => {}} onBack={() => {}} initial="About" />);
    fireEvent.click(screen.getByRole('button', { name: 'Copy diagnostics' }));
    await waitFor(() => expect(writeText).toHaveBeenCalled());
    const text = (writeText.mock.calls[0] as unknown as [string])[0];
    expect(text).toContain('"verified": true');
    expect(text).not.toMatch(/apiKey=|"credential"\s*:\s*"[^<]/);
    expect(screen.getByRole('status').textContent).toContain('IP addresses removed');
  });
});

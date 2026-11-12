import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const info = {
  infohash: 'c16c2a8dcadb8d4a060651fed1a85107f7b7677b',
  name: 'SyncWatch Lab Pack',
  totalSize: 77_448_448,
  trackers: 1,
  webSeeds: 0,
  files: [
    { index: 1, path: 'SyncWatch Lab Pack/long.mkv', size: 77_424_675, video: true },
    { index: 0, path: 'SyncWatch Lab Pack/extras/trailer.mkv', size: 23_725, video: true },
    { index: 2, path: 'SyncWatch Lab Pack/readme.nfo', size: 50, video: false },
  ],
};
const calls: { cmd: string; args: unknown }[] = [];
vi.mock('../lib/player', async (orig) => ({ ...(await orig<typeof import('../lib/player')>()), inTauri: () => true }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args: unknown) => {
    calls.push({ cmd, args });
    if (cmd === 'torrent_settings_get') return Promise.resolve({ noticeAccepted: true });
    if (cmd === 'torrent_inspect_file') return Promise.resolve(info);
    return Promise.resolve(null);
  },
}));
vi.mock('@tauri-apps/api/webview', () => ({ getCurrentWebview: () => ({ onDragDropEvent: () => Promise.resolve(() => {}) }) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: () => Promise.resolve('C:\lab\lab.torrent') }));

import { ChangeSourceDialog } from './ChangeSourceDialog';

afterEach(cleanup);

describe('Change source: torrent file', () => {
  it('shows name, infohash, size and files at once, and the host picks the file', async () => {
    const chosen = vi.fn();
    render(<ChangeSourceDialog inRoom onClose={() => {}} onLocalChosen={() => {}} onTorrentFileChosen={chosen} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Torrent' }));
    expect(screen.getByLabelText('Magnet link')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Choose .torrent...' }));
    await waitFor(() => expect(screen.getByTestId('magnet-files')).toBeTruthy());
    expect(calls.some((c) => c.cmd === 'torrent_inspect_file')).toBe(true);
    expect(calls.some((c) => c.cmd === 'torrent_metadata')).toBe(false);
    expect(screen.getByTestId('torrent-summary').textContent).toContain('c16c2a8dcadb8d4a060651fed1a85107f7b7677b');
    expect(screen.getByTestId('torrent-summary').textContent).toContain('77.4 MB');
    const radios = screen.getAllByRole('radio') as HTMLInputElement[];
    expect(radios.map((r) => r.disabled)).toEqual([false, false, true]);
    expect(radios[0]!.checked).toBe(true);
    fireEvent.click(radios[1]!);
    fireEvent.click(screen.getByRole('button', { name: 'Change source' }));
    expect(chosen).toHaveBeenCalledWith(info.infohash, 0);
  });

  it('warns about web seeds', async () => {
    info.webSeeds = 2;
    render(<ChangeSourceDialog inRoom onClose={() => {}} onLocalChosen={() => {}} />);
    fireEvent.click(screen.getByRole('tab', { name: 'Torrent' }));
    fireEvent.click(screen.getByRole('button', { name: 'Choose .torrent...' }));
    await waitFor(() => expect(screen.getByText(/web seeds/)).toBeTruthy());
  });
});

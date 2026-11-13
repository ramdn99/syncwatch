import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { parseMagnet } from '../lib/magnet';
import { inTauri } from '../lib/player';
import { fmtBytes, TORRENT_NOTICE, torrents, type TorrentInfo } from '../lib/room';

// Change source (spec section 6): local file OR torrent magnet, in either direction, without
// leaving the room. The Rust parser is authoritative; the TS mirror gives instant feedback.

interface Prepared {
  kind: 'local' | 'magnet';
  title: string;
  size: number | null;
  infohash: string | null;
  frameBytes: number;
  torrentEngineAvailable: boolean;
}

const MAGNET_ERRORS: Record<string, string> = {
  NotMagnet: 'This is not a magnet link. It must start with magnet:?',
  TooLong: 'This magnet link is too long.',
  NoInfohash: 'This magnet link has no BitTorrent infohash (xt=urn:btih:...).',
  BadInfohash: 'The infohash in this magnet link is not valid.',
  ConflictingInfohash: 'This magnet link contains two different infohashes.',
  V2Only: 'BitTorrent v2-only magnet links are not supported.',
  BadEncoding: 'This magnet link contains broken characters.',
};

const fmtSize = (n: number | null) => (n == null ? '' : n >= 1e9 ? `${(n / 1e9).toFixed(2)} GB` : `${(n / 1e6).toFixed(1)} MB`);

export function ChangeSourceDialog(props: {
  initialPath?: string;
  onClose: () => void;
  onLocalChosen: (path: string) => void;
  /** In a room: torrents are possible (they always play in a room). */
  inRoom?: boolean;
  /** Host picked file `index` of a magnet. */
  onTorrentChosen?: (magnet: string, file: { index: number; path: string; size: number }) => void;
  /** Host picked file `index` of an inspected .torrent file. */
  onTorrentFileChosen?: (infohash: string, fileIndex: number) => void;
}) {
  const [tab, setTab] = useState<'local' | 'magnet'>('local');
  const [path, setPath] = useState<string | null>(props.initialPath ?? null);
  const [magnet, setMagnet] = useState('');
  const [prepared, setPrepared] = useState<Prepared | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [info, setInfo] = useState<TorrentInfo | null>(null);
  const [fileIndex, setFileIndex] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [noticeOk, setNoticeOk] = useState(false);
  const [noticeSaved, setNoticeSaved] = useState(false);
  /** Where the torrent's file list came from. */
  const [torrentSource, setTorrentSource] = useState<'magnet' | 'file' | null>(null);
  const [torrentPath, setTorrentPath] = useState<string | null>(null);

  // A .torrent file: read and validated locally, the file list shows at once (no network).
  const openTorrentFile = async (p: string) => {
    setBusy(true);
    setError(null);
    setInfo(null);
    setFileIndex(null);
    try {
      const i = await torrents.inspectFile(p);
      setInfo(i);
      setTorrentSource('file');
      setTorrentPath(p);
      setFileIndex(i.files.find((f) => f.video)?.index ?? null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const pickTorrentFile = async () => {
    if (!inTauri()) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const p = await open({ multiple: false, directory: false, filters: [{ name: 'Torrent file', extensions: ['torrent'] }] });
    if (typeof p === 'string') void openTorrentFile(p);
  };

  useEffect(() => {
    if (!inTauri()) return;
    void torrents
      .settings()
      .then((s) => setNoticeSaved(s.noticeAccepted))
      .catch(() => {});
  }, []);

  const mirror = magnet.trim() ? parseMagnet(magnet) : null;

  // Metadata progress: the file list comes from peers (can take a while, or never).
  const fetchFiles = async () => {
    setBusy(true);
    setError(null);
    setInfo(null);
    setFileIndex(null);
    setElapsed(0);
    const t0 = Date.now();
    const timer = window.setInterval(() => setElapsed(Math.floor((Date.now() - t0) / 1000)), 500);
    try {
      const i = await torrents.metadata(magnet.trim());
      setInfo(i);
      setTorrentSource('magnet');
      setFileIndex(i.files.find((f) => f.video)?.index ?? null);
    } catch (e) {
      setError(String(e));
    } finally {
      window.clearInterval(timer);
      setBusy(false);
    }
  };

  const chosenFile = info?.files.find((f) => f.index === fileIndex) ?? null;
  const canUseTorrent = chosenFile != null && (noticeSaved || noticeOk);

  const prepare = async (source: { kind: 'local'; path: string } | { kind: 'magnet'; text: string }) => {
    setBusy(true);
    setError(null);
    setPrepared(null);
    try {
      if (!inTauri()) return;
      setPrepared(await invoke<Prepared>('prepare_change_media', { source }));
    } catch (e) {
      const code = String(e);
      setError(MAGNET_ERRORS[code.charAt(0).toUpperCase() + code.slice(1)] ?? code);
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (path) void prepare({ kind: 'local', path });
  }, [path]);

  const pick = async () => {
    if (!inTauri()) return;
    const { open } = await import('@tauri-apps/plugin-dialog');
    const p = await open({ multiple: false, directory: false });
    if (typeof p === 'string') setPath(p);
  };

  // Drag-and-drop onto the dialog.
  useEffect(() => {
    if (!inTauri()) return;
    let un: (() => void) | undefined;
    let gone = false;
    void (async () => {
      try {
        const { getCurrentWebview } = await import('@tauri-apps/api/webview');
        const u = await getCurrentWebview().onDragDropEvent((ev) => {
          const p = ev.payload.type === 'drop' ? ev.payload.paths[0] : undefined;
          if (!p) return;
          if (tab === 'magnet' && p.toLowerCase().endsWith('.torrent')) void openTorrentFile(p);
          else if (tab === 'local') setPath(p);
        });
        if (gone) u();
        else un = u;
      } catch {
        // No webview (tests, or the window is closing): the file picker still works.
      }
    })();
    return () => {
      gone = true;
      un?.();
    };
  }, [tab]);

  const confirm = () => {
    if (tab === 'magnet') {
      if (!chosenFile || !canUseTorrent) return;
      if (!noticeSaved) {
        void torrents
          .settings()
          .then((s) => torrents.saveSettings({ ...s, noticeAccepted: true }))
          .catch(() => {});
      }
      if (torrentSource === 'file' && info) props.onTorrentFileChosen?.(info.infohash, chosenFile.index);
      else props.onTorrentChosen?.(magnet.trim(), chosenFile);
      props.onClose();
      return;
    }
    if (prepared?.kind === 'local' && path) {
      props.onLocalChosen(path);
      props.onClose();
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4" role="dialog" aria-modal="true" aria-label="Change source">
      <div className="w-full max-w-lg rounded-xl border border-border bg-elevated p-5 shadow-2xl">
        <div className="flex items-center justify-between">
          <h2 className="font-brand text-lg font-bold">Change source</h2>
          <button type="button" aria-label="Close" className="rounded px-2 text-muted hover:text-text" onClick={props.onClose}>
            ✕
          </button>
        </div>
        <div className="mt-3 flex gap-1 border-b border-border" role="tablist">
          {(
            [
              ['local', 'Local file'],
              ['magnet', 'Torrent'],
            ] as const
          ).map(([t, label]) => (
            <button
              key={t}
              type="button"
              role="tab"
              aria-selected={tab === t}
              className={`-mb-px border-b-2 px-3 py-1.5 text-sm ${tab === t ? 'border-accent' : 'border-transparent text-muted hover:text-text'}`}
              onClick={() => {
                setTab(t);
                setPrepared(null);
                setInfo(null);
                setError(null);
              }}
            >
              {label}
            </button>
          ))}
        </div>

        {tab === 'local' ? (
          <div className="mt-4">
            <div className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-sm text-muted">
              Drop a video file here, or
              <button type="button" className="ml-2 rounded-md bg-surface px-3 py-1 text-text hover:bg-border" onClick={() => void pick()}>
                Choose file...
              </button>
            </div>
            {busy && <p className="mt-2 text-sm text-muted">Checking the file...</p>}
          </div>
        ) : (
          <div className="mt-4 space-y-2">
            <label className="block text-sm text-muted" htmlFor="magnet-input">
              Magnet link
            </label>
            <textarea
              id="magnet-input"
              className="h-24 w-full rounded-md border border-border bg-surface px-3 py-2 font-mono text-xs text-text focus:border-accent focus:outline-none"
              placeholder="magnet:?xt=urn:btih:..."
              spellCheck={false}
              value={magnet}
              onChange={(e) => {
                setMagnet(e.target.value);
                setTorrentSource(null);
                setPrepared(null);
                setInfo(null);
                setError(null);
              }}
            />
            {mirror && !mirror.ok && <p className="text-sm text-danger">{MAGNET_ERRORS[mirror.error]}</p>}
            {mirror?.ok && (
              <div className="text-sm">
                <p>
                  <span className="text-muted">Name:</span> {mirror.magnet.dn ?? <span className="text-muted">(not given)</span>}
                </p>
                <p className="font-mono text-xs text-muted">infohash {mirror.magnet.infohash}</p>
                {props.inRoom ? (
                  <button type="button" disabled={busy} className="mt-2 rounded-md bg-surface px-3 py-1 hover:bg-border" onClick={() => void fetchFiles()}>
                    Get file list
                  </button>
                ) : (
                  <p className="mt-2 text-warning">Torrents play in a room. Create or join a room first.</p>
                )}
              </div>
            )}
            <div className="space-y-1">
              <p className="text-sm text-muted">Torrent file</p>
              <div className="rounded-lg border border-dashed border-border px-4 py-3 text-center text-sm text-muted" data-testid="torrent-file-drop">
                Drop a .torrent file here, or
                <button type="button" disabled={!props.inRoom} className="ml-2 rounded-md bg-surface px-3 py-1 text-text hover:bg-border disabled:opacity-40" onClick={() => void pickTorrentFile()}>
                  Choose .torrent...
                </button>
                {torrentPath && torrentSource === 'file' && <p className="mt-1 truncate text-xs" title={torrentPath}>{torrentPath.split(/[\\/]/).pop()}</p>}
              </div>
            </div>
            {busy && torrentSource !== 'file' && magnet.trim() && (
              <p className="text-sm text-muted" role="status" data-testid="magnet-progress">
                Getting the file list from peers... {elapsed} s
              </p>
            )}
            {info && (
              <div className="space-y-1" data-testid="magnet-files">
                <p className="text-sm font-medium">{info.name}</p>
                <p className="font-mono text-xs text-muted" data-testid="torrent-summary">
                  infohash {info.infohash} · {fmtBytes(info.totalSize)} · {info.files.length} file{info.files.length === 1 ? '' : 's'} · {info.trackers} tracker
                  {info.trackers === 1 ? '' : 's'}
                </p>
                {info.webSeeds > 0 && (
                  <p className="text-xs text-warning">This torrent lists web seeds (HTTP downloads). SyncWatch cannot use them: it needs peers sharing the torrent.</p>
                )}
                <ul className="max-h-48 space-y-0.5 overflow-y-auto rounded-md border border-border p-1 text-sm" role="radiogroup" aria-label="Files in the torrent">
                  {info.files.map((f) => (
                    <li key={f.index}>
                      <label className={`flex items-center gap-2 rounded px-2 py-1 ${f.video ? 'hover:bg-surface' : 'text-muted'}`}>
                        <input
                          type="radio"
                          name="torrent-file"
                          disabled={!f.video}
                          checked={fileIndex === f.index}
                          onChange={() => setFileIndex(f.index)}
                        />
                        <span className="min-w-0 flex-1 truncate" title={f.path}>
                          {f.path}
                        </span>
                        <span className="tabular text-xs text-muted">{fmtBytes(f.size)}</span>
                      </label>
                    </li>
                  ))}
                </ul>
                {!info.files.some((f) => f.video) && <p className="text-sm text-warning">This torrent has no video file SyncWatch can play.</p>}
                <p className="text-xs text-muted">Only the selected file is downloaded.</p>
                {!noticeSaved && (
                  <label className="flex items-start gap-2 text-xs text-warning">
                    <input type="checkbox" checked={noticeOk} onChange={(e) => setNoticeOk(e.target.checked)} />
                    <span>{TORRENT_NOTICE}</span>
                  </label>
                )}
              </div>
            )}
          </div>
        )}

        {error && <p className="mt-3 text-sm text-danger" role="alert">{error}</p>}
        {prepared && (
          <div className="mt-3 rounded-md bg-surface px-3 py-2 text-sm" data-testid="prepared-source">
            <p className="font-medium">{prepared.title}</p>
            <p className="text-xs text-muted">
              {prepared.kind === 'local' ? `${fmtSize(prepared.size)} · identity checked` : `infohash ${prepared.infohash}`}
            </p>
            {prepared.kind === 'magnet' && !prepared.torrentEngineAvailable && (
              <p className="mt-1 text-warning">Torrent engine not available yet. Nothing was sent to any network.</p>
            )}
          </div>
        )}

        <p className="mt-4 text-xs text-warning">Changing the source restarts playback for everyone in the room.</p>
        <div className="mt-4 flex justify-end gap-2">
          <button type="button" className="rounded-md border border-border px-4 py-2 text-sm hover:bg-surface" onClick={props.onClose}>
            Cancel
          </button>
          <button
            type="button"
            disabled={busy || (tab === 'magnet' ? !canUseTorrent : !prepared || prepared.kind !== 'local')}
            className="rounded-md bg-accent px-4 py-2 text-sm font-semibold text-on-accent hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
            onClick={confirm}
          >
            Change source
          </button>
        </div>
      </div>
    </div>
  );
}

import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';
import { parseMagnet } from '../lib/magnet';
import { inTauri } from '../lib/player';

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
}) {
  const [tab, setTab] = useState<'local' | 'magnet'>('local');
  const [path, setPath] = useState<string | null>(props.initialPath ?? null);
  const [magnet, setMagnet] = useState('');
  const [prepared, setPrepared] = useState<Prepared | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const mirror = magnet.trim() ? parseMagnet(magnet) : null;

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
    if (!inTauri() || tab !== 'local') return;
    let un: (() => void) | undefined;
    void import('@tauri-apps/api/webview').then(({ getCurrentWebview }) =>
      getCurrentWebview()
        .onDragDropEvent((ev) => {
          if (ev.payload.type === 'drop' && ev.payload.paths[0]) setPath(ev.payload.paths[0]);
        })
        .then((u) => (un = u)),
    );
    return () => un?.();
  }, [tab]);

  const confirm = () => {
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
              ['magnet', 'Torrent magnet'],
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
            <textarea
              className="h-24 w-full rounded-md border border-border bg-surface px-3 py-2 font-mono text-xs text-text focus:border-accent focus:outline-none"
              placeholder="magnet:?xt=urn:btih:..."
              spellCheck={false}
              value={magnet}
              onChange={(e) => {
                setMagnet(e.target.value);
                setPrepared(null);
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
                <button type="button" disabled={busy} className="mt-2 rounded-md bg-surface px-3 py-1 hover:bg-border" onClick={() => void prepare({ kind: 'magnet', text: magnet })}>
                  Check magnet
                </button>
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
            disabled={!prepared || prepared.kind !== 'local' || busy}
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

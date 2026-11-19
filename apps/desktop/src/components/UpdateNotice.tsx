// A non-modal update card (bottom right of the home screen; never inside a room).
import { useEffect, useState } from 'react';
import { friendlyError } from '../lib/room';
import { IDLE_UPDATE, onUpdateState, progressText, updates, type UpdateState } from '../lib/updates';
import { Icon } from '../ui/icons';

export function UpdateNotice(props: { skipVersion: string; onSkip: (version: string) => void }) {
  const [s, setS] = useState<UpdateState>(IDLE_UPDATE);
  const [later, setLater] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    void updates.state().then(setS).catch(() => {});
    return onUpdateState(setS);
  }, []);
  const run = (p: Promise<unknown>) => void p.catch((e) => setError(friendlyError(e).text));

  const show =
    !later &&
    (s.phase === 'downloading' || s.phase === 'ready' || (s.phase === 'available' && s.version !== null && s.version !== props.skipVersion));
  if (!show) return null;
  const pct = s.total ? Math.min(100, Math.round((s.received / s.total) * 100)) : null;

  return (
    <aside
      role="status"
      aria-label="Update"
      className="fixed right-4 bottom-4 z-40 w-80 space-y-2 rounded-xl border border-border bg-elevated p-4 text-sm shadow-xl"
      data-testid="update-notice"
    >
      <p className="flex items-center gap-2 font-semibold">
        <Icon name="downloading" size="sm" />
        {s.phase === 'ready' ? `Version ${s.version} is ready` : `Version ${s.version} is available`}
      </p>
      {s.notes && s.phase === 'available' && <p className="max-h-24 overflow-auto text-xs whitespace-pre-wrap text-muted">{s.notes}</p>}
      {s.phase === 'available' && (
        <div className="flex flex-wrap gap-2">
          <button type="button" className="rounded-md bg-accent px-3 py-1.5 font-semibold text-on-accent hover:bg-accent-hover" onClick={() => run(updates.download())}>
            Update now
          </button>
          <button type="button" className="rounded-md px-3 py-1.5 text-muted hover:text-text" onClick={() => setLater(true)}>
            Later
          </button>
          <button type="button" className="rounded-md px-3 py-1.5 text-muted hover:text-text" onClick={() => s.version && props.onSkip(s.version)}>
            Skip this version
          </button>
        </div>
      )}
      {s.phase === 'downloading' && (
        <>
          <div className="h-1.5 overflow-hidden rounded bg-surface" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct ?? undefined}>
            <div className="h-full bg-accent" style={{ width: `${pct ?? 5}%` }} />
          </div>
          <div className="flex items-center justify-between text-xs text-muted">
            <span className="tabular">{progressText(s.received, s.total)}</span>
            <button type="button" className="text-text underline" onClick={() => run(updates.cancel())}>
              Cancel
            </button>
          </div>
        </>
      )}
      {s.phase === 'ready' && (
        <>
          <p className="text-xs text-muted">Downloaded and checked. SyncWatch closes, installs the update and starts again.</p>
          <div className="flex gap-2">
            <button type="button" className="rounded-md bg-accent px-3 py-1.5 font-semibold text-on-accent hover:bg-accent-hover" onClick={() => run(updates.install())}>
              Restart to update
            </button>
            <button type="button" className="rounded-md px-3 py-1.5 text-muted hover:text-text" onClick={() => setLater(true)}>
              Later
            </button>
          </div>
        </>
      )}
      {(error || s.error) && <p className="text-xs text-danger">{error ?? s.error}</p>}
    </aside>
  );
}

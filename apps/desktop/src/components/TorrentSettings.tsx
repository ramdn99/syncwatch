import { useEffect, useState } from 'react';
import { inTauri } from '../lib/player';
import { TORRENT_NOTICE, torrents, type TorrentSettings } from '../lib/room';

/** Settings · Torrents (spec section 10): cache, size cap, keep finished files, port/bind, DHT. */
export function TorrentSettingsSection() {
  const [s, setS] = useState<TorrentSettings | null>(null);
  const [msg, setMsg] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri()) return;
    void torrents
      .settings()
      .then(setS)
      .catch(() => {});
  }, []);
  if (!s) return null;
  const save = (next: TorrentSettings) => {
    setS(next);
    void torrents
      .saveSettings(next)
      .then(() => setMsg('Saved.'))
      .catch((e: unknown) => setMsg(String(e)));
  };
  const field = 'w-28 rounded-md border border-border bg-surface px-2 py-1 text-sm';
  return (
    <section className="mt-6 space-y-3 rounded-xl border border-border bg-elevated p-5 text-sm" aria-label="Torrents" data-testid="torrent-settings">
      <h2 className="font-medium">Torrents</h2>
      <p className="text-xs text-warning">{TORRENT_NOTICE}</p>
      <label className="block">
        <span className="text-muted">Cache folder (empty = default)</span>
        <input className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1" value={s.cacheDir ?? ''} onChange={(e) => setS({ ...s, cacheDir: e.target.value || null })} onBlur={() => save(s)} />
      </label>
      <label className="flex items-center gap-2">
        <span className="text-muted">Cache size limit (GB)</span>
        <input type="number" min={1} className={field} value={s.capGb} onChange={(e) => save({ ...s, capGb: Math.max(1, Number(e.target.value) || 1) })} />
      </label>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={s.keepFinished} onChange={(e) => save({ ...s, keepFinished: e.target.checked })} />
        <span>Keep finished files after leaving a room (replayed later without joining the swarm again)</span>
      </label>
      <label className="flex items-center gap-2">
        <span className="text-muted">Incoming port (0 = random)</span>
        <input type="number" min={0} max={65535} className={field} value={s.port} onChange={(e) => save({ ...s, port: Math.min(65535, Math.max(0, Number(e.target.value) || 0)) })} />
      </label>
      <label className="flex items-center gap-2">
        <span className="text-muted">Bind address (empty = all)</span>
        <input className={field} value={s.bind} onChange={(e) => setS({ ...s, bind: e.target.value })} onBlur={() => save(s)} />
      </label>
      <label className="flex items-center gap-2">
        <input type="checkbox" checked={s.dht} onChange={(e) => save({ ...s, dht: e.target.checked })} />
        <span>Use DHT to find peers (only while a torrent is active)</span>
      </label>
      <p className="text-xs text-muted">
        When a file is complete, downloading and sharing stop and the torrent engine shuts down: no connections stay open. Other DHT nodes may still list your address for about 30
        minutes, but nobody can connect because every socket is closed.
      </p>
      {msg && <p className="text-xs text-muted">{msg}</p>}
    </section>
  );
}
